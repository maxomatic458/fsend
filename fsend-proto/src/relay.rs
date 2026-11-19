use serde::{Deserialize, Serialize};

use crate::code::Nameplate;
use crate::peer::limits;

pub const VERSION: u32 = 2;
pub const PATH: &str = "/relay";
/// Largest frame a client sends, text or binary.
pub const MAX_FRAME_BYTES: usize = 32 * 1024;
/// Binary frame bytes a client may send in a session: the largest sealed
/// offer, with room for its frame headers and the other signals.
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
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Version,
    NotFound,
    Expired,
    NoCommonTransport,
    PeerLeft,
    Busy,
    Protocol,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::pinned;

    #[test]
    fn client_messages_keep_their_wire_format() {
        pinned(
            ClientMessage::Create {
                version: 2,
                transports: vec![Transport::Iroh, Transport::Webrtc],
            },
            r#"{"type":"create","version":2,"transports":["iroh","webrtc"]}"#,
        );
        pinned(
            ClientMessage::Join {
                version: 2,
                nameplate: "K7Q2".parse().unwrap(),
                transports: vec![Transport::Webrtc],
            },
            r#"{"type":"join","version":2,"nameplate":"K7Q2","transports":["webrtc"]}"#,
        );
    }

    #[test]
    fn server_messages_keep_their_wire_format() {
        pinned(
            ServerMessage::Created {
                nameplate: "K7Q2".parse().unwrap(),
                expires_in: 300,
            },
            r#"{"type":"created","nameplate":"K7Q2","expires_in":300}"#,
        );
        pinned(
            ServerMessage::Paired {
                transport: Transport::Iroh,
            },
            r#"{"type":"paired","transport":"iroh"}"#,
        );
        pinned(
            ServerMessage::Error {
                code: ErrorCode::NoCommonTransport,
                message: "m".into(),
            },
            r#"{"type":"error","code":"no_common_transport","message":"m"}"#,
        );
    }

    #[test]
    fn unknown_types_and_codes_fail_to_parse() {
        for json in [
            r#"{"type":"motd","text":"hi"}"#,
            r#"{"type":"error","code":"teapot","message":""}"#,
            r#"{"type":"create","version":2,"transports":[]}"#,
        ] {
            assert!(
                serde_json::from_str::<ServerMessage>(json).is_err(),
                "{json}"
            );
        }
        assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"created"}"#).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        assert_eq!(
            serde_json::from_str::<ServerMessage>(
                r#"{"type":"paired","transport":"webrtc","extra":1}"#
            )
            .unwrap(),
            ServerMessage::Paired {
                transport: Transport::Webrtc
            }
        );
    }

    #[test]
    fn unknown_transports_are_rejected() {
        for json in [
            r#"{"type":"create","version":2,"transports":["iroh","quic"]}"#,
            r#"{"type":"join","version":2,"nameplate":"K7Q2","transports":["quic"]}"#,
        ] {
            assert!(
                serde_json::from_str::<ClientMessage>(json).is_err(),
                "{json}"
            );
        }
        assert!(
            serde_json::from_str::<ServerMessage>(r#"{"type":"paired","transport":"quic"}"#)
                .is_err()
        );
    }
}
