use axum::{
    Router,
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use dashmap::DashMap;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::oneshot,
    time::{Instant, Interval, MissedTickBehavior, timeout},
};
use tower_http::cors::CorsLayer;
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Protocol {
    WebRtc,
    Iroh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum ConnectionInfo {
    WebRtc {
        sdp: String,
        ice_candidates: Vec<String>,
    },
    Iroh {
        node_id: String,
        addrs: Vec<String>,
    },
}

// Client -> Server
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

// Server -> Client

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateSessionAnswer {
    code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JoinSessionAnswer {
    protocol: Protocol,
}

/// Sent to the sender when a peer joins their session.
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

fn negotiate_protocol(sender: &[Protocol], receiver: &[Protocol]) -> Option<Protocol> {
    if sender.contains(&Protocol::Iroh) && receiver.contains(&Protocol::Iroh) {
        Some(Protocol::Iroh)
    } else if sender.contains(&Protocol::WebRtc) && receiver.contains(&Protocol::WebRtc) {
        Some(Protocol::WebRtc)
    } else {
        None
    }
}

struct Session {
    sender_capabilities: Vec<Protocol>,
    /// Channel for the receiver to send itself to the sender's task.
    receiver_join_tx: oneshot::Sender<ReceiverJoin>,
}

struct ReceiverJoin {
    protocol: Protocol,
    /// Sender reads the receiver's ConnectionInfo from here.
    receiver_info_rx: oneshot::Receiver<ConnectionInfo>,
    /// Sender writes its ConnectionInfo here for the receiver.
    sender_info_tx: oneshot::Sender<ConnectionInfo>,
}

#[derive(Clone)]
struct Config {
    /// Keeps proxies from dropping a client that is waiting on its peer.
    ping_interval: Duration,
    /// How long a new client may take to create or join a session.
    hello_timeout: Duration,
    /// How long a code stays valid.
    session_ttl: Duration,
    /// How long joined peers may take to swap connection info.
    exchange_timeout: Duration,
    max_sessions: usize,
    max_connections_per_ip: usize,
    max_message_bytes: usize,
    /// Header holding the client IP when behind a proxy.
    ip_header: Option<HeaderName>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(30),
            hello_timeout: Duration::from_secs(10),
            session_ttl: Duration::from_secs(5 * 60),
            exchange_timeout: Duration::from_secs(60),
            max_sessions: 10_000,
            max_connections_per_ip: 20,
            max_message_bytes: 64 * 1024,
            ip_header: None,
        }
    }
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<DashMap<String, Session>>,
    connections: Arc<DashMap<IpAddr, usize>>,
    config: Arc<Config>,
}

impl AppState {
    fn new(config: Config) -> Self {
        AppState {
            sessions: Arc::new(DashMap::new()),
            connections: Arc::new(DashMap::new()),
            config: Arc::new(config),
        }
    }

    fn generate_code(&self) -> String {
        let mut rng = rand::rng();
        loop {
            let code: String = (0..CODE_LENGTH)
                .map(|_| {
                    let idx = rng.random_range(0..CHARSET.len());
                    CHARSET[idx] as char
                })
                .collect();
            if !self.sessions.contains_key(&code) {
                return code;
            }
        }
    }
}

/// One of an IP's connections, given back on drop.
struct ConnectionSlot {
    connections: Arc<DashMap<IpAddr, usize>>,
    ip: IpAddr,
}

impl ConnectionSlot {
    fn acquire(state: &AppState, ip: IpAddr) -> Option<Self> {
        let mut count = state.connections.entry(ip).or_insert(0);
        if *count >= state.config.max_connections_per_ip {
            return None;
        }
        *count += 1;
        Some(Self {
            connections: state.connections.clone(),
            ip,
        })
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.connections.remove_if_mut(&self.ip, |_, count| {
            *count -= 1;
            *count == 0
        });
    }
}

fn client_ip(header: Option<&HeaderName>, headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    header
        .and_then(|name| headers.get(name)?.to_str().ok()?.trim().parse().ok())
        .unwrap_or(peer.ip())
}

/// A WebSocket that is pinged while the relay waits on it.
struct Client {
    ws: WebSocket,
    ping: Interval,
}

impl Client {
    fn new(ws: WebSocket, ping_interval: Duration) -> Self {
        let mut ping = tokio::time::interval_at(Instant::now() + ping_interval, ping_interval);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self { ws, ping }
    }

    async fn send(&mut self, msg: &ServerMessage) -> Option<()> {
        let text = serde_json::to_string(msg).ok()?;
        self.ws.send(Message::Text(text.into())).await.ok()
    }

    async fn send_error(&mut self, message: &str) {
        let error = ServerMessage::Error(ErrorAnswer {
            message: message.into(),
        });
        let _ = self.send(&error).await;
    }

    /// The next message, or `None` once the client is gone or sends garbage.
    async fn recv(&mut self) -> Option<ClientMessage> {
        loop {
            tokio::select! {
                msg = self.ws.recv() => match msg? {
                    Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
                    Ok(Message::Close(_)) | Err(_) => return None,
                    Ok(_) => {}
                },
                _ = self.ping.tick() => self.ws.send(Message::Ping(Default::default())).await.ok()?,
            }
        }
    }

    /// Awaits `fut`, or `None` if the client leaves or speaks out of turn first.
    async fn wait<T>(&mut self, fut: impl Future<Output = T>) -> Option<T> {
        tokio::select! {
            out = fut => Some(out),
            _ = self.recv() => None,
        }
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let ip = client_ip(state.config.ip_header.as_ref(), &headers, peer);
    let Some(slot) = ConnectionSlot::acquire(&state, ip) else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let limit = state.config.max_message_bytes;
    ws.max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| async move {
            let _slot = slot;
            handle_socket(socket, state).await
        })
}

async fn handle_socket(ws: WebSocket, state: AppState) {
    let mut client = Client::new(ws, state.config.ping_interval);

    // First message determines the role: CreateSession (sender) or JoinSession (receiver).
    match timeout(state.config.hello_timeout, client.recv()).await {
        Ok(Some(ClientMessage::CreateSession(req))) => handle_sender(client, state, req).await,
        Ok(Some(ClientMessage::JoinSession(req))) => handle_receiver(client, state, req).await,
        Ok(Some(ClientMessage::Exchange(_))) => {
            client
                .send_error("unexpected Exchange before session setup")
                .await
        }
        Ok(None) | Err(_) => {}
    }
}

async fn handle_sender(mut client: Client, state: AppState, req: CreateSessionRequest) {
    if state.sessions.len() >= state.config.max_sessions {
        return client
            .send_error("the relay is busy, try again later")
            .await;
    }

    let code = state.generate_code();
    let (receiver_join_tx, receiver_join_rx) = oneshot::channel();
    state.sessions.insert(
        code.clone(),
        Session {
            sender_capabilities: req.capabilities,
            receiver_join_tx,
        },
    );

    let Some(join) = wait_for_receiver(&mut client, &state.config, &code, receiver_join_rx).await
    else {
        state.sessions.remove(&code);
        return;
    };

    let exchange = sender_exchange(&mut client, join);
    if let Ok(Some(())) = timeout(state.config.exchange_timeout, exchange).await {
        info!(code = %code, "exchange complete (sender side)");
    }
}

/// Hands the sender its code and waits for a receiver to join, at most until the code expires.
async fn wait_for_receiver(
    client: &mut Client,
    config: &Config,
    code: &str,
    receiver_join_rx: oneshot::Receiver<ReceiverJoin>,
) -> Option<ReceiverJoin> {
    let created = ServerMessage::CreateSession(CreateSessionAnswer { code: code.into() });
    client.send(&created).await?;
    info!(code = %code, "session created, waiting for receiver");

    match timeout(config.session_ttl, client.wait(receiver_join_rx)).await {
        Ok(joined) => joined?.ok(),
        Err(_) => {
            client.send_error("session expired").await;
            None
        }
    }
}

async fn sender_exchange(client: &mut Client, join: ReceiverJoin) -> Option<()> {
    let joined = ServerMessage::PeerJoined(PeerJoinedAnswer {
        protocol: join.protocol,
    });
    client.send(&joined).await?;

    let Some(ClientMessage::Exchange(ex)) = client.recv().await else {
        return None;
    };
    join.sender_info_tx.send(ex.connection_info).ok()?;

    let receiver_info = client.wait(join.receiver_info_rx).await?.ok()?;
    client
        .send(&ServerMessage::Exchange(ExchangeAnswer {
            connection_info: receiver_info,
        }))
        .await
}

async fn handle_receiver(mut client: Client, state: AppState, req: JoinSessionRequest) {
    let Some((_, session)) = state.sessions.remove(&req.code) else {
        return client.send_error("session not found").await;
    };
    let Some(protocol) = negotiate_protocol(&session.sender_capabilities, &req.capabilities) else {
        return client.send_error("no compatible protocol").await;
    };

    let (receiver_info_tx, receiver_info_rx) = oneshot::channel();
    let (sender_info_tx, sender_info_rx) = oneshot::channel();
    let join = ReceiverJoin {
        protocol,
        receiver_info_rx,
        sender_info_tx,
    };
    if session.receiver_join_tx.send(join).is_err() {
        return client.send_error("sender disconnected").await;
    }
    info!(code = %req.code, ?protocol, "receiver joined, exchanging");

    let exchange = receiver_exchange(&mut client, protocol, sender_info_rx, receiver_info_tx);
    if let Ok(Some(())) = timeout(state.config.exchange_timeout, exchange).await {
        info!(code = %req.code, "exchange complete (receiver side)");
    }
}

async fn receiver_exchange(
    client: &mut Client,
    protocol: Protocol,
    sender_info_rx: oneshot::Receiver<ConnectionInfo>,
    receiver_info_tx: oneshot::Sender<ConnectionInfo>,
) -> Option<()> {
    client
        .send(&ServerMessage::JoinSession(JoinSessionAnswer { protocol }))
        .await?;

    let sender_info = client.wait(sender_info_rx).await?.ok()?;
    client
        .send(&ServerMessage::Exchange(ExchangeAnswer {
            connection_info: sender_info,
        }))
        .await?;

    let Some(ClientMessage::Exchange(ex)) = client.recv().await else {
        return None;
    };
    receiver_info_tx.send(ex.connection_info).ok()
}

fn app(config: Config) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(AppState::new(config))
}

const CODE_LENGTH: usize = 8;
const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

#[derive(Parser)]
#[command(name = "fsend-relay")]
#[command(about = "A relay server for fsend.sh")]
struct Args {
    #[arg(default_value = "0.0.0.0:3001")]
    bind: SocketAddr,

    /// Header holding the client IP when behind a proxy, e.g. cf-connecting-ip.
    #[arg(long)]
    ip_header: Option<HeaderName>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let config = Config {
        ip_header: args.ip_header,
        ..Config::default()
    };

    let listener = tokio::net::TcpListener::bind(args.bind).await.unwrap();
    info!("listening on {}", args.bind);
    let service = app(config).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, service).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::future::IntoFuture;
    use tokio_tungstenite::{connect_async, tungstenite};

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    fn quick() -> Config {
        Config {
            ping_interval: Duration::from_millis(50),
            hello_timeout: Duration::from_millis(200),
            session_ttl: Duration::from_millis(300),
            exchange_timeout: Duration::from_millis(300),
            ..Config::default()
        }
    }

    async fn serve(config: Config) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let service = app(config).into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(axum::serve(listener, service).into_future());
        url
    }

    async fn connect(url: &str) -> Ws {
        connect_async(url).await.unwrap().0
    }

    async fn send(ws: &mut Ws, msg: Value) {
        let text = tungstenite::Message::Text(msg.to_string().into());
        ws.send(text).await.unwrap();
    }

    /// The next JSON message, or `None` once the relay hangs up.
    async fn next_json(ws: &mut Ws) -> Option<Value> {
        let next = async {
            loop {
                match ws.next().await? {
                    Ok(tungstenite::Message::Text(text)) => {
                        return serde_json::from_str(&text).ok();
                    }
                    Ok(tungstenite::Message::Close(_)) | Err(_) => return None,
                    Ok(_) => {}
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(2), next)
            .await
            .expect("the relay went quiet")
    }

    async fn create(ws: &mut Ws) -> String {
        send(
            ws,
            json!({"type": "create_session", "capabilities": ["web_rtc"]}),
        )
        .await;
        let created = next_json(ws).await.unwrap();
        created["code"].as_str().unwrap().to_owned()
    }

    async fn join(ws: &mut Ws, code: &str) -> Value {
        let msg = json!({"type": "join_session", "code": code, "capabilities": ["web_rtc"]});
        send(ws, msg).await;
        next_json(ws).await.unwrap()
    }

    fn exchange(sdp: &str) -> Value {
        json!({
            "type": "exchange",
            "connection_info": {"type": "web_rtc", "sdp": sdp, "ice_candidates": []},
        })
    }

    #[tokio::test]
    async fn peers_swap_connection_info() {
        let url = serve(Config::default()).await;
        let mut sender = connect(&url).await;
        let code = create(&mut sender).await;
        let mut receiver = connect(&url).await;

        assert_eq!(join(&mut receiver, &code).await["type"], "join_session");
        assert_eq!(next_json(&mut sender).await.unwrap()["type"], "peer_joined");
        send(&mut sender, exchange("offer")).await;
        let offer = next_json(&mut receiver).await.unwrap();
        assert_eq!(offer["connection_info"]["sdp"], "offer");
        send(&mut receiver, exchange("answer")).await;
        let answer = next_json(&mut sender).await.unwrap();
        assert_eq!(answer["connection_info"]["sdp"], "answer");
    }

    #[tokio::test]
    async fn waiting_senders_are_pinged() {
        let url = serve(quick()).await;
        let mut sender = connect(&url).await;
        create(&mut sender).await;

        let frame = tokio::time::timeout(Duration::from_secs(1), sender.next())
            .await
            .expect("no ping while waiting")
            .unwrap()
            .unwrap();
        assert!(matches!(frame, tungstenite::Message::Ping(_)), "{frame:?}");
    }

    #[tokio::test]
    async fn codes_expire_on_time() {
        let url = serve(quick()).await;
        let mut sender = connect(&url).await;
        let code = create(&mut sender).await;

        let started = std::time::Instant::now();
        assert_eq!(
            next_json(&mut sender).await.unwrap()["message"],
            "session expired"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(next_json(&mut sender).await.is_none());

        let mut receiver = connect(&url).await;
        assert_eq!(
            join(&mut receiver, &code).await["message"],
            "session not found"
        );
    }

    #[tokio::test]
    async fn silent_clients_are_dropped() {
        let url = serve(quick()).await;
        let mut ws = connect(&url).await;
        assert_eq!(next_json(&mut ws).await, None);
    }

    #[tokio::test]
    async fn a_stalled_exchange_is_cut_off() {
        let url = serve(quick()).await;
        let mut sender = connect(&url).await;
        let code = create(&mut sender).await;
        let mut receiver = connect(&url).await;
        join(&mut receiver, &code).await;
        next_json(&mut sender).await.unwrap();

        // Neither side sends its connection info.
        assert_eq!(next_json(&mut sender).await, None);
        assert_eq!(next_json(&mut receiver).await, None);
    }

    #[tokio::test]
    async fn oversized_messages_close_the_connection() {
        let url = serve(Config {
            max_message_bytes: 1024,
            ..quick()
        })
        .await;
        let mut ws = connect(&url).await;
        let padding = "x".repeat(2048);
        let msg =
            json!({"type": "create_session", "capabilities": ["web_rtc"], "padding": padding});
        send(&mut ws, msg).await;
        assert_eq!(next_json(&mut ws).await, None);
    }

    #[tokio::test]
    async fn sessions_are_capped() {
        let url = serve(Config {
            max_sessions: 1,
            ..Config::default()
        })
        .await;
        let mut first = connect(&url).await;
        create(&mut first).await;

        let mut second = connect(&url).await;
        send(
            &mut second,
            json!({"type": "create_session", "capabilities": ["web_rtc"]}),
        )
        .await;
        let busy = next_json(&mut second).await.unwrap();
        assert_eq!(busy["message"], "the relay is busy, try again later");
    }

    #[tokio::test]
    async fn connections_per_ip_are_capped() {
        let url = serve(Config {
            max_connections_per_ip: 2,
            ..Config::default()
        })
        .await;
        let first = connect(&url).await;
        let _second = connect(&url).await;

        let refused = connect_async(&url).await.unwrap_err();
        assert!(
            matches!(&refused, tungstenite::Error::Http(r) if r.status() == 429),
            "{refused}"
        );

        drop(first);
        let reconnected = async {
            while connect_async(&url).await.is_err() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(1), reconnected)
            .await
            .expect("a closed connection frees its slot");
    }

    #[test]
    fn the_client_ip_comes_from_the_configured_header() {
        let peer: SocketAddr = "10.0.0.1:1234".parse().unwrap();
        let header = HeaderName::from_static("cf-connecting-ip");
        let mut headers = HeaderMap::new();
        headers.insert(&header, "203.0.113.7".parse().unwrap());

        assert_eq!(
            client_ip(Some(&header), &headers, peer).to_string(),
            "203.0.113.7"
        );
        assert_eq!(
            client_ip(None, &headers, peer),
            peer.ip(),
            "only when configured"
        );
        assert_eq!(client_ip(Some(&header), &HeaderMap::new(), peer), peer.ip());

        headers.insert(&header, "not an ip".parse().unwrap());
        assert_eq!(client_ip(Some(&header), &headers, peer), peer.ip());
    }
}
