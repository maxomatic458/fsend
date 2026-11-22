use std::future::Future;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use fsend_proto::code::Code;
use fsend_proto::pairing::ConnectionInfo;
use fsend_proto::peer::Entry;
use fsend_proto::peer::Offer;
use fsend_proto::peer::resume::{Conflict, ConflictPolicy};
use fsend_proto::relay::Transport as Kind;
use fsend_proto::stats::{self, Platform, Report, Route};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::Error;
use crate::iroh::IrohTransfer;
use crate::relay::{self, Answer, Relay, endpoint};
use crate::transfer::{self, ConnectionKind, Event, Outgoing, Transport, cancellable};
use crate::webrtc::WebRtcTransfer;

/// For iroh to find a direct path before the connection kind is reported.
const SETTLE: Duration = Duration::from_secs(2);
/// For the stats report, which must not hold up the end of a transfer for long.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct Client {
    /// The fsend server, which pairs the peers and counts the bytes they move.
    pub server_url: Url,
    /// Shown to the peer, e.g. `fsend-cli 0.4.0`.
    pub app: String,
    /// Reported with the bytes received.
    pub platform: Platform,
    /// Whether a receiver tells the server how many bytes arrived.
    pub telemetry: bool,
}

#[async_trait]
pub trait ReceiveHandler: Send {
    /// Nothing about this side reaches the sender before it accepts.
    async fn accept(&mut self, offer: &Offer) -> bool;
    async fn resolve(&mut self, entries: &[Entry], conflicts: &[Conflict]) -> ConflictPolicy;
    fn event(&mut self, event: Event<'_>);
}

impl Client {
    pub async fn send(
        &self,
        outgoing: &Outgoing,
        compress: bool,
        on_event: &mut (dyn FnMut(Event<'_>) + Send),
        cancel: CancellationToken,
    ) -> Result<(), Error> {
        let connecting = async {
            let mut relay = relay::pair_as_sender(&self.server_url, &self.app, outgoing, |code| {
                on_event(Event::Code(code))
            })
            .await?;
            on_event(Event::Paired);
            connect_as_sender(&mut relay).await
        };
        // Boxed: the transports make large futures.
        let mut transport = cancellable(&cancel, Box::pin(connecting)).await?;
        settle(transport.as_mut(), on_event).await;
        transfer::send_files(transport.as_mut(), outgoing, compress, on_event, cancel).await
    }

    pub async fn receive(
        &self,
        code: &Code,
        out: &Path,
        handler: &mut dyn ReceiveHandler,
        cancel: CancellationToken,
    ) -> Result<(), Error> {
        let connecting = async {
            let (mut relay, offer) =
                relay::pair_as_receiver(&self.server_url, &self.app, code).await?;
            if !handler.accept(&offer).await {
                relay.reject(None).await?;
                return Err(Error::Declined);
            }
            let transport = connect_as_receiver(&mut relay).await?;
            Ok((transport, offer, relay.transport, relay.stats_token))
        };
        let (mut transport, offer, kind, stats_token) =
            cancellable(&cancel, Box::pin(connecting)).await?;
        let route = settle(transport.as_mut(), &mut |event| handler.event(event)).await;
        let mut counting = Counting { handler, bytes: 0 };
        let result =
            transfer::receive_files(transport.as_mut(), &offer, out, &mut counting, cancel).await;
        if let Some(token) = stats_token
            && self.telemetry
            && counting.bytes > 0
        {
            self.report(Report {
                token,
                platform: self.platform,
                transport: kind,
                route: route.into(),
                bytes: counting.bytes,
            })
            .await;
        }
        result
    }

    /// Tells the server how many bytes arrived and how, and nothing else; failures are ignored.
    async fn report(&self, report: Report) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let sent = reqwest::Client::new()
            .post(endpoint(&self.server_url, stats::PATH))
            .json(&report)
            .timeout(REPORT_TIMEOUT)
            .send()
            .await;
        if let Err(error) = sent {
            tracing::debug!("the stats report failed: {error}");
        }
    }
}

/// Counts the file bytes received on their way to `handler`.
struct Counting<'a> {
    handler: &'a mut dyn ReceiveHandler,
    bytes: u64,
}

#[async_trait]
impl ReceiveHandler for Counting<'_> {
    async fn accept(&mut self, offer: &Offer) -> bool {
        self.handler.accept(offer).await
    }

    async fn resolve(&mut self, entries: &[Entry], conflicts: &[Conflict]) -> ConflictPolicy {
        self.handler.resolve(entries, conflicts).await
    }

    fn event(&mut self, event: Event<'_>) {
        if let Event::Progress { bytes, .. } = event {
            self.bytes += bytes;
        }
        self.handler.event(event);
    }
}

async fn settle(
    transport: &mut dyn Transport,
    on_event: &mut (dyn FnMut(Event<'_>) + Send),
) -> ConnectionKind {
    tokio::time::sleep(SETTLE).await;
    let kind = transport.connection_kind().await;
    on_event(Event::Connected(kind));
    kind
}

impl From<ConnectionKind> for Route {
    fn from(kind: ConnectionKind) -> Self {
        match kind {
            ConnectionKind::Direct => Self::Direct,
            ConnectionKind::Relay => Self::Relay,
            ConnectionKind::Unknown => Self::Unknown,
        }
    }
}

async fn connect_as_sender(relay: &mut Relay) -> Result<Box<dyn Transport>, Error> {
    match relay.transport {
        Kind::Iroh => {
            let local = async {
                let endpoint = IrohTransfer::create_endpoint().await?;
                let info = IrohTransfer::local_connection_info(&endpoint).await;
                Ok((endpoint, info))
            };
            let (endpoint, info) = once_accepted(relay, local).await?;
            relay.send_connection(info).await?;
            let peer = relay.connection().await?;
            Ok(Box::new(IrohTransfer::accept(endpoint, &peer).await?))
        }
        Kind::Webrtc => {
            let (mut webrtc, sdp) = once_accepted(relay, WebRtcTransfer::create_offerer()).await?;
            relay
                .send_connection(ConnectionInfo::WebRtc { sdp })
                .await?;
            webrtc.set_answer(&peer_sdp(relay).await?).await?;
            webrtc.wait_connected().await?;
            Ok(Box::new(webrtc))
        }
    }
}

/// Runs `prepare` while the receiver decides; none of it is sent before it accepts.
async fn once_accepted<T>(
    relay: &mut Relay,
    prepare: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    let mut prepare = std::pin::pin!(prepare);
    let mut prepared = None;
    let answer = loop {
        tokio::select! {
            answer = relay.answer() => break answer?,
            ready = &mut prepare, if prepared.is_none() => prepared = Some(ready?),
        }
    };
    if let Answer::Rejected(reason) = answer {
        return Err(Error::PeerDeclined(reason));
    }
    match prepared {
        Some(ready) => Ok(ready),
        None => prepare.await,
    }
}

async fn connect_as_receiver(relay: &mut Relay) -> Result<Box<dyn Transport>, Error> {
    relay.accept().await?;
    match relay.transport {
        Kind::Iroh => {
            let endpoint = IrohTransfer::create_endpoint().await?;
            // Finding our address can take a while.
            let (info, peer) = tokio::try_join!(
                async { Ok(IrohTransfer::local_connection_info(&endpoint).await) },
                relay.connection()
            )?;
            relay.send_connection(info).await?;
            Ok(Box::new(IrohTransfer::connect(endpoint, &peer).await?))
        }
        Kind::Webrtc => {
            let (mut webrtc, sdp) =
                WebRtcTransfer::create_answerer(&peer_sdp(relay).await?).await?;
            relay
                .send_connection(ConnectionInfo::WebRtc { sdp })
                .await?;
            webrtc.wait_connected().await?;
            Ok(Box::new(webrtc))
        }
    }
}

async fn peer_sdp(relay: &mut Relay) -> Result<String, Error> {
    match relay.connection().await? {
        ConnectionInfo::WebRtc { sdp } => Ok(sdp),
        _ => Err(Error::Protocol(
            "connection info for another transport".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;

    use super::*;
    use crate::build_offer;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

    use crate::transfer::test_support::{
        APP, sample_tree, snapshot, start_server, start_server_with,
    };

    fn client(server_url: Url) -> Client {
        Client {
            server_url,
            app: APP.into(),
            platform: Platform::Cli,
            telemetry: true,
        }
    }

    /// Consecutive progress events count once.
    fn record(events: &mut Vec<&'static str>, event: Event<'_>) {
        let name = match event {
            Event::Code(_) => "code",
            Event::Paired => "paired",
            Event::Connected(_) => "connected",
            Event::Verifying { .. } => "verifying",
            Event::Started(_) => "started",
            Event::Progress { .. } => "progress",
        };
        if events.last() != Some(&name) {
            events.push(name);
        }
    }

    struct Handler {
        accept: bool,
        offered: Option<Offer>,
        events: Vec<&'static str>,
    }

    #[async_trait]
    impl ReceiveHandler for Handler {
        async fn accept(&mut self, offer: &Offer) -> bool {
            self.offered = Some(offer.clone());
            self.accept
        }

        async fn resolve(&mut self, _: &[Entry], _: &[Conflict]) -> ConflictPolicy {
            ConflictPolicy::Skip
        }

        fn event(&mut self, event: Event<'_>) {
            record(&mut self.events, event);
        }
    }

    type Run = (
        Result<(), Error>,
        Result<(), Error>,
        Vec<&'static str>,
        Handler,
    );

    async fn run(outgoing: &Outgoing, out: &Path, accept: bool) -> Run {
        run_on(client(start_server().await), outgoing, out, accept).await
    }

    /// Sends `outgoing` with `client` to a receiver that answers with
    /// `accept`; returns both results, the sender's events and the receiver's handler.
    async fn run_on(client: Client, outgoing: &Outgoing, out: &Path, accept: bool) -> Run {
        let (code_tx, code_rx) = oneshot::channel();
        let mut code_tx = Some(code_tx);
        let mut sent = Vec::new();
        let mut on_event = |event: Event<'_>| {
            if let Event::Code(code) = event {
                code_tx.take().unwrap().send(code.clone()).unwrap();
            }
            record(&mut sent, event);
        };
        let mut handler = Handler {
            accept,
            offered: None,
            events: Vec::new(),
        };
        let send = client.send(outgoing, true, &mut on_event, CancellationToken::new());
        let receive = async {
            let code = code_rx.await.unwrap();
            (client.receive(&code, out, &mut handler, CancellationToken::new())).await
        };
        let (send, receive) = tokio::time::timeout(Duration::from_secs(120), async {
            tokio::join!(send, receive)
        })
        .await
        .expect("the transfer hung");
        (send, receive, sent, handler)
    }

    #[tokio::test]
    async fn sends_through_the_relay_to_an_accepting_receiver() {
        let tmp = tempfile::tempdir().unwrap();
        let outgoing = build_offer(&[sample_tree(tmp.path())]).unwrap();
        let out = tmp.path().join("out");

        let (send, receive, sent, handler) = run(&outgoing, &out, true).await;

        send.unwrap();
        receive.unwrap();
        assert_eq!(
            snapshot(&out.join("album")),
            snapshot(&tmp.path().join("album"))
        );
        assert_eq!(handler.offered.as_ref(), Some(outgoing.offer()));
        assert_eq!(sent, ["code", "paired", "connected", "started", "progress"]);
        assert_eq!(handler.events, ["connected", "started", "progress"]);
    }

    /// The `(platform, transport, route, bytes)` rows a transfer with
    /// `telemetry` leaves, and the size sent.
    async fn reported(telemetry: bool) -> (Vec<(String, String, String, i64)>, usize) {
        let tmp = tempfile::tempdir().unwrap();
        let database = tmp.path().join("stats.db");
        let server = start_server_with(fsend_server::Config {
            database: Some(database.clone()),
            ..fsend_server::Config::default()
        })
        .await;
        let client = Client {
            telemetry,
            ..client(server)
        };
        let outgoing = build_offer(&[sample_tree(tmp.path())]).unwrap();

        let (send, receive, _, _) = run_on(client, &outgoing, &tmp.path().join("out"), true).await;

        send.unwrap();
        receive.unwrap();
        let size = (snapshot(&tmp.path().join("album")).iter())
            .map(|(_, bytes)| bytes.len())
            .sum();
        let options = SqliteConnectOptions::new().filename(&database);
        let db = SqlitePool::connect_with(options).await.unwrap();
        let rows = sqlx::query_as("SELECT platform, transport, route, bytes FROM transfers")
            .fetch_all(&db)
            .await
            .unwrap();
        (rows, size)
    }

    #[tokio::test]
    async fn the_receiver_reports_the_bytes_it_received() {
        let (rows, size) = reported(true).await;
        // Two local endpoints find each other directly.
        let row = ("cli".into(), "iroh".into(), "direct".into(), size as i64);
        assert_eq!(rows, [row]);
    }

    #[tokio::test]
    async fn without_telemetry_the_receiver_reports_nothing() {
        let (rows, _) = reported(false).await;
        assert_eq!(rows, []);
    }

    #[tokio::test]
    async fn a_declined_offer_ends_both_sides_before_they_connect() {
        let tmp = tempfile::tempdir().unwrap();
        let outgoing = build_offer(&[sample_tree(tmp.path())]).unwrap();
        let out = tmp.path().join("out");

        let (send, receive, sent, handler) = run(&outgoing, &out, false).await;

        assert!(matches!(send, Err(Error::PeerDeclined(None))), "{send:?}");
        assert!(matches!(receive, Err(Error::Declined)), "{receive:?}");
        assert_eq!(handler.offered.as_ref(), Some(outgoing.offer()));
        assert_eq!(sent, ["code", "paired"]);
        assert!(handler.events.is_empty(), "{:?}", handler.events);
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn a_cancelled_sender_stops_waiting_for_the_receiver() {
        let tmp = tempfile::tempdir().unwrap();
        let outgoing = build_offer(&[sample_tree(tmp.path())]).unwrap();
        let client = client(start_server().await);
        let cancel = CancellationToken::new();
        let mut on_event = |event: Event<'_>| {
            if let Event::Code(_) = event {
                cancel.cancel();
            }
        };

        let send = client.send(&outgoing, true, &mut on_event, cancel.clone());
        let result = tokio::time::timeout(Duration::from_secs(10), send).await;

        assert!(matches!(result, Ok(Err(Error::Cancelled))), "{result:?}");
    }

    /// Small enough to hold many, or to await on a small stack.
    #[test]
    fn transfers_are_small_futures() {
        let client = client("http://localhost".parse().unwrap());
        let outgoing = Outgoing::default();
        let mut on_event = |_: Event<'_>| {};
        let send = client.send(&outgoing, true, &mut on_event, CancellationToken::new());
        assert!(std::mem::size_of_val(&send) < 16 * 1024);
        let mut handler = Handler {
            accept: true,
            offered: None,
            events: vec![],
        };
        let code = "ZZZZ000000".parse().unwrap();
        let cancel = CancellationToken::new();
        let receive = client.receive(&code, Path::new("out"), &mut handler, cancel);
        assert!(std::mem::size_of_val(&receive) < 16 * 1024);
    }

    /// A GUI runs each transfer on a task of its own.
    #[test]
    fn transfers_can_be_spawned() {
        fn spawnable<A, B, F: Future + Send + 'static>(_: fn(A, B) -> F) {}
        async fn send(client: Client, outgoing: Outgoing) {
            let cancel = CancellationToken::new();
            let _ = client.send(&outgoing, true, &mut |_| {}, cancel).await;
        }
        async fn receive(client: Client, mut handler: Box<dyn ReceiveHandler>) {
            let code = "ZZZZ000000".parse().unwrap();
            let out = Path::new("out");
            let cancel = CancellationToken::new();
            let _ = client.receive(&code, out, handler.as_mut(), cancel).await;
        }
        spawnable(send);
        spawnable(receive);
    }
}
