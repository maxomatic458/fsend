use serde::{Deserialize, Serialize};

use crate::code::Nameplate;
use crate::peer::limits;

/// The protocol version; the relay pairs only clients that speak it.
pub const VERSION: u32 = 2;
pub const PATH: &str = "/relay";
/// Largest frame a client sends, text or binary.
pub const MAX_FRAME_BYTES: usize = 32 * 1024;
/// Binary bytes a client may send in a session: the largest sealed offer, and room to spare.
pub const MAX_SIGNAL_BYTES: usize = limits::MAX_FRAME_BYTES + 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Iroh,
    Webrtc,
}

/// Control messages are text frames; signals after `paired` are binary frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClientMessage {
    Create {
        version: u32,
        transports: Vec<Transport>,
    },
    Join {
        version: u32,
        nameplate: Nameplate,
        transports: Vec<Transport>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ServerMessage {
    Created {
        nameplate: Nameplate,
        /// Seconds until an unjoined session expires.
        expires_in: u32,
    },
    /// Sent to both sides once the receiver joined.
    Paired {
        transport: Transport,
        /// The receiver's one-time token for its stats report, if the server keeps stats.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        stats_token: Option<String>,
    },
    Error {
        code: Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "RelayError"))]
#[serde(rename_all = "snake_case")]
pub enum Error {
    #[error("the relay speaks another protocol version; update fsend")]
    Version,
    #[error("no session with this code")]
    NotFound,
    #[error("the code expired")]
    Expired,
    #[error("the two sides have no connection type in common; update fsend on both sides")]
    NoCommonTransport,
    #[error("the other side left")]
    PeerLeft,
    #[error("the relay is busy; try again later")]
    Busy,
    #[error("the relay refused a message")]
    Protocol,
}
