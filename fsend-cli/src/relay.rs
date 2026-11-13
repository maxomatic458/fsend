use std::{collections::VecDeque, time::Duration};

use colored::Colorize;
use fsend_proto::{
    code::{Code, CodeError},
    pairing::{ConnectionInfo, Sealed},
    relay::{ErrorCode, PATH, Transport},
    rendezvous::{Action, Event, Rendezvous, RendezvousError},
};
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// In order of preference.
pub const TRANSPORTS: [Transport; 2] = [Transport::Iroh, Transport::Webrtc];
pub const APP: &str = concat!("fsend-cli ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("invalid code: {0}")]
    Code(#[from] CodeError),
    #[error("could not reach the relay: {0}")]
    Connect(tokio_tungstenite::tungstenite::Error),
    #[error("timed out connecting to the relay")]
    ConnectTimeout,
    #[error("relay connection: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error(transparent)]
    Rendezvous(#[from] RendezvousError),
    #[error("unexpected message during pairing")]
    Unexpected,
}

impl RelayError {
    /// A message for the errors a user can act on.
    pub fn user_error(&self) -> Option<String> {
        let message = match self {
            Self::Code(_) => format!("{self}. A code looks like K7Q29XMV4P."),
            Self::Connect(_) | Self::ConnectTimeout => self.to_string(),
            Self::Rendezvous(RendezvousError::WrongCode) => {
                "the codes did not match. A code works only once, so start a new session and try again.".into()
            }
            Self::Rendezvous(RendezvousError::Relay { code, .. }) => match code {
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
                ErrorCode::Protocol | ErrorCode::Unknown => return None,
            },
            Self::WebSocket(_)
            | Self::Rendezvous(RendezvousError::Closed | RendezvousError::Protocol { .. })
            | Self::Unexpected => return None,
        };
        Some(format!("{} {message}", "error:".red()))
    }
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Relay {
    ws: WsStream,
    rendezvous: Rendezvous,
    events: VecDeque<Event>,
    failure: Option<RendezvousError>,
}

impl Relay {
    /// Connects to the relay at `base_url` (`/relay` is appended).
    pub async fn connect(base_url: &str, rendezvous: Rendezvous) -> Result<Self, RelayError> {
        let url = format!("{}{PATH}", base_url.trim_end_matches('/'));
        let (ws, _) = timeout(CONNECT_TIMEOUT, connect_async(url))
            .await
            .map_err(|_| RelayError::ConnectTimeout)?
            .map_err(RelayError::Connect)?;
        let mut relay = Self {
            ws,
            rendezvous,
            events: VecDeque::new(),
            failure: None,
        };
        relay.flush().await?;
        Ok(relay)
    }

    /// Sender only: the code for the receiver.
    pub async fn code(&mut self) -> Result<Code, RelayError> {
        match self.next_event().await? {
            Event::Code { code, .. } => Ok(code),
            _ => Err(RelayError::Unexpected),
        }
    }

    /// Waits until both sides proved they hold the same code.
    pub async fn paired(&mut self) -> Result<Transport, RelayError> {
        match self.next_event().await? {
            Event::Paired { transport, .. } => Ok(transport),
            _ => Err(RelayError::Unexpected),
        }
    }

    pub async fn send_connection(&mut self, info: ConnectionInfo) -> Result<(), RelayError> {
        self.rendezvous.send_sealed(Sealed::Connection { info });
        self.flush().await
    }

    pub async fn connection(&mut self) -> Result<ConnectionInfo, RelayError> {
        match self.next_event().await? {
            Event::Sealed {
                message: Sealed::Connection { info },
            } => Ok(info),
            _ => Err(RelayError::Unexpected),
        }
    }

    async fn next_event(&mut self) -> Result<Event, RelayError> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }
            if let Some(error) = &self.failure {
                return Err(error.clone().into());
            }
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => self.rendezvous.text(&text),
                Some(Ok(Message::Binary(data))) => self.rendezvous.binary(&data),
                Some(Ok(Message::Close(_)) | Err(_)) | None => self.rendezvous.closed(),
                Some(Ok(_)) => continue,
            }
            self.flush().await?;
        }
    }

    /// Sends what the rendezvous queued; events and failures wait for `next_event`.
    async fn flush(&mut self) -> Result<(), RelayError> {
        while let Some(action) = self.rendezvous.poll() {
            match action {
                Action::Send { text } => self.ws.send(Message::text(text)).await?,
                Action::SendBinary { data } => self.ws.send(Message::binary(data)).await?,
                Action::Event { event } => self.events.push_back(event),
                Action::Failed { error } => self.failure = Some(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsend_relay::Config;
    use tokio::net::TcpListener;

    async fn start_relay() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(fsend_relay::serve(listener, Config::default()));
        url
    }

    fn iroh_info(id: &str) -> ConnectionInfo {
        ConnectionInfo::Iroh {
            node_id: id.into(),
            addrs: vec!["10.0.0.1:1234".into()],
        }
    }

    async fn sender(url: &str) -> (Relay, Code) {
        let mut relay = Relay::connect(url, Rendezvous::sender(TRANSPORTS.to_vec(), APP))
            .await
            .unwrap();
        let code = relay.code().await.unwrap();
        (relay, code)
    }

    async fn receiver(url: &str, code: &Code) -> Result<Relay, RelayError> {
        Relay::connect(url, Rendezvous::receiver(code, TRANSPORTS.to_vec(), APP)).await
    }

    #[tokio::test]
    async fn both_sides_pair_through_the_relay_and_swap_connection_info() {
        let url = start_relay().await;
        let (mut sender, code) = sender(&url).await;
        let mut receiver = receiver(&url, &code).await.unwrap();

        let (sent, received) = tokio::join!(
            async {
                assert_eq!(sender.paired().await.unwrap(), Transport::Iroh);
                sender.send_connection(iroh_info("sender")).await.unwrap();
                sender.connection().await.unwrap()
            },
            async {
                assert_eq!(receiver.paired().await.unwrap(), Transport::Iroh);
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
        let (mut sender, code) = sender(&url).await;
        let wrong = Code::new(code.nameplate(), "000000").unwrap();
        let mut receiver = receiver(&url, &wrong).await.unwrap();

        let (sent, received) = tokio::join!(sender.paired(), receiver.paired());
        for result in [sent, received] {
            let err = result.unwrap_err();
            assert!(
                matches!(err, RelayError::Rendezvous(RendezvousError::WrongCode)),
                "{err}"
            );
            assert!(err.user_error().unwrap().contains("did not match"));
        }
    }

    #[tokio::test]
    async fn an_unknown_code_is_reported() {
        let url = start_relay().await;
        let code = "ZZZZ000000".parse().unwrap();
        let err = receiver(&url, &code)
            .await
            .unwrap()
            .paired()
            .await
            .unwrap_err();
        assert!(
            err.user_error()
                .unwrap()
                .contains("no session with this code"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_relay_that_hangs_up_is_a_closed_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.close(None).await;
        });

        let mut relay = Relay::connect(&url, Rendezvous::sender(TRANSPORTS.to_vec(), APP))
            .await
            .unwrap();
        let err = relay.code().await.unwrap_err();
        assert!(
            matches!(err, RelayError::Rendezvous(RendezvousError::Closed)),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_relay_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        drop(listener);
        let result = Relay::connect(&url, Rendezvous::sender(TRANSPORTS.to_vec(), APP)).await;
        assert!(matches!(result, Err(RelayError::Connect(_))));
    }
}
