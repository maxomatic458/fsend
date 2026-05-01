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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
