use std::time::Duration;

use fsend_proto::code::Code;
use fsend_proto::pairing::ConnectionInfo;
use fsend_proto::relay::Transport;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::iroh::IrohTransfer;
use crate::relay::{Relay, RelayError, TRANSPORTS, pair_as_receiver, pair_as_sender};
use crate::transfer::{
    self, Event, EventCb, Outgoing, ReceiveHandler, TransferError, receive_files, send_files,
};
use crate::webrtc::{WebRtcConfig, WebRtcTransfer};

/// How long a new connection gets before its kind is reported: iroh starts relayed and moves to a direct path.
const SETTLE: Duration = Duration::from_secs(2);

/// Why a session failed.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Relay(#[from] RelayError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
}

impl Error {
    /// See [`TransferError::user_error`].
    pub fn user_error(&self) -> Option<String> {
        match self {
            Self::Relay(e) => e.user_error(),
            Self::Transfer(e) => e.user_error(),
        }
    }

    /// See [`TransferError::is_deliberate`].
    pub fn is_deliberate(&self) -> bool {
        matches!(self, Self::Transfer(e) if e.is_deliberate())
    }
}

/// Pairs through a relay, connects to the peer and transfers.
#[derive(Debug, Clone)]
pub struct Client {
    /// The relay that introduces the peers; `/relay` is appended.
    pub relay_url: Url,
    /// Who this side is to the peer, e.g. `fsend-cli 0.4.0`.
    pub app: String,
    /// The transports to offer, in order of preference.
    pub transports: Vec<Transport>,
    pub webrtc: WebRtcConfig,
}

impl Client {
    /// A client of the relay at `relay_url` that speaks every transport.
    pub fn new(relay_url: Url, app: impl Into<String>) -> Self {
        Self {
            relay_url,
            app: app.into(),
            transports: TRANSPORTS.to_vec(),
            webrtc: WebRtcConfig::default(),
        }
    }

    /// Sends `outgoing` to whoever joins with the code `on_code` gets; see [`send_files`].
    pub async fn send(
        &self,
        outgoing: &Outgoing,
        compress: bool,
        on_code: impl FnOnce(&Code) + Send,
        on_event: EventCb<'_>,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let pairing = pair_as_sender(&self.relay_url, &self.app, &self.transports, on_code);
        let mut relay = until_cancelled(cancel, pairing).await??;
        let mut transport = until_cancelled(cancel, self.connect_as_sender(&mut relay)).await??;
        drop(relay);
        settle(cancel).await;
        on_event(Event::Connected(transport.connection_kind()));
        Ok(send_files(transport.as_mut(), outgoing, compress, on_event, cancel).await?)
    }

    /// Receives from the sender of `code`; see [`receive_files`].
    pub async fn receive(
        &self,
        code: &Code,
        handler: &mut dyn ReceiveHandler,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let pairing = pair_as_receiver(&self.relay_url, &self.app, &self.transports, code);
        let mut relay = until_cancelled(cancel, pairing).await??;
        let mut transport = until_cancelled(cancel, self.connect_as_receiver(&mut relay)).await??;
        drop(relay);
        settle(cancel).await;
        handler.event(Event::Connected(transport.connection_kind()));
        Ok(receive_files(transport.as_mut(), handler, cancel).await?)
    }

    async fn connect_as_sender(
        &self,
        relay: &mut Relay,
    ) -> Result<Box<dyn transfer::Transport>, Error> {
        match relay.transport {
            Transport::Iroh => {
                let endpoint = IrohTransfer::create_endpoint().await?;
                let my_info = IrohTransfer::local_connection_info(&endpoint).await;

                relay.send_connection(my_info).await?;
                let peer_info = relay.connection().await?;

                Ok(Box::new(IrohTransfer::accept(endpoint, &peer_info).await?))
            }
            Transport::Webrtc => {
                let (mut wrt, offer_sdp) =
                    WebRtcTransfer::create_offerer_with(&self.webrtc).await?;
                let my_info = ConnectionInfo::WebRtc { sdp: offer_sdp };

                relay.send_connection(my_info).await?;
                let ConnectionInfo::WebRtc { sdp: answer_sdp } = relay.connection().await? else {
                    return Err(TransferError::UnexpectedConnectionInfo.into());
                };
                wrt.set_answer(&answer_sdp).await?;
                wrt.wait_connected().await?;
                Ok(Box::new(wrt))
            }
        }
    }

    async fn connect_as_receiver(
        &self,
        relay: &mut Relay,
    ) -> Result<Box<dyn transfer::Transport>, Error> {
        match relay.transport {
            Transport::Iroh => {
                let endpoint = IrohTransfer::create_endpoint().await?;
                // Finding our own address can take a while; the sender's arrives meanwhile.
                let (my_info, peer_info) = tokio::try_join!(
                    async { Ok(IrohTransfer::local_connection_info(&endpoint).await) },
                    relay.connection()
                )?;
                relay.send_connection(my_info).await?;

                Ok(Box::new(IrohTransfer::connect(endpoint, &peer_info).await?))
            }
            Transport::Webrtc => {
                let ConnectionInfo::WebRtc { sdp: offer_sdp } = relay.connection().await? else {
                    return Err(TransferError::UnexpectedConnectionInfo.into());
                };

                let (mut wrt, answer_sdp) =
                    WebRtcTransfer::create_answerer_with(&offer_sdp, &self.webrtc).await?;
                let my_info = ConnectionInfo::WebRtc { sdp: answer_sdp };
                relay.send_connection(my_info).await?;

                wrt.wait_connected().await?;
                Ok(Box::new(wrt))
            }
        }
    }
}

/// `work`, unless `cancel` comes first.
async fn until_cancelled<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = T>,
) -> Result<T, TransferError> {
    cancel
        .run_until_cancelled(work)
        .await
        .ok_or(TransferError::Cancelled)
}

/// Gives a new connection a moment to settle before its kind is reported.
async fn settle(cancel: &CancellationToken) {
    cancel.run_until_cancelled(tokio::time::sleep(SETTLE)).await;
}

#[cfg(test)]
#[path = "tests/session.rs"]
mod tests;
