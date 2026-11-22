use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr},
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
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message, Utf8Bytes, WebSocket},
    },
    http::{HeaderMap, HeaderName},
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
/// Addresses tracked before those without a connection are forgotten.
const MAX_TRACKED: usize = 100_000;

#[derive(Debug, Clone)]
pub struct Config {
    /// How long a session waits for its receiver.
    pub session_ttl: Duration,
    /// How long the two sides may signal after the join, the receiver's decision included.
    pub pair_timeout: Duration,
    /// Sessions open at once, waiting or pairing.
    pub max_sessions: usize,
    pub ping_interval: Duration,
    /// Connections open at once from one address.
    pub connections_per_ip: usize,
    /// Sessions one address may create or join per minute, all at once if it likes.
    pub sessions_per_minute: u32,
    /// The header a proxy in front sets to the client's address, which
    /// otherwise is the connection's own. Anyone reaching the relay directly can
    /// set it too.
    pub ip_header: Option<HeaderName>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(5 * 60),
            pair_timeout: Duration::from_secs(10 * 60),
            max_sessions: 10_000,
            ping_interval: Duration::from_secs(20),
            connections_per_ip: 32,
            sessions_per_minute: 30,
            ip_header: None,
        }
    }
}

pub async fn serve(listener: TcpListener, config: Config) -> io::Result<()> {
    let relay = Arc::new(Relay {
        config,
        slots: Mutex::default(),
        active: AtomicUsize::new(0),
        clients: Mutex::default(),
    });
    let router = Router::new().route(PATH, get(upgrade)).with_state(relay);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

struct Relay {
    config: Config,
    slots: Mutex<HashMap<Nameplate, Slot>>,
    active: AtomicUsize,
    clients: Mutex<HashMap<IpAddr, Client>>,
}

/// What one address uses of the relay.
struct Client {
    connections: usize,
    /// Sessions it may still create or join; refills over a minute.
    allowance: f64,
    refilled: Instant,
}

impl Client {
    fn refill(&mut self, per_minute: u32) {
        let now = Instant::now();
        let refilled =
            now.duration_since(self.refilled).as_secs_f64() * f64::from(per_minute) / 60.0;
        self.allowance = (self.allowance + refilled).min(f64::from(per_minute));
        self.refilled = now;
    }
}

/// Counts a connection of its address while it lives.
struct Connected {
    relay: Arc<Relay>,
    ip: IpAddr,
}

impl Drop for Connected {
    fn drop(&mut self) {
        let per_minute = self.relay.config.sessions_per_minute;
        let mut clients = self.relay.clients();
        let Some(client) = clients.get_mut(&self.ip) else {
            return;
        };
        client.connections -= 1;
        client.refill(per_minute);
        if client.connections == 0 && client.allowance >= f64::from(per_minute) {
            clients.remove(&self.ip);
        }
    }
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

    fn clients(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Client>> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A connection from `ip`, unless it has too many open.
    fn connect(self: &Arc<Self>, ip: IpAddr) -> Option<Connected> {
        let mut clients = self.clients();
        if clients.len() >= MAX_TRACKED {
            clients.retain(|_, client| client.connections > 0);
        }
        let client = clients.entry(ip).or_insert_with(|| Client {
            connections: 0,
            allowance: f64::from(self.config.sessions_per_minute),
            refilled: Instant::now(),
        });
        if client.connections >= self.config.connections_per_ip {
            return None;
        }
        client.connections += 1;
        Some(Connected {
            relay: self.clone(),
            ip,
        })
    }

    /// Whether the address of `connected` may create or join another session now.
    fn attempt(&self, connected: &Connected) -> bool {
        let mut clients = self.clients();
        let Some(client) = clients.get_mut(&connected.ip) else {
            return false;
        };
        client.refill(self.config.sessions_per_minute);
        if client.allowance < 1.0 {
            return false;
        }
        client.allowance -= 1.0;
        true
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

async fn upgrade(
    ws: WebSocketUpgrade,
    State(relay): State<Arc<Relay>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let connected = relay.connect(client_ip(&relay.config, &headers, addr));
    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |ws| async move {
            let ws = Socket::new(ws);
            match connected {
                Some(connected) => connection(ws, relay, connected).await,
                None => ws.fail(Error::Busy).await,
            }
        })
}

/// The client's address, an IPv6 one by its /64, which one host usually has all of.
fn client_ip(config: &Config, headers: &HeaderMap, addr: SocketAddr) -> IpAddr {
    let ip = (config.ip_header.as_ref())
        .and_then(|name| headers.get(name)?.to_str().ok()?.trim().parse().ok())
        .unwrap_or(addr.ip());
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !u128::from(u64::MAX))),
        },
        v4 => v4,
    }
}

async fn connection(mut ws: Socket, relay: Arc<Relay>, connected: Connected) {
    let text = match timeout(FIRST_MESSAGE_TIMEOUT, ws.recv()).await {
        Ok(Some(Frame::Text(text))) => text,
        Ok(Some(Frame::Binary(_))) => return ws.fail(Error::Protocol).await,
        Ok(None) | Err(_) => return,
    };
    if !has_supported_version(&text) {
        return ws.fail(Error::Version).await;
    }
    let message = serde_json::from_str::<ClientMessage>(&text);
    if message.is_ok() && !relay.attempt(&connected) {
        return ws.fail(Error::Busy).await;
    }
    match message {
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
