use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    WebRtc,
    Iroh,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateSessionRequest {
    capabilities: Vec<Protocol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JoinSessionRequest {
    code: String,
    capabilities: Vec<Protocol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExchangeRequest {
    connection_info: ConnectionInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum ClientMessage {
    CreateSession(CreateSessionRequest),
    JoinSession(JoinSessionRequest),
    Exchange(ExchangeRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateSessionAnswer {
    code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JoinSessionAnswer {
    protocol: Protocol,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerJoinedAnswer {
    protocol: Protocol,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExchangeAnswer {
    connection_info: ConnectionInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ErrorAnswer {
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum ServerMessage {
    CreateSession(CreateSessionAnswer),
    JoinSession(JoinSessionAnswer),
    PeerJoined(PeerJoinedAnswer),
    Exchange(ExchangeAnswer),
    Error(ErrorAnswer),
}

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("relay error: {0}")]
    Relay(String),
    #[error("unexpected message from relay")]
    UnexpectedMessage,
    #[error("connection closed")]
    ConnectionClosed,
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct RelayClient {
    ws: WsStream,
}

impl RelayClient {
    pub async fn connect(relay_url: &str) -> Result<Self, RelayError> {
        let (ws, _) = connect_async(relay_url).await?;
        Ok(Self { ws })
    }

    async fn send(&mut self, msg: &ClientMessage) -> Result<(), RelayError> {
        let text = serde_json::to_string(msg)?;
        self.ws.send(Message::Text(text.into())).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<ServerMessage, RelayError> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    let msg: ServerMessage = serde_json::from_str(&text)?;
                    return Ok(msg);
                }
                Some(Ok(Message::Close(_))) | None => return Err(RelayError::ConnectionClosed),
                Some(Err(e)) => return Err(RelayError::WebSocket(e)),
                _ => continue,
            }
        }
    }

    pub async fn create_session(
        &mut self,
        capabilities: Vec<Protocol>,
    ) -> Result<String, RelayError> {
        self.send(&ClientMessage::CreateSession(CreateSessionRequest {
            capabilities,
        }))
        .await?;

        match self.recv().await? {
            ServerMessage::CreateSession(answer) => Ok(answer.code),
            ServerMessage::Error(e) => Err(RelayError::Relay(e.message)),
            _ => Err(RelayError::UnexpectedMessage),
        }
    }

    pub async fn wait_for_peer(&mut self) -> Result<Protocol, RelayError> {
        match self.recv().await? {
            ServerMessage::PeerJoined(answer) => Ok(answer.protocol),
            ServerMessage::Error(e) => Err(RelayError::Relay(e.message)),
            _ => Err(RelayError::UnexpectedMessage),
        }
    }

    pub async fn join_session(
        &mut self,
        code: String,
        capabilities: Vec<Protocol>,
    ) -> Result<Protocol, RelayError> {
        self.send(&ClientMessage::JoinSession(JoinSessionRequest {
            code,
            capabilities,
        }))
        .await?;

        match self.recv().await? {
            ServerMessage::JoinSession(answer) => Ok(answer.protocol),
            ServerMessage::Error(e) => Err(RelayError::Relay(e.message)),
            _ => Err(RelayError::UnexpectedMessage),
        }
    }

    pub async fn send_exchange(&mut self, info: ConnectionInfo) -> Result<(), RelayError> {
        self.send(&ClientMessage::Exchange(ExchangeRequest {
            connection_info: info,
        }))
        .await
    }

    pub async fn recv_exchange(&mut self) -> Result<ConnectionInfo, RelayError> {
        match self.recv().await? {
            ServerMessage::Exchange(answer) => Ok(answer.connection_info),
            ServerMessage::Error(e) => Err(RelayError::Relay(e.message)),
            _ => Err(RelayError::UnexpectedMessage),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    fn iroh_info(id: &str) -> ConnectionInfo {
        ConnectionInfo::Iroh {
            node_id: id.into(),
            addrs: vec!["10.0.0.1:1234".into()],
        }
    }

    /// What the relay does with each client message.
    type Script = Box<dyn Fn(ClientMessage) -> Vec<Message> + Send + Sync>;

    fn reply(msg: &ServerMessage) -> Message {
        Message::Text(serde_json::to_string(msg).unwrap().into())
    }

    /// A relay that runs `script` for one client.
    async fn mock_relay(script: Script) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                let Message::Text(text) = msg else { continue };
                let client: ClientMessage = serde_json::from_str(&text).unwrap();
                for frame in script(client) {
                    if ws.send(frame).await.is_err() {
                        return;
                    }
                }
            }
        });
        url
    }

    #[tokio::test]
    async fn sender_flow_creates_a_session_and_exchanges_info() {
        let url = mock_relay(Box::new(|msg| match msg {
            ClientMessage::CreateSession(req) => {
                assert_eq!(req.capabilities, [Protocol::Iroh, Protocol::WebRtc]);
                // The relay answers with the code, then with the peer.
                vec![
                    reply(&ServerMessage::CreateSession(CreateSessionAnswer {
                        code: "AB12CD34".into(),
                    })),
                    reply(&ServerMessage::PeerJoined(PeerJoinedAnswer {
                        protocol: Protocol::Iroh,
                    })),
                ]
            }
            ClientMessage::Exchange(req) => {
                assert_eq!(req.connection_info, iroh_info("sender"));
                vec![reply(&ServerMessage::Exchange(ExchangeAnswer {
                    connection_info: iroh_info("receiver"),
                }))]
            }
            other => panic!("unexpected {other:?}"),
        }))
        .await;

        let mut relay = RelayClient::connect(&url).await.unwrap();
        let code = relay
            .create_session(vec![Protocol::Iroh, Protocol::WebRtc])
            .await
            .unwrap();
        assert_eq!(code, "AB12CD34");
        assert_eq!(relay.wait_for_peer().await.unwrap(), Protocol::Iroh);
        relay.send_exchange(iroh_info("sender")).await.unwrap();
        assert_eq!(relay.recv_exchange().await.unwrap(), iroh_info("receiver"));
    }

    #[tokio::test]
    async fn receiver_flow_joins_and_exchanges_info() {
        let url = mock_relay(Box::new(|msg| match msg {
            ClientMessage::JoinSession(req) => {
                assert_eq!(req.code, "AB12CD34");
                assert_eq!(req.capabilities, [Protocol::WebRtc]);
                vec![
                    reply(&ServerMessage::JoinSession(JoinSessionAnswer {
                        protocol: Protocol::WebRtc,
                    })),
                    reply(&ServerMessage::Exchange(ExchangeAnswer {
                        connection_info: ConnectionInfo::WebRtc {
                            sdp: "v=0 offer".into(),
                            ice_candidates: vec![],
                        },
                    })),
                ]
            }
            ClientMessage::Exchange(req) => {
                assert!(matches!(req.connection_info, ConnectionInfo::WebRtc { ref sdp, .. } if sdp == "v=0 answer"));
                vec![]
            }
            other => panic!("unexpected {other:?}"),
        }))
        .await;

        let mut relay = RelayClient::connect(&url).await.unwrap();
        let protocol = relay
            .join_session("AB12CD34".into(), vec![Protocol::WebRtc])
            .await
            .unwrap();
        assert_eq!(protocol, Protocol::WebRtc);
        let offer = relay.recv_exchange().await.unwrap();
        assert!(matches!(offer, ConnectionInfo::WebRtc { ref sdp, .. } if sdp == "v=0 offer"));
        relay
            .send_exchange(ConnectionInfo::WebRtc {
                sdp: "v=0 answer".into(),
                ice_candidates: vec![],
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_relay_that_hangs_up_is_a_closed_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let _ = ws.close(None).await;
        });

        let mut relay = RelayClient::connect(&url).await.unwrap();
        let err = relay.wait_for_peer().await.unwrap_err();
        assert!(
            matches!(err, RelayError::ConnectionClosed | RelayError::WebSocket(_)),
            "{err}"
        );
    }
}
