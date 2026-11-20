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
        let mut transport = cancellable(&cancel, connecting).await?;
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
        let (mut transport, offer) = cancellable(&cancel, connecting).await?;
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
            let ConnectionInfo::WebRtc { sdp } = relay.connection().await? else {
                return Err(Error::Protocol(
                    "connection info for another transport".into(),
                ));
            };
            webrtc.set_answer(&sdp).await?;
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
            let ConnectionInfo::WebRtc { sdp } = relay.connection().await? else {
                return Err(Error::Protocol(
                    "connection info for another transport".into(),
                ));
            };
            let (mut webrtc, sdp) = WebRtcTransfer::create_answerer(&sdp).await?;
            relay
                .send_connection(ConnectionInfo::WebRtc { sdp })
                .await?;
            webrtc.wait_connected().await?;
            Ok(Box::new(webrtc))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
