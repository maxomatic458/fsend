use std::time::Duration;

use fsend_proto::{
    code::Code,
    pairing::{Channel, ConnectionInfo, PairingError, Pake, Role, Sealed, Signal},
    peer::{
        Offer,
        offer::{self, OfferError},
    },
    relay::{ClientMessage, ErrorCode, PATH, ServerMessage, Transport, VERSION},
};
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use url::Url;

use crate::transfer::Outgoing;

/// For connecting and for each reply of the relay.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// For each signal from the peer.
pub(crate) const PEER_TIMEOUT: Duration = Duration::from_secs(60);
/// For the receiver's answer to the offer, which its user takes; the relay's
/// deadline for pairing comes first.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
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
    #[error("the sender offered invalid files: {0}")]
    InvalidOffer(OfferError),
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
            Self::WebSocket(_)
            | Self::Closed
            | Self::Pairing(_)
            | Self::InvalidOffer(_)
            | Self::Protocol(_) => {
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

/// The receiver's answer to the offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Accepted,
    Rejected(Option<String>),
}

/// Creates a session at the relay at `base_url`, pairs with whoever joins it
/// and offers them `outgoing`; `show_code` gets the code for the receiver.
/// `app` names this client to the peer.
pub async fn pair_as_sender(
    base_url: &Url,
    app: &str,
    outgoing: &Outgoing,
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
    let mut relay = handshake(ws, &code, Role::Sender, transport, app).await?;
    let frames = relay.channel.seal_plaintext(&outgoing.encoded);
    relay.send_frames(frames).await?;
    Ok(relay)
}

/// Joins the session of `code` at the relay at `base_url`; returns the
/// sender's offer, checked.
pub async fn pair_as_receiver(
    base_url: &Url,
    app: &str,
    code: &Code,
) -> Result<(Relay, Offer), RelayError> {
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
    let mut relay = handshake(ws, code, Role::Receiver, transport, app).await?;
    let Sealed::Offer(offer) = relay.recv_sealed(PEER_TIMEOUT).await? else {
        return Err(RelayError::Protocol("expected an offer"));
    };
    offer::validate(&offer).map_err(RelayError::InvalidOffer)?;
    Ok((relay, offer))
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
    send_frame(&mut ws, Signal::Pake(message).encode()).await?;
    let peer_message = recv_signal(&mut ws, PEER_TIMEOUT).await?;
    let Some(Signal::Pake(peer_message)) = Signal::decode(&peer_message) else {
        return Err(PairingError::UnexpectedSignal.into());
    };
    let channel = pake.finish(&peer_message, role)?;
    let mut relay = Relay {
        ws,
        channel,
        transport,
    };

    relay
        .send_sealed(&Sealed::Confirm { app: app.into() })
        .await?;
    let Sealed::Confirm { app } = relay.recv_sealed(PEER_TIMEOUT).await? else {
        return Err(RelayError::Protocol("expected a confirm message"));
    };
    tracing::info!("paired with {app} via {transport:?}");
    Ok(relay)
}

impl Relay {
    /// Sender: waits for the receiver's user to answer the offer.
    pub async fn answer(&mut self) -> Result<Answer, RelayError> {
        match self.recv_sealed(ANSWER_TIMEOUT).await? {
            Sealed::Accept => Ok(Answer::Accepted),
            Sealed::Reject { reason } => Ok(Answer::Rejected(reason)),
            _ => Err(RelayError::Protocol("expected an answer to the offer")),
        }
    }

    /// Receiver: takes the offer; the two sides connect next.
    pub async fn accept(&mut self) -> Result<(), RelayError> {
        self.send_sealed(&Sealed::Accept).await
    }

    /// Receiver: turns the offer down, which ends the session.
    pub async fn reject(mut self, reason: Option<String>) -> Result<(), RelayError> {
        self.send_sealed(&Sealed::Reject { reason }).await?;
        Ok(self.ws.close(None).await?)
    }

    pub async fn send_connection(&mut self, info: ConnectionInfo) -> Result<(), RelayError> {
        self.send_sealed(&Sealed::Connection { info }).await
    }

    pub async fn connection(&mut self) -> Result<ConnectionInfo, RelayError> {
        match self.recv_sealed(PEER_TIMEOUT).await? {
            Sealed::Connection { info } => Ok(info),
            _ => Err(RelayError::Protocol("expected connection info")),
        }
    }

    async fn send_sealed(&mut self, message: &Sealed) -> Result<(), RelayError> {
        let frames = self.channel.seal(message)?;
        self.send_frames(frames).await
    }

    async fn send_frames(&mut self, frames: Vec<Vec<u8>>) -> Result<(), RelayError> {
        for frame in frames {
            send_frame(&mut self.ws, frame).await?;
        }
        Ok(())
    }

    /// The peer's next sealed message, each of its frames within `wait`.
    async fn recv_sealed(&mut self, wait: Duration) -> Result<Sealed, RelayError> {
        loop {
            let frame = recv_signal(&mut self.ws, wait).await?;
            if let Some(message) = self.channel.open(&frame)? {
                return Ok(message);
            }
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

async fn send_frame(ws: &mut WsStream, frame: Vec<u8>) -> Result<(), RelayError> {
    Ok(ws.send(Message::binary(frame)).await?)
}

enum Frame {
    Message(ServerMessage),
    /// A binary frame from the peer.
    Signal(Vec<u8>),
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
                Some(Ok(Message::Binary(data))) => return Ok(Frame::Signal(data.to_vec())),
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

async fn recv_signal(ws: &mut WsStream, wait: Duration) -> Result<Vec<u8>, RelayError> {
    match recv(ws, wait).await? {
        Frame::Signal(frame) => Ok(frame),
        Frame::Message(_) => Err(RelayError::Protocol("unexpected relay message")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsend_proto::pairing::Sealed;
    use fsend_proto::peer::checksum::{Algorithm, Digest};
    use fsend_proto::peer::{Node, frame};
    use fsend_relay::Config;
    use tokio::{net::TcpListener, sync::oneshot};

    const APP: &str = "fsend-core tests";

    fn file(name: &str, checksum: Option<&[u8]>) -> Node {
        Node::File {
            name: name.into(),
            size: 3,
            checksum: checksum.map(Digest::from),
        }
    }

    /// An offer of `entries`, with checksums under `checksums`; it need not be valid.
    fn offer_of(checksums: Option<Algorithm>, entries: Vec<Node>) -> Outgoing {
        let offer = Offer { checksums, entries };
        Outgoing {
            encoded: frame::encode_body(&Sealed::Offer(offer.clone())).unwrap(),
            offer,
            sources: vec![],
        }
    }

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

    /// Pairs a sender of `outgoing` with a receiver that uses `receiver_code(code)`.
    async fn pair_with(
        url: &Url,
        outgoing: &Outgoing,
        receiver_code: impl FnOnce(Code) -> Code,
    ) -> (
        Result<Relay, RelayError>,
        Result<(Relay, Offer), RelayError>,
    ) {
        let (code_tx, code_rx) = oneshot::channel();
        tokio::join!(
            pair_as_sender(url, APP, outgoing, |code: &Code| code_tx
                .send(code.clone())
                .unwrap()),
            async {
                let code = receiver_code(code_rx.await.unwrap());
                pair_as_receiver(url, APP, &code).await
            }
        )
    }

    async fn pair(
        url: &Url,
        receiver_code: impl FnOnce(Code) -> Code,
    ) -> (Result<Relay, RelayError>, Result<Relay, RelayError>) {
        let outgoing = offer_of(None, vec![file("a", None)]);
        let (sender, receiver) = pair_with(url, &outgoing, receiver_code).await;
        (sender, receiver.map(|(relay, _)| relay))
    }

    #[tokio::test]
    async fn the_offer_comes_first_and_connection_info_once_accepted() {
        let url = start_relay().await;
        let outgoing = offer_of(Some(Algorithm::Sha256), vec![file("a", Some(&[1; 32]))]);
        let (sender, receiver) = pair_with(&url, &outgoing, |code| code).await;
        let (mut sender, (mut receiver, offered)) = (sender.unwrap(), receiver.unwrap());
        assert_eq!(offered, outgoing.offer);
        assert_eq!(sender.transport, Transport::Iroh);
        assert_eq!(receiver.transport, Transport::Iroh);

        let (sent, received) = tokio::join!(
            async {
                assert_eq!(sender.answer().await.unwrap(), Answer::Accepted);
                sender.send_connection(iroh_info("sender")).await.unwrap();
                sender.connection().await.unwrap()
            },
            async {
                receiver.accept().await.unwrap();
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
    async fn a_rejected_offer_ends_the_session() {
        let url = start_relay().await;
        let (sender, receiver) = pair(&url, |code| code).await;
        let (mut sender, receiver) = (sender.unwrap(), receiver.unwrap());

        receiver.reject(Some("not now".into())).await.unwrap();
        assert_eq!(
            sender.answer().await.unwrap(),
            Answer::Rejected(Some("not now".into()))
        );
        // Nothing but the relay's goodbye follows.
        let err = sender.connection().await.err().unwrap();
        assert!(
            matches!(
                err,
                RelayError::Relay {
                    code: ErrorCode::PeerLeft,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_large_offer_spans_many_frames() {
        let url = start_relay().await;
        // Checksums do not compress.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let entries: Vec<_> = (0..50_000)
            .map(|i| {
                let mut digest = [0; 32];
                for chunk in digest.chunks_mut(8) {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    chunk.copy_from_slice(&state.to_le_bytes());
                }
                file(&format!("file-{i}.bin"), Some(&digest))
            })
            .collect();
        let outgoing = offer_of(Some(Algorithm::Sha256), entries);
        assert!(
            outgoing.encoded.len() > 40 * fsend_proto::relay::MAX_FRAME_BYTES,
            "{} bytes",
            outgoing.encoded.len()
        );

        let (sender, receiver) = pair_with(&url, &outgoing, |code| code).await;
        sender.unwrap();
        assert_eq!(receiver.unwrap().1, outgoing.offer);
    }

    #[tokio::test]
    async fn an_invalid_offer_fails_the_receiver() {
        let url = start_relay().await;
        let outgoing = offer_of(None, vec![file("..", None)]);
        let (_sender, receiver) = pair_with(&url, &outgoing, |code| code).await;
        let err = receiver.err().unwrap();
        assert!(matches!(err, RelayError::InvalidOffer(_)), "{err}");
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

        let err = pair_as_sender(&url, APP, &offer_of(None, vec![file("a", None)]), |_| {})
            .await
            .err()
            .unwrap();
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

        let err = pair_as_sender(&url, APP, &offer_of(None, vec![file("a", None)]), |_| {})
            .await
            .err()
            .unwrap();
        assert!(matches!(err, RelayError::Closed), "{err}");
    }

    #[tokio::test]
    async fn an_unreachable_relay_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws(&listener);
        drop(listener);
        let result =
            pair_as_sender(&url, APP, &offer_of(None, vec![file("a", None)]), |_| {}).await;
        assert!(matches!(result, Err(RelayError::Connect(_))));
    }
}
