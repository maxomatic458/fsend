use std::time::Duration;

use fsend_proto::code::Code;
use fsend_proto::pairing::{Channel, ConnectionInfo, PairingError, Pake, Role, Sealed, Signal};
use fsend_proto::peer::{Offer, offer};
use fsend_proto::relay::{ClientMessage, PATH, ServerMessage, Transport, VERSION};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use url::Url;

use crate::Error;
use crate::transfer::Outgoing;

/// For connecting and for each reply of the relay.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// For each signal from the peer.
const PEER_TIMEOUT: Duration = Duration::from_secs(60);
/// For the receiver's user; the relay's deadline comes first.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// In order of preference.
const TRANSPORTS: [Transport; 2] = [Transport::Iroh, Transport::Webrtc];

impl From<tungstenite::Error> for Error {
    fn from(e: tungstenite::Error) -> Self {
        Self::RelayLost(e.to_string())
    }
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Relay {
    ws: WsStream,
    channel: Channel,
    pub transport: Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Accepted,
    Rejected(Option<String>),
}

pub async fn pair_as_sender(
    base_url: &Url,
    app: &str,
    outgoing: &Outgoing,
    show_code: impl FnOnce(&Code),
) -> Result<Relay, Error> {
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
        return Err(protocol("expected created"));
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

/// Returns the sender's offer, validated.
pub async fn pair_as_receiver(
    base_url: &Url,
    app: &str,
    code: &Code,
) -> Result<(Relay, Offer), Error> {
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
        return Err(protocol("expected an offer"));
    };
    offer::validate(&offer).map_err(Error::InvalidOffer)?;
    Ok((relay, offer))
}

/// SPAKE2, then each side's sealed confirm proves both used the same code.
async fn handshake(
    mut ws: WsStream,
    code: &Code,
    role: Role,
    transport: Transport,
    app: &str,
) -> Result<Relay, Error> {
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
        return Err(protocol("expected a confirm message"));
    };
    tracing::info!("paired with {app} via {transport:?}");
    Ok(relay)
}

impl Relay {
    pub async fn answer(&mut self) -> Result<Answer, Error> {
        match self.recv_sealed(ANSWER_TIMEOUT).await? {
            Sealed::Accept => Ok(Answer::Accepted),
            Sealed::Reject { reason } => Ok(Answer::Rejected(reason)),
            _ => Err(protocol("expected an answer to the offer")),
        }
    }

    pub async fn accept(&mut self) -> Result<(), Error> {
        self.send_sealed(&Sealed::Accept).await
    }

    /// Ends the session.
    pub async fn reject(mut self, reason: Option<String>) -> Result<(), Error> {
        self.send_sealed(&Sealed::Reject { reason }).await?;
        Ok(self.ws.close(None).await?)
    }

    pub async fn send_connection(&mut self, info: ConnectionInfo) -> Result<(), Error> {
        self.send_sealed(&Sealed::Connection { info }).await
    }

    pub async fn connection(&mut self) -> Result<ConnectionInfo, Error> {
        match self.recv_sealed(PEER_TIMEOUT).await? {
            Sealed::Connection { info } => Ok(info),
            _ => Err(protocol("expected connection info")),
        }
    }

    async fn send_sealed(&mut self, message: &Sealed) -> Result<(), Error> {
        let frames = self.channel.seal(message)?;
        self.send_frames(frames).await
    }

    async fn send_frames(&mut self, frames: Vec<Vec<u8>>) -> Result<(), Error> {
        for frame in frames {
            send_frame(&mut self.ws, frame).await?;
        }
        Ok(())
    }

    /// Each frame within `wait`.
    async fn recv_sealed(&mut self, wait: Duration) -> Result<Sealed, Error> {
        loop {
            let frame = recv_signal(&mut self.ws, wait).await?;
            if let Some(message) = self.channel.open(&frame)? {
                return Ok(message);
            }
        }
    }
}

/// `/relay` is appended to `base_url`.
async fn connect(base_url: &Url) -> Result<WsStream, Error> {
    let mut url = base_url.clone();
    url.set_path(&format!("{}{PATH}", url.path().trim_end_matches('/')));
    let (ws, _) = timeout(CONNECT_TIMEOUT, connect_async(url.as_str()))
        .await
        .map_err(|_| Error::RelayUnreachable("timed out".into()))?
        .map_err(|e| Error::RelayUnreachable(e.to_string()))?;
    Ok(ws)
}

async fn send(ws: &mut WsStream, message: &ClientMessage) -> Result<(), Error> {
    let text = serde_json::to_string(message).expect("client messages serialize");
    Ok(ws.send(Message::text(text)).await?)
}

async fn send_frame(ws: &mut WsStream, frame: Vec<u8>) -> Result<(), Error> {
    Ok(ws.send(Message::binary(frame)).await?)
}

enum Frame {
    Message(ServerMessage),
    Signal(Vec<u8>),
}

/// A relay `error` fails.
async fn recv(ws: &mut WsStream, wait: Duration) -> Result<Frame, Error> {
    let next = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                    Ok(ServerMessage::Error { code }) => return Err(Error::Relay(code)),
                    Ok(message) => return Ok(Frame::Message(message)),
                    Err(_) => return Err(protocol("invalid relay message")),
                },
                Some(Ok(Message::Binary(data))) => return Ok(Frame::Signal(data.to_vec())),
                Some(Ok(Message::Close(_))) | None => {
                    return Err(Error::RelayLost("the relay closed the connection".into()));
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
            }
        }
    };
    timeout(wait, next)
        .await
        .map_err(|_| Error::Timeout("waiting for the relay or the other side"))?
}

async fn recv_message(ws: &mut WsStream, wait: Duration) -> Result<ServerMessage, Error> {
    match recv(ws, wait).await? {
        Frame::Message(message) => Ok(message),
        Frame::Signal(_) => Err(PairingError::UnexpectedSignal.into()),
    }
}

async fn recv_paired(ws: &mut WsStream, wait: Duration) -> Result<Transport, Error> {
    match recv_message(ws, wait).await? {
        ServerMessage::Paired { transport } => Ok(transport),
        _ => Err(protocol("expected paired")),
    }
}

fn protocol(what: &str) -> Error {
    Error::Protocol(format!("relay: {what}"))
}

async fn recv_signal(ws: &mut WsStream, wait: Duration) -> Result<Vec<u8>, Error> {
    match recv(ws, wait).await? {
        Frame::Signal(frame) => Ok(frame),
        Frame::Message(_) => Err(protocol("unexpected relay message")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsend_proto::peer::checksum::{Algorithm, Digest};
    use fsend_proto::peer::{Node, frame};
    use tokio::{net::TcpListener, sync::oneshot};

    use crate::transfer::test_support::{APP, start_relay, ws_url};

    fn file(name: &str, checksum: Option<&[u8]>) -> Node {
        Node::File {
            name: name.into(),
            size: 3,
            checksum: checksum.map(Digest::from),
        }
    }

    /// Need not be valid.
    fn offer_of(checksums: Option<Algorithm>, entries: Vec<Node>) -> Outgoing {
        let offer = Offer { checksums, entries };
        Outgoing {
            encoded: frame::encode_body(&Sealed::Offer(offer.clone())).unwrap(),
            offer,
            sources: vec![],
        }
    }

    fn iroh_info(id: &str) -> ConnectionInfo {
        ConnectionInfo::Iroh {
            node_id: id.into(),
            addrs: vec!["10.0.0.1:1234".parse().unwrap()],
            relays: vec![],
        }
    }

    async fn pair_with(
        url: &Url,
        outgoing: &Outgoing,
        receiver_code: impl FnOnce(Code) -> Code,
    ) -> (Result<Relay, Error>, Result<(Relay, Offer), Error>) {
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
    ) -> (Result<Relay, Error>, Result<Relay, Error>) {
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
            matches!(err, Error::Relay(fsend_proto::relay::Error::PeerLeft)),
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
        assert!(matches!(err, Error::InvalidOffer(_)), "{err}");
    }

    #[tokio::test]
    async fn a_wrong_code_fails_on_both_sides() {
        let url = start_relay().await;
        let (sender, receiver) =
            pair(&url, |code| Code::new(&code.nameplate(), "000000").unwrap()).await;
        for result in [sender, receiver] {
            let err = result.err().unwrap();
            assert!(
                matches!(err, Error::Pairing(PairingError::WrongCode)),
                "{err}"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_code_is_reported() {
        let url = start_relay().await;
        let code = "ZZZZ000000".parse().unwrap();
        let err = pair_as_receiver(&url, APP, &code).await.err().unwrap();
        assert!(
            matches!(err, Error::Relay(fsend_proto::relay::Error::NotFound)),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_unknown_relay_message_is_a_protocol_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws_url(&listener);
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
        assert!(matches!(err, Error::Protocol(_)), "{err}");
    }

    #[tokio::test]
    async fn a_relay_that_hangs_up_is_a_closed_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws_url(&listener);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.close(None).await;
        });

        let err = pair_as_sender(&url, APP, &offer_of(None, vec![file("a", None)]), |_| {})
            .await
            .err()
            .unwrap();
        assert!(matches!(err, Error::RelayLost(_)), "{err}");
    }

    #[tokio::test]
    async fn an_unreachable_relay_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ws_url(&listener);
        drop(listener);
        let result =
            pair_as_sender(&url, APP, &offer_of(None, vec![file("a", None)]), |_| {}).await;
        assert!(matches!(result, Err(Error::RelayUnreachable(_))));
    }
}
