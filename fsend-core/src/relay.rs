use std::time::Duration;

use fsend_proto::{
    code::Code,
    pairing::{Channel, ConnectionInfo, PairingError, Pake, Role, Sealed, Signal},
    relay::{ClientMessage, ErrorCode, PATH, ServerMessage, Transport, VERSION},
};
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use url::Url;

/// For connecting and for each reply of the relay.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// For each signal from the peer; the relay gives the two sides a minute.
pub(crate) const PEER_TIMEOUT: Duration = Duration::from_secs(60);
/// In order of preference.
pub(crate) const TRANSPORTS: [Transport; 2] = [Transport::Iroh, Transport::Webrtc];

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("could not reach the relay: {0}")]
    Connect(tokio_tungstenite::tungstenite::Error),
    #[error("timed out connecting to the relay")]
    ConnectTimeout,
    #[error("relay connection: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("the relay connection closed")]
    Closed,
    #[error("timed out waiting for the relay or the other side")]
    Timeout,
    #[error("relay error ({code:?}): {message}")]
    Relay { code: ErrorCode, message: String },
    #[error("pairing failed: {0}")]
    Pairing(#[from] PairingError),
    #[error("protocol error: {0}")]
    Protocol(&'static str),
}

impl RelayError {
    /// A message for the errors a user can act on.
    pub fn user_message(&self) -> Option<String> {
        let message = match self {
            Self::Connect(_) | Self::ConnectTimeout | Self::Timeout => self.to_string(),
            Self::Pairing(PairingError::WrongCode) => {
                "the codes did not match. A code works only once, so start a new session and try again.".into()
            }
            Self::Relay { code, .. } => match code {
                ErrorCode::NotFound => {
                    "no session with this code. Check the code; a code works only once.".into()
                }
                ErrorCode::Expired => "the code expired. Start a new session.".into(),
                ErrorCode::Version => {
                    "the relay speaks another protocol version. Update fsend.".into()
                }
                ErrorCode::Busy => "the relay is busy. Try again later.".into(),
                ErrorCode::PeerLeft => "the other side left.".into(),
                ErrorCode::NoCommonTransport => {
                    "the two sides have no connection type in common. Update fsend on both sides.".into()
                }
                ErrorCode::Protocol => return None,
            },
            Self::WebSocket(_) | Self::Closed | Self::Pairing(_) | Self::Protocol(_) => {
                return None;
            }
        };
        Some(message)
    }
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The relay connection once both sides proved they hold the same code.
pub struct Relay {
    ws: WsStream,
    channel: Channel,
    pub transport: Transport,
}

/// Creates a session at the relay at `base_url` and pairs with whoever joins
/// it; `show_code` gets the code for the receiver. `app` names this client to the peer.
pub async fn pair_as_sender(
    base_url: &Url,
    app: &str,
    show_code: impl FnOnce(&Code),
) -> Result<Relay, RelayError> {
    let mut ws = connect(base_url).await?;
    send(
        &mut ws,
        &ClientMessage::Create {
            version: VERSION,
            transports: TRANSPORTS.to_vec(),
        },
    )
    .await?;
    let ServerMessage::Created {
        nameplate,
        expires_in,
    } = recv_message(&mut ws, CONNECT_TIMEOUT).await?
    else {
        return Err(RelayError::Protocol("expected created"));
    };
    let code = Code::with_random_password(&nameplate);
    show_code(&code);

    let wait = Duration::from_secs(expires_in.into()) + CONNECT_TIMEOUT;
    let transport = recv_paired(&mut ws, wait).await?;
    handshake(ws, &code, Role::Sender, transport, app).await
}

/// Joins the session of `code` at the relay at `base_url`.
pub async fn pair_as_receiver(base_url: &Url, app: &str, code: &Code) -> Result<Relay, RelayError> {
    let mut ws = connect(base_url).await?;
    send(
        &mut ws,
        &ClientMessage::Join {
            version: VERSION,
            nameplate: code.nameplate(),
            transports: TRANSPORTS.to_vec(),
        },
    )
    .await?;
    let transport = recv_paired(&mut ws, CONNECT_TIMEOUT).await?;
    handshake(ws, code, Role::Receiver, transport, app).await
}

/// SPAKE2, then each side's sealed confirm proves both used the same code.
async fn handshake(
    mut ws: WsStream,
    code: &Code,
    role: Role,
    transport: Transport,
    app: &str,
) -> Result<Relay, RelayError> {
    let (pake, message) = Pake::start(code);
    send_signal(&mut ws, Signal::Pake(message)).await?;
    let Signal::Pake(peer_message) = recv_signal(&mut ws).await? else {
        return Err(PairingError::UnexpectedSignal.into());
    };
    let mut channel = pake.finish(&peer_message, role)?;

    let confirm = Sealed::Confirm { app: app.into() };
    send_signal(&mut ws, Signal::Sealed(channel.seal(&confirm))).await?;
    let Signal::Sealed(peer_confirm) = recv_signal(&mut ws).await? else {
        return Err(PairingError::UnexpectedSignal.into());
    };
    let Sealed::Confirm { app } = channel.open(&peer_confirm)? else {
        return Err(RelayError::Protocol("expected a confirm message"));
    };
    tracing::info!("paired with {app} via {transport:?}");
    Ok(Relay {
        ws,
        channel,
        transport,
    })
}

impl Relay {
    pub async fn send_connection(&mut self, info: ConnectionInfo) -> Result<(), RelayError> {
        let sealed = self.channel.seal(&Sealed::Connection { info });
        send_signal(&mut self.ws, Signal::Sealed(sealed)).await
    }

    pub async fn connection(&mut self) -> Result<ConnectionInfo, RelayError> {
        let Signal::Sealed(sealed) = recv_signal(&mut self.ws).await? else {
            return Err(PairingError::UnexpectedSignal.into());
        };
        match self.channel.open(&sealed)? {
            Sealed::Connection { info } => Ok(info),
            Sealed::Confirm { .. } => Err(RelayError::Protocol("repeated confirm message")),
        }
    }
}

/// Connects to the relay at `base_url` (`/relay` is appended).
async fn connect(base_url: &Url) -> Result<WsStream, RelayError> {
    let mut url = base_url.clone();
    url.set_path(&format!("{}{PATH}", url.path().trim_end_matches('/')));
    let (ws, _) = timeout(CONNECT_TIMEOUT, connect_async(url.as_str()))
        .await
        .map_err(|_| RelayError::ConnectTimeout)?
        .map_err(RelayError::Connect)?;
    Ok(ws)
}

async fn send(ws: &mut WsStream, message: &ClientMessage) -> Result<(), RelayError> {
    let text = serde_json::to_string(message).expect("client messages serialize");
    Ok(ws.send(Message::text(text)).await?)
}

async fn send_signal(ws: &mut WsStream, signal: Signal) -> Result<(), RelayError> {
    Ok(ws.send(Message::binary(signal.encode())).await?)
}

enum Frame {
    Message(ServerMessage),
    Signal(Signal),
}

/// The next relay message or peer signal within `wait`; a relay error fails.
async fn recv(ws: &mut WsStream, wait: Duration) -> Result<Frame, RelayError> {
    let next = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                    Ok(ServerMessage::Error { code, message }) => {
                        return Err(RelayError::Relay { code, message });
                    }
                    Ok(message) => return Ok(Frame::Message(message)),
                    Err(_) => return Err(RelayError::Protocol("invalid relay message")),
                },
                Some(Ok(Message::Binary(data))) => {
                    let signal = Signal::decode(&data).ok_or(PairingError::UnexpectedSignal)?;
                    return Ok(Frame::Signal(signal));
                }
                Some(Ok(Message::Close(_))) | None => return Err(RelayError::Closed),
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
            }
        }
    };
    timeout(wait, next).await.map_err(|_| RelayError::Timeout)?
}

async fn recv_message(ws: &mut WsStream, wait: Duration) -> Result<ServerMessage, RelayError> {
    match recv(ws, wait).await? {
        Frame::Message(message) => Ok(message),
        Frame::Signal(_) => Err(PairingError::UnexpectedSignal.into()),
    }
}

async fn recv_paired(ws: &mut WsStream, wait: Duration) -> Result<Transport, RelayError> {
    match recv_message(ws, wait).await? {
        ServerMessage::Paired { transport } => Ok(transport),
        _ => Err(RelayError::Protocol("expected paired")),
    }
}

async fn recv_signal(ws: &mut WsStream) -> Result<Signal, RelayError> {
    match recv(ws, PEER_TIMEOUT).await? {
        Frame::Signal(signal) => Ok(signal),
        Frame::Message(_) => Err(RelayError::Protocol("unexpected relay message")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsend_relay::Config;
    use tokio::{net::TcpListener, sync::oneshot};

    const APP: &str = "fsend-core tests";

    fn ws(listener: &TcpListener) -> Url {
        Url::parse(&format!("ws://{}", listener.local_addr().unwrap())).unwrap()
    }

    async fn start_relay() -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws(&listener);
        tokio::spawn(fsend_relay::serve(listener, Config::default()));
        url
    }

    fn iroh_info(id: &str) -> ConnectionInfo {
        ConnectionInfo::Iroh {
            node_id: id.into(),
            addrs: vec!["10.0.0.1:1234".parse().unwrap()],
            relays: vec![],
        }
    }

    /// Pairs a sender with a receiver that uses `receiver_code(code)`.
    async fn pair(
        url: &Url,
        receiver_code: impl FnOnce(Code) -> Code,
    ) -> (Result<Relay, RelayError>, Result<Relay, RelayError>) {
        let (code_tx, code_rx) = oneshot::channel();
        tokio::join!(
            pair_as_sender(url, APP, |code| code_tx.send(code.clone()).unwrap()),
            async {
                let code = receiver_code(code_rx.await.unwrap());
                pair_as_receiver(url, APP, &code).await
            }
        )
    }

    #[tokio::test]
    async fn both_sides_pair_through_the_relay_and_swap_connection_info() {
        let url = start_relay().await;
        let (sender, receiver) = pair(&url, |code| code).await;
        let (mut sender, mut receiver) = (sender.unwrap(), receiver.unwrap());
        assert_eq!(sender.transport, Transport::Iroh);
        assert_eq!(receiver.transport, Transport::Iroh);

        let (sent, received) = tokio::join!(
            async {
                sender.send_connection(iroh_info("sender")).await.unwrap();
                sender.connection().await.unwrap()
            },
            async {
                let info = receiver.connection().await.unwrap();
                receiver
                    .send_connection(iroh_info("receiver"))
                    .await
                    .unwrap();
                info
            }
        );
        assert_eq!(sent, iroh_info("receiver"));
        assert_eq!(received, iroh_info("sender"));
    }

    #[tokio::test]
    async fn a_wrong_code_fails_on_both_sides() {
        let url = start_relay().await;
        let (sender, receiver) =
            pair(&url, |code| Code::new(&code.nameplate(), "000000").unwrap()).await;
        for result in [sender, receiver] {
            let err = result.err().unwrap();
            assert!(
                matches!(err, RelayError::Pairing(PairingError::WrongCode)),
                "{err}"
            );
            assert!(err.user_message().unwrap().contains("did not match"));
        }
    }

    #[tokio::test]
    async fn an_unknown_code_is_reported() {
        let url = start_relay().await;
        let code = "ZZZZ000000".parse().unwrap();
        let err = pair_as_receiver(&url, APP, &code).await.err().unwrap();
        assert!(
            err.user_message()
                .unwrap()
                .contains("no session with this code"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_unknown_relay_message_is_a_protocol_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws(&listener);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            let _ = ws
                .send(Message::text(r#"{"type":"motd","text":"hi"}"#))
                .await;
            let _ = ws.next().await;
        });

        let err = pair_as_sender(&url, APP, |_| {}).await.err().unwrap();
        assert!(matches!(err, RelayError::Protocol(_)), "{err}");
    }

    #[tokio::test]
    async fn a_relay_that_hangs_up_is_a_closed_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws(&listener);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.close(None).await;
        });

        let err = pair_as_sender(&url, APP, |_| {}).await.err().unwrap();
        assert!(matches!(err, RelayError::Closed), "{err}");
    }

    #[tokio::test]
    async fn an_unreachable_relay_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws(&listener);
        drop(listener);
        let result = pair_as_sender(&url, APP, |_| {}).await;
        assert!(matches!(result, Err(RelayError::Connect(_))));
    }
}
