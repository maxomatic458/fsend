use fsend_proto::pairing::ConnectionInfo;
use fsend_proto::relay::Transport as Kind;

use crate::Error;
use crate::iroh::IrohTransfer;
use crate::relay::Relay;
use crate::transfer::{TransferError, Transport};
use crate::webrtc::WebRtcTransfer;

/// Swaps connection info over the paired `relay` and connects to the receiver.
pub async fn connect_as_sender(relay: &mut Relay) -> Result<Box<dyn Transport>, Error> {
    match relay.transport {
        Kind::Iroh => {
            let endpoint = IrohTransfer::create_endpoint().await?;
            let my_info = IrohTransfer::local_connection_info(&endpoint).await;

            relay.send_connection(my_info).await?;
            let peer_info = relay.connection().await?;

            Ok(Box::new(IrohTransfer::accept(endpoint, &peer_info).await?))
        }
        Kind::Webrtc => {
            let (mut wrt, offer_sdp) = WebRtcTransfer::create_offerer().await?;
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

/// Swaps connection info over the paired `relay` and connects to the sender.
pub async fn connect_as_receiver(relay: &mut Relay) -> Result<Box<dyn Transport>, Error> {
    match relay.transport {
        Kind::Iroh => {
            let endpoint = IrohTransfer::create_endpoint().await?;
            // Finding our own address can take a while; the sender's arrives meanwhile.
            let (my_info, peer_info) = tokio::try_join!(
                async { Ok(IrohTransfer::local_connection_info(&endpoint).await) },
                relay.connection()
            )?;
            relay.send_connection(my_info).await?;

            Ok(Box::new(IrohTransfer::connect(endpoint, &peer_info).await?))
        }
        Kind::Webrtc => {
            let ConnectionInfo::WebRtc { sdp: offer_sdp } = relay.connection().await? else {
                return Err(TransferError::UnexpectedConnectionInfo.into());
            };

            let (mut wrt, answer_sdp) = WebRtcTransfer::create_answerer(&offer_sdp).await?;
            let my_info = ConnectionInfo::WebRtc { sdp: answer_sdp };
            relay.send_connection(my_info).await?;

            wrt.wait_connected().await?;
            Ok(Box::new(wrt))
        }
    }
}
