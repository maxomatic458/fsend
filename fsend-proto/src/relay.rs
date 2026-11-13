use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    WebRtc,
    Iroh,
}

/// How to reach a peer, forwarded by the relay as is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ConnectionInfo {
    WebRtc {
        sdp: String,
        ice_candidates: Vec<String>,
    },
    Iroh {
        node_id: String,
        addrs: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub capabilities: Vec<Protocol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSessionRequest {
    pub code: String,
    pub capabilities: Vec<Protocol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRequest {
    pub connection_info: ConnectionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClientMessage {
    CreateSession(CreateSessionRequest),
    JoinSession(JoinSessionRequest),
    Exchange(ExchangeRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionAnswer {
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSessionAnswer {
    pub protocol: Protocol,
}

/// Sent to the sender when a peer joins their session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerJoinedAnswer {
    pub protocol: Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeAnswer {
    pub connection_info: ConnectionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorAnswer {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ServerMessage {
    CreateSession(CreateSessionAnswer),
    JoinSession(JoinSessionAnswer),
    PeerJoined(PeerJoinedAnswer),
    Exchange(ExchangeAnswer),
    Error(ErrorAnswer),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn webrtc() -> ConnectionInfo {
        ConnectionInfo::WebRtc {
            sdp: "v=0".into(),
            ice_candidates: vec!["c".into()],
        }
    }

    /// Each message serializes to exactly this JSON and parses back.
    fn pinned<T>(message: T, json: &str)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        assert_eq!(serde_json::to_string(&message).unwrap(), json);
        assert_eq!(serde_json::from_str::<T>(json).unwrap(), message);
    }

    #[test]
    fn client_messages_keep_their_wire_format() {
        pinned(
            ClientMessage::CreateSession(CreateSessionRequest {
                capabilities: vec![Protocol::Iroh, Protocol::WebRtc],
            }),
            r#"{"type":"create_session","capabilities":["iroh","web_rtc"]}"#,
        );
        pinned(
            ClientMessage::JoinSession(JoinSessionRequest {
                code: "AB12CD34".into(),
                capabilities: vec![Protocol::WebRtc],
            }),
            r#"{"type":"join_session","code":"AB12CD34","capabilities":["web_rtc"]}"#,
        );
        pinned(
            ClientMessage::Exchange(ExchangeRequest {
                connection_info: webrtc(),
            }),
            r#"{"type":"exchange","connection_info":{"type":"web_rtc","sdp":"v=0","ice_candidates":["c"]}}"#,
        );
    }

    #[test]
    fn server_messages_keep_their_wire_format() {
        pinned(
            ServerMessage::CreateSession(CreateSessionAnswer {
                code: "AB12CD34".into(),
            }),
            r#"{"type":"create_session","code":"AB12CD34"}"#,
        );
        pinned(
            ServerMessage::JoinSession(JoinSessionAnswer {
                protocol: Protocol::Iroh,
            }),
            r#"{"type":"join_session","protocol":"iroh"}"#,
        );
        pinned(
            ServerMessage::PeerJoined(PeerJoinedAnswer {
                protocol: Protocol::WebRtc,
            }),
            r#"{"type":"peer_joined","protocol":"web_rtc"}"#,
        );
        pinned(
            ServerMessage::Exchange(ExchangeAnswer {
                connection_info: ConnectionInfo::Iroh {
                    node_id: "n".into(),
                    addrs: vec!["10.0.0.1:1234".into()],
                },
            }),
            r#"{"type":"exchange","connection_info":{"type":"iroh","node_id":"n","addrs":["10.0.0.1:1234"]}}"#,
        );
        pinned(
            ServerMessage::Error(ErrorAnswer {
                message: "session not found".into(),
            }),
            r#"{"type":"error","message":"session not found"}"#,
        );
    }
}
