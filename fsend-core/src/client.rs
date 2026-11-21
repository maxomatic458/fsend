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
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::Error;
use crate::iroh::IrohTransfer;
use crate::relay::{self, Answer, Relay};
use crate::transfer::{self, Event, Outgoing, Transport, cancellable};
use crate::webrtc::WebRtcTransfer;

/// For iroh to find a direct path before the connection kind is reported.
const SETTLE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Client {
    pub relay_url: Url,
    /// Shown to the peer, e.g. `fsend-cli 0.4.0`.
    pub app: String,
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
            let mut relay = relay::pair_as_sender(&self.relay_url, &self.app, outgoing, |code| {
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
                relay::pair_as_receiver(&self.relay_url, &self.app, code).await?;
            if !handler.accept(&offer).await {
                relay.reject(None).await?;
                return Err(Error::Declined);
            }
            Ok((connect_as_receiver(&mut relay).await?, offer))
        };
        let (mut transport, offer) = cancellable(&cancel, Box::pin(connecting)).await?;
        settle(transport.as_mut(), &mut |event| handler.event(event)).await;
        transfer::receive_files(transport.as_mut(), &offer, out, handler, cancel).await
    }
}

async fn settle(transport: &mut dyn Transport, on_event: &mut (dyn FnMut(Event<'_>) + Send)) {
    tokio::time::sleep(SETTLE).await;
    on_event(Event::Connected(transport.connection_kind()));
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
    use crate::transfer::test_support::{APP, sample_tree, snapshot, start_relay};

    fn client(relay_url: Url) -> Client {
        Client {
            relay_url,
            app: APP.into(),
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

    /// Sends `outgoing` to a receiver that answers with `accept`; returns
    /// both results, the sender's events and the receiver's handler.
    async fn run(
        outgoing: &Outgoing,
        out: &Path,
        accept: bool,
    ) -> (
        Result<(), Error>,
        Result<(), Error>,
        Vec<&'static str>,
        Handler,
    ) {
        let client = client(start_relay().await);
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
        let client = client(start_relay().await);
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
        let client = client("ws://localhost".parse().unwrap());
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
