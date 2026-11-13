use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const PATH: &str = "/relay";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Iroh,
    Webrtc,
}

/// Control messages are text frames; signals after `paired` are binary frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClientMessage {
    Create {
        version: u32,
        transports: Vec<Transport>,
    },
    Join {
        version: u32,
        nameplate: String,
        transports: Vec<Transport>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ServerMessage {
    Created {
        nameplate: String,
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
    #[serde(other)]
    Unknown,
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
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::pinned;

    #[test]
    fn client_messages_keep_their_wire_format() {
        pinned(
            ClientMessage::Create {
                version: 1,
                transports: vec![Transport::Iroh, Transport::Webrtc],
            },
            r#"{"type":"create","version":1,"transports":["iroh","webrtc"]}"#,
        );
        pinned(
            ClientMessage::Join {
                version: 1,
                nameplate: "K7Q2".into(),
                transports: vec![Transport::Webrtc],
            },
            r#"{"type":"join","version":1,"nameplate":"K7Q2","transports":["webrtc"]}"#,
        );
    }

    #[test]
    fn server_messages_keep_their_wire_format() {
        pinned(
            ServerMessage::Created {
                nameplate: "K7Q2".into(),
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
    fn unknown_message_types_codes_and_fields_are_tolerated() {
        let parse = |json| serde_json::from_str::<ServerMessage>(json).unwrap();
        assert_eq!(
            parse(r#"{"type":"motd","text":"hi"}"#),
            ServerMessage::Unknown
        );
        assert_eq!(
            parse(r#"{"type":"paired","transport":"webrtc","extra":1}"#),
            ServerMessage::Paired {
                transport: Transport::Webrtc
            }
        );
        assert_eq!(
            parse(r#"{"type":"error","code":"teapot","message":""}"#),
            ServerMessage::Error {
                code: ErrorCode::Unknown,
                message: String::new()
            }
        );
    }

    #[test]
    fn unknown_transports_are_rejected() {
        for json in [
            r#"{"type":"create","version":1,"transports":["iroh","quic"]}"#,
            r#"{"type":"join","version":1,"nameplate":"K7Q2","transports":["quic"]}"#,
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
