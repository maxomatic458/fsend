mod checksum;
mod client;
mod iroh;
mod relay;
mod transfer;
mod webrtc;

use std::io;
use std::path::PathBuf;

use fsend_proto::pairing::PairingError;
use fsend_proto::peer::{ErrorReport, offer::OfferError};
use thiserror::Error;

pub use async_trait::async_trait;
pub use client::{Client, ReceiveHandler};
pub use fsend_proto as proto;
pub use tokio_util::sync::CancellationToken;
pub use transfer::{ConnectionKind, Event, Outgoing, build_offer};

#[derive(Debug, Error)]
pub enum Error {
    #[error("not found:{}", list(.0))]
    PathsNotFound(Vec<PathBuf>),
    #[error("cannot send these files: {0}")]
    Unsendable(OfferError),
    #[error("could not reach the relay: {0}")]
    RelayUnreachable(String),
    #[error("lost the connection to the relay: {0}")]
    RelayLost(String),
    #[error("{0}")]
    Relay(#[from] fsend_proto::relay::Error),
    #[error("pairing failed: {0}")]
    Pairing(#[from] PairingError),
    #[error("the sender offered invalid files: {0}")]
    InvalidOffer(OfferError),
    #[error("transfer declined")]
    Declined,
    #[error("the peer declined the files{}", reason(.0))]
    PeerDeclined(Option<String>),
    #[error("could not connect to the peer: {0}")]
    Connect(String),
    #[error("lost the connection to the peer ({0}); a new session resumes where it stopped")]
    Disconnected(String),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("transfer cancelled")]
    Cancelled,
    #[error("the peer cancelled the transfer{}", reason(.0))]
    PeerCancelled(Option<String>),
    #[error("the peer failed: {0}")]
    Peer(ErrorReport),
    /// The peer is not told.
    #[error("{} does not match its checksum; delete it to receive it again", .0.display())]
    ChecksumMismatch(PathBuf),
    #[error("the peer broke the protocol: {0}")]
    Protocol(String),
    #[error("{0}")]
    Io(io::Error),
    #[error("internal error: {0}")]
    Internal(&'static str),
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| format!("\n - {}", path.display()))
        .collect()
}

fn reason(reason: &Option<String>) -> String {
    reason
        .as_ref()
        .map(|r| format!(": {r}"))
        .unwrap_or_default()
}

/// How the transports report a lost peer.
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        use io::ErrorKind::*;
        match e.kind() {
            ConnectionAborted | ConnectionReset | NotConnected | BrokenPipe => {
                Self::Disconnected(e.to_string())
            }
            _ => Self::Io(e),
        }
    }
}
