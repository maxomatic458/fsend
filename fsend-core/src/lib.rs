mod checksum;
pub mod connect;
pub mod iroh;
pub mod relay;
pub mod transfer;
pub mod webrtc;

pub use fsend_proto as proto;
pub use tokio_util::sync::CancellationToken;

use relay::RelayError;
use thiserror::Error;
use transfer::TransferError;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Relay(#[from] RelayError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
}

impl Error {
    /// A message for the errors a user can act on.
    pub fn user_message(&self) -> Option<String> {
        match self {
            Self::Relay(e) => e.user_message(),
            Self::Transfer(e) => e.user_message(),
        }
    }
}
