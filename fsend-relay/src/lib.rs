use std::{
    collections::HashMap,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, Utf8Bytes, WebSocket},
    },
    response::Response,
    routing::get,
};
use fsend_proto::{
    code::Nameplate,
    relay::{
        ClientMessage, Error, MAX_FRAME_BYTES, MAX_SIGNAL_BYTES, PATH, ServerMessage, Transport,
        VERSION,
    },
};
use serde::Deserialize;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::{Instant, Interval, MissedTickBehavior, sleep_until, timeout},
};
use tracing::info;

/// Signals queued for a peer that is still reading earlier ones.
const QUEUED_SIGNALS: usize = 4;
/// Nameplates tried before a nearly full relay reports itself busy.
const NAMEPLATE_TRIES: usize = 64;
/// Time for any single write to a client.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Time for a client to say `create` or `join` after connecting.
const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Config {
    /// How long a session waits for its receiver.
    pub session_ttl: Duration,
    /// How long the two sides may signal after the join, the receiver's decision included.
    pub pair_timeout: Duration,
    /// Sessions open at once, waiting or pairing.
    pub max_sessions: usize,
    pub ping_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(5 * 60),
            pair_timeout: Duration::from_secs(10 * 60),
            max_sessions: 10_000,
            ping_interval: Duration::from_secs(20),
        }
    }
}

pub fn router(config: Config) -> Router {
    let relay = Arc::new(Relay {
        config,
        slots: Mutex::default(),
        active: AtomicUsize::new(0),
    });
    Router::new().route(PATH, get(upgrade)).with_state(relay)
}

pub async fn serve(listener: TcpListener, config: Config) -> io::Result<()> {
    axum::serve(listener, router(config)).await
}

struct Relay {
    config: Config,
    slots: Mutex<HashMap<Nameplate, Slot>>,
    active: AtomicUsize,
}

enum Slot {
    Open {
        transports: Vec<Transport>,
        join: oneshot::Sender<Result<Peer, Error>>,
        expires_at: Instant,
    },
    /// Kept for a while so a late receiver hears `expired`, not `not_found`.
    Expired { until: Instant },
}

/// The receiver's answer to the waiting sender.
type Joined = oneshot::Receiver<Result<Peer, Error>>;

struct Peer {
    transport: Transport,
    link: Link,
    /// When pairing must be done, shared by both sides.
    deadline: Instant,
}

/// One side of the channel pair between the two connections.
struct Link {
    to_peer: mpsc::Sender<Bytes>,
    from_peer: mpsc::Receiver<Bytes>,
}

impl Link {
    fn pair() -> (Self, Self) {
        let (a_tx, a_rx) = mpsc::channel(QUEUED_SIGNALS);
        let (b_tx, b_rx) = mpsc::channel(QUEUED_SIGNALS);
        (
            Self {
                to_peer: a_tx,
                from_peer: b_rx,
            },
            Self {
                to_peer: b_tx,
                from_peer: a_rx,
            },
        )
    }
}

/// Counts a session as active while it lives.
struct Active(Arc<Relay>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Relay {
    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<Nameplate, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens a session under a fresh nameplate, or `None` when full.
    fn open(self: &Arc<Self>, transports: Vec<Transport>) -> Option<(Nameplate, Joined, Active)> {
        if self.active.fetch_add(1, Ordering::Relaxed) >= self.config.max_sessions {
            self.active.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        let active = Active(self.clone());
        let now = Instant::now();
        let (join, joined) = oneshot::channel();
        let mut slots = self.slots();
        slots.retain(|_, slot| !matches!(slot, Slot::Expired { until } if *until <= now));
        let nameplate = std::iter::repeat_with(Nameplate::random)
            .take(NAMEPLATE_TRIES)
            .find(|nameplate| !slots.contains_key(nameplate))?;
        slots.insert(
            nameplate.clone(),
            Slot::Open {
                transports,
                join,
                expires_at: now + self.config.session_ttl,
            },
        );
        Some((nameplate, joined, active))
    }

    /// Drops a session nobody joined; false if it was joined meanwhile.
    fn close(&self, nameplate: &Nameplate, expired: bool) -> bool {
        let mut slots = self.slots();
        if !matches!(slots.get(nameplate), Some(Slot::Open { .. })) {
            return false;
        }
        if expired {
            let until = Instant::now() + self.config.session_ttl;
            slots.insert(nameplate.clone(), Slot::Expired { until });
        } else {
            slots.remove(nameplate);
        }
        true
    }
}

async fn upgrade(ws: WebSocketUpgrade, State(relay): State<Arc<Relay>>) -> Response {
    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |ws| connection(Socket::new(ws), relay))
}

async fn connection(mut ws: Socket, relay: Arc<Relay>) {
    let text = match timeout(FIRST_MESSAGE_TIMEOUT, ws.recv()).await {
        Ok(Some(Frame::Text(text))) => text,
        Ok(Some(Frame::Binary(_))) => return ws.fail(Error::Protocol).await,
        Ok(None) | Err(_) => return,
    };
    if !has_supported_version(&text) {
        return ws.fail(Error::Version).await;
    }
    match serde_json::from_str::<ClientMessage>(&text) {
        Ok(ClientMessage::Create { transports, .. }) => create(ws, relay, transports).await,
        Ok(ClientMessage::Join {
            nameplate,
            transports,
            ..
        }) => join(ws, relay, nameplate, transports).await,
        Err(_) => ws.fail(Error::Protocol).await,
    }
}

fn has_supported_version(text: &str) -> bool {
    #[derive(Deserialize)]
    struct Versioned {
        version: Option<serde_json::Value>,
    }
    serde_json::from_str::<Versioned>(text).is_ok_and(|v| v.version == Some(VERSION.into()))
}

async fn create(mut ws: Socket, relay: Arc<Relay>, transports: Vec<Transport>) {
    let Some((nameplate, mut joined, _active)) = relay.open(transports) else {
        return ws.fail(Error::Busy).await;
    };
    info!(%nameplate, "session created");
    let ttl = relay.config.session_ttl;
    let created = ServerMessage::Created {
        nameplate: nameplate.clone(),
        expires_in: ttl.as_secs().try_into().unwrap_or(u32::MAX),
    };
    if !ws.send_json(&created).await {
        relay.close(&nameplate, false);
        return;
    }

    let expiry = sleep_until(Instant::now() + ttl);
    tokio::pin!(expiry);
    let mut ping = pinger(relay.config.ping_interval);
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = &mut joined => break outcome,
            _ = &mut expiry => {
                if relay.close(&nameplate, true) {
                    return ws.fail(Error::Expired).await;
                }
                // Joined just now; the peer is on its way.
                break joined.await;
            }
            _ = ping.tick() => {
                if !ws.ping().await {
                    relay.close(&nameplate, false);
                    return;
                }
            }
            incoming = ws.recv() => {
                relay.close(&nameplate, false);
                if incoming.is_some() {
                    ws.fail(Error::Protocol).await;
                }
                return;
            }
        }
    };
    match outcome {
        Ok(Ok(peer)) => forward(ws, &relay.config, peer).await,
        Ok(Err(code)) => ws.fail(code).await,
        // `join` always answers before dropping the slot.
        Err(_) => {}
    }
}

async fn join(ws: Socket, relay: Arc<Relay>, nameplate: Nameplate, transports: Vec<Transport>) {
    // Consumed by the first attempt, whatever happens next.
    let slot = relay.slots().remove(&nameplate);
    let (offered, join) = match slot {
        Some(Slot::Open {
            transports,
            join,
            expires_at,
        }) if expires_at > Instant::now() => (transports, join),
        Some(Slot::Open { join, .. }) => {
            let _ = join.send(Err(Error::Expired));
            return ws.fail(Error::Expired).await;
        }
        Some(Slot::Expired { .. }) => return ws.fail(Error::Expired).await,
        None => return ws.fail(Error::NotFound).await,
    };
    let Some(transport) = offered.into_iter().find(|t| transports.contains(t)) else {
        let _ = join.send(Err(Error::NoCommonTransport));
        return ws.fail(Error::NoCommonTransport).await;
    };
    info!(%nameplate, ?transport, "session joined");

    let (theirs, ours) = Link::pair();
    let deadline = Instant::now() + relay.config.pair_timeout;
    let peer = Peer {
        transport,
        link: theirs,
        deadline,
    };
    if join.send(Ok(peer)).is_err() {
        return ws.fail(Error::PeerLeft).await;
    }
    let peer = Peer {
        transport,
        link: ours,
        deadline,
    };
    forward(ws, &relay.config, peer).await;
}

/// Relays signal frames both ways until a side leaves or the deadline passes;
/// a slow peer holds back a fast one.
async fn forward(mut ws: Socket, config: &Config, peer: Peer) {
    let Peer {
        transport,
        mut link,
        deadline,
    } = peer;
    if !ws.send_json(&ServerMessage::Paired { transport }).await {
        return;
    }
    let deadline = sleep_until(deadline);
    tokio::pin!(deadline);
    let mut ping = pinger(config.ping_interval);
    let mut signal_bytes = 0;
    loop {
        let room = link.to_peer.capacity() > 0;
        if !room {
            // Unread frames hold the client's pong back while the peer catches up.
            ws.excuse();
            ping.reset();
        }
        tokio::select! {
            // Report `expired`, not `peer_left`, when both fire.
            biased;
            _ = &mut deadline => {
                return ws.fail(Error::Expired).await;
            }
            _ = ping.tick(), if room => {
                if !ws.ping().await {
                    return;
                }
                // A ping can wait behind frames; the client gets a full period to answer.
                ping.reset();
            }
            forwarded = link.from_peer.recv() => match forwarded {
                Some(data) => {
                    if !ws.send(Message::Binary(data)).await {
                        return;
                    }
                    // Its pong queues behind the frames; that they go out shows it is there.
                    ws.excuse();
                }
                None => {
                    return ws.fail(Error::PeerLeft).await;
                }
            },
            // Wakes once the peer has taken a signal, to read the next.
            _ = link.to_peer.reserve(), if !room => {}
            incoming = ws.recv(), if room => {
                let Some(frame) = incoming else { return };
                let data = match frame {
                    Frame::Binary(data) if signal_bytes + data.len() <= MAX_SIGNAL_BYTES => data,
                    _ => return ws.fail(Error::Protocol).await,
                };
                signal_bytes += data.len();
                // There is room; fails only once the peer is gone, which `from_peer` reports.
                let _ = link.to_peer.try_send(data);
            }
        }
    }
}

fn pinger(period: Duration) -> Interval {
    let mut ping = tokio::time::interval_at(Instant::now() + period, period);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ping
}

struct Socket {
    ws: WebSocket,
    answered: bool,
}

impl Socket {
    fn new(ws: WebSocket) -> Self {
        Self { ws, answered: true }
    }

    /// Counts the last ping as answered, for a client whose pong is held up.
    fn excuse(&mut self) {
        self.answered = true;
    }

    /// Pings, unless the last ping went unanswered: the client is gone.
    async fn ping(&mut self) -> bool {
        if !std::mem::replace(&mut self.answered, false) {
            return false;
        }
        self.send(Message::Ping(Default::default())).await
    }

    async fn send(&mut self, message: Message) -> bool {
        matches!(
            timeout(SEND_TIMEOUT, self.ws.send(message)).await,
            Ok(Ok(()))
        )
    }

    async fn send_json(&mut self, message: &ServerMessage) -> bool {
        let text = serde_json::to_string(message).expect("server messages serialize");
        self.send(Message::text(text)).await
    }

    async fn fail(mut self, code: Error) {
        if self.send_json(&ServerMessage::Error { code }).await {
            self.send(Message::Close(None)).await;
        }
    }

    /// Anything the client sends counts as a pong.
    async fn recv(&mut self) -> Option<Frame> {
        loop {
            let message = self.ws.recv().await?;
            self.answered = true;
            match message {
                Ok(Message::Text(text)) => return Some(Frame::Text(text)),
                Ok(Message::Binary(data)) => return Some(Frame::Binary(data)),
                Ok(Message::Pong(_) | Message::Ping(_)) => {}
                Ok(Message::Close(_)) | Err(_) => return None,
            }
        }
    }
}

enum Frame {
    Text(Utf8Bytes),
    Binary(Bytes),
}
