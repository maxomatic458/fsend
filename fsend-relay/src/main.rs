use std::{net::SocketAddr, sync::Arc, time::{Duration, Instant}};
use axum::{extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}}, response::IntoResponse, routing::get, Router};
use clap::Parser;
use dashmap::DashMap;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;
use tracing::info;

// --- Protocol types ---

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

// --- Client -> Server messages ---

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

// --- Server -> Client messages ---

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

// --- Negotiation ---

fn negotiate_protocol(sender: &[Protocol], receiver: &[Protocol]) -> Option<Protocol> {
    if sender.contains(&Protocol::Iroh) && receiver.contains(&Protocol::Iroh) {
        Some(Protocol::Iroh)
    } else if sender.contains(&Protocol::WebRtc) && receiver.contains(&Protocol::WebRtc) {
        Some(Protocol::WebRtc)
    } else {
        None
    }
}

// --- Session state ---

/// A waiting session: the sender is connected and waiting for a receiver.
struct Session {
    sender_capabilities: Vec<Protocol>,
    /// Channel for the receiver to send itself to the sender's task.
    /// Carries (negotiated protocol, receiver's exchange tx, receiver's exchange rx).
    receiver_join_tx: oneshot::Sender<ReceiverJoin>,
    created_at: Instant,
    expires_at: Instant,
}

struct ReceiverJoin {
    protocol: Protocol,
    /// Sender reads the receiver's ConnectionInfo from here.
    receiver_info_rx: oneshot::Receiver<ConnectionInfo>,
    /// Sender writes its ConnectionInfo here for the receiver.
    sender_info_tx: oneshot::Sender<ConnectionInfo>,
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<DashMap<String, Session>>,
}

impl AppState {
    fn new() -> Self {
        AppState {
            sessions: Arc::new(DashMap::new()),
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

    fn cleanup_expired_sessions(&self) {
        let now = Instant::now();
        self.sessions.retain(|_, session| session.expires_at > now);
    }
}

// --- Helpers ---

fn send_server_msg(msg: &ServerMessage) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap().into())
}

fn send_error(message: impl Into<String>) -> Message {
    send_server_msg(&ServerMessage::Error(ErrorAnswer { message: message.into() }))
}

/// Read the next text message from the socket, ignoring pings/pongs/close.
async fn recv_client_msg(ws: &mut WebSocket) -> Option<ClientMessage> {
    loop {
        match ws.recv().await? {
            Ok(Message::Text(text)) => {
                return serde_json::from_str(&text).ok();
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            _ => continue, // ignore ping/pong/binary
        }
    }
}

// --- WebSocket handler ---

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut ws: WebSocket, state: AppState) {
    // First message determines the role: CreateSession (sender) or JoinSession (receiver).
    let msg = match recv_client_msg(&mut ws).await {
        Some(m) => m,
        None => return,
    };

    match msg {
        ClientMessage::CreateSession(req) => handle_sender(ws, state, req).await,
        ClientMessage::JoinSession(req) => handle_receiver(ws, state, req).await,
        ClientMessage::Exchange(_) => {
            let _ = ws.send(send_error("unexpected Exchange before session setup")).await;
        }
    }
}

async fn handle_sender(mut ws: WebSocket, state: AppState, req: CreateSessionRequest) {
    let code = state.generate_code();
    let (receiver_join_tx, receiver_join_rx) = oneshot::channel::<ReceiverJoin>();

    let now = Instant::now();
    state.sessions.insert(code.clone(), Session {
        sender_capabilities: req.capabilities,
        receiver_join_tx,
        created_at: now,
        expires_at: now + Duration::from_secs(SESSION_DURATION_SECONDS),
    });

    // Tell the sender their code.
    if ws.send(send_server_msg(&ServerMessage::CreateSession(CreateSessionAnswer {
        code: code.clone(),
    }))).await.is_err() {
        state.sessions.remove(&code);
        return;
    }

    info!(code = %code, "session created, waiting for receiver");

    // Wait for a receiver to join (or timeout / disconnect).
    let join = tokio::select! {
        join = receiver_join_rx => {
            match join {
                Ok(j) => j,
                Err(_) => return, // channel dropped (session was cleaned up)
            }
        }
        // Also watch for the sender disconnecting while waiting.
        msg = recv_client_msg(&mut ws) => {
            // Sender sent something unexpected or disconnected.
            if msg.is_none() {
                state.sessions.remove(&code);
            }
            return;
        }
    };

    // Session consumed from the map by the receiver; notify the sender of the negotiated protocol.
    if ws.send(send_server_msg(&ServerMessage::PeerJoined(PeerJoinedAnswer {
        protocol: join.protocol,
    }))).await.is_err() {
        return;
    }

    // Wait for the sender's ExchangeRequest.
    let sender_info = match recv_client_msg(&mut ws).await {
        Some(ClientMessage::Exchange(ex)) => ex.connection_info,
        _ => return,
    };

    // Send our info to the receiver and get theirs.
    let _ = join.sender_info_tx.send(sender_info);

    let receiver_info = match join.receiver_info_rx.await {
        Ok(info) => info,
        Err(_) => return,
    };

    // Forward the receiver's connection info to the sender.
    let _ = ws.send(send_server_msg(&ServerMessage::Exchange(ExchangeAnswer {
        connection_info: receiver_info,
    }))).await;

    info!(code = %code, "exchange complete (sender side)");
}

async fn handle_receiver(mut ws: WebSocket, state: AppState, req: JoinSessionRequest) {
    // Look up and remove the session.
    let session = match state.sessions.remove(&req.code) {
        Some((_, s)) => s,
        None => {
            let _ = ws.send(send_error("session not found")).await;
            return;
        }
    };

    // Negotiate protocol.
    let protocol = match negotiate_protocol(&session.sender_capabilities, &req.capabilities) {
        Some(p) => p,
        None => {
            let _ = ws.send(send_error("no compatible protocol")).await;
            return;
        }
    };

    // Set up exchange channels.
    let (receiver_info_tx, receiver_info_rx) = oneshot::channel::<ConnectionInfo>();
    let (sender_info_tx, sender_info_rx) = oneshot::channel::<ConnectionInfo>();

    // Notify the sender's task that we've joined.
    if session.receiver_join_tx.send(ReceiverJoin {
        protocol,
        receiver_info_rx,
        sender_info_tx,
    }).is_err() {
        let _ = ws.send(send_error("sender disconnected")).await;
        return;
    }

    // Tell the receiver the negotiated protocol.
    if ws.send(send_server_msg(&ServerMessage::JoinSession(JoinSessionAnswer {
        protocol,
    }))).await.is_err() {
        return;
    }

    info!(code = %req.code, ?protocol, "receiver joined, exchanging");

    // First, get the sender's info and forward it to the receiver.
    // This allows the receiver to see the sender's info (e.g. WebRTC offer)
    // before generating its own response (e.g. WebRTC answer).
    let sender_info = match sender_info_rx.await {
        Ok(info) => info,
        Err(_) => return,
    };

    if ws.send(send_server_msg(&ServerMessage::Exchange(ExchangeAnswer {
        connection_info: sender_info,
    }))).await.is_err() {
        return;
    }

    // Now wait for the receiver's ExchangeRequest (response).
    let receiver_info = match recv_client_msg(&mut ws).await {
        Some(ClientMessage::Exchange(ex)) => ex.connection_info,
        _ => return,
    };

    // Forward receiver's info back to the sender.
    let _ = receiver_info_tx.send(receiver_info);

    info!(code = %req.code, "exchange complete (receiver side)");
}

// --- Constants ---

const SESSION_DURATION_SECONDS: u64 = 5 * 60;
const CLEANUP_INTERVAL_SECONDS: u64 = 5 * 60;
const CODE_LENGTH: usize = 8;
const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

#[derive(Parser)]
#[command(name = "fsend-relay")]
#[command(about = "A relay server for fsend.sh")]
struct Args {
    #[arg(default_value = "0.0.0.0:3001")]
    bind: SocketAddr,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let state = AppState::new();

    // Periodic cleanup of expired sessions.
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(CLEANUP_INTERVAL_SECONDS));
        loop {
            interval.tick().await;
            cleanup_state.cleanup_expired_sessions();
        }
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(args.bind).await.unwrap();
    info!("listening on {}", args.bind);
    axum::serve(listener, app).await.unwrap();
}
