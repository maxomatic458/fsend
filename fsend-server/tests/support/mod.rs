// Each test file uses only some of these.
#![allow(dead_code)]

use std::{net::SocketAddr, time::Duration};

use fsend_proto::relay::VERSION;
use fsend_server::Config;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

/// The address of a server started with `config`.
pub async fn start_at(config: Config) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(fsend_server::serve(listener, config));
    addr
}

/// The relay URL of a server started with `config`.
pub async fn start(config: Config) -> String {
    format!("ws://{}/relay", start_at(config).await)
}

pub struct Client(pub WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>);

impl Client {
    pub async fn connect(url: &str) -> Self {
        Self(connect_async(url).await.unwrap().0)
    }

    /// Connects as Cloudflare would for a client at `ip`.
    pub async fn connect_as(url: &str, ip: &str) -> Self {
        let mut request = url.into_client_request().unwrap();
        (request.headers_mut()).insert("cf-connecting-ip", ip.parse().unwrap());
        Self(connect_async(request).await.unwrap().0)
    }

    /// Sends `create`; the relay answers `created` or an error.
    pub async fn try_create(&mut self) -> Value {
        self.send(json!({"type": "create", "version": VERSION, "transports": ["webrtc"]}))
            .await;
        self.recv().await
    }

    pub async fn send(&mut self, message: Value) {
        self.0
            .send(Message::text(message.to_string()))
            .await
            .unwrap();
    }

    pub async fn send_binary(&mut self, data: &[u8]) {
        self.0.send(Message::binary(data.to_vec())).await.unwrap();
    }

    /// The next text or binary frame, or `None` once the relay closed.
    pub async fn frame(&mut self) -> Option<Message> {
        loop {
            let frame = timeout(Duration::from_secs(5), self.0.next())
                .await
                .expect("the relay should answer");
            match frame {
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                Some(Ok(other)) => return Some(other),
            }
        }
    }

    /// The next JSON frame, or `None` once the relay closed.
    pub async fn next(&mut self) -> Option<Value> {
        match self.frame().await? {
            Message::Text(text) => Some(serde_json::from_str(&text).unwrap()),
            other => panic!("expected a text frame, got {other:?}"),
        }
    }

    pub async fn recv(&mut self) -> Value {
        self.next().await.expect("a frame, not a close")
    }

    pub async fn recv_binary(&mut self) -> Vec<u8> {
        match self.frame().await {
            Some(Message::Binary(data)) => data.to_vec(),
            other => panic!("expected a binary frame, got {other:?}"),
        }
    }

    /// Asserts the relay answers with `code` and closes.
    pub async fn expect_error(&mut self, code: &str) {
        assert_eq!(self.recv().await, json!({"type": "error", "code": code}));
        assert_eq!(self.next().await, None, "the relay closes after an error");
    }

    pub async fn create(url: &str, transports: &[&str]) -> (Self, String) {
        let mut client = Self::connect(url).await;
        client
            .send(json!({"type": "create", "version": VERSION, "transports": transports}))
            .await;
        let created = client.recv().await;
        assert_eq!(created["type"], "created", "{created}");
        let nameplate = created["nameplate"].as_str().unwrap().to_owned();
        (client, nameplate)
    }

    pub async fn join(url: &str, nameplate: &str, transports: &[&str]) -> Self {
        let mut client = Self::connect(url).await;
        client
            .send(json!({"type": "join", "version": VERSION, "nameplate": nameplate, "transports": transports}))
            .await;
        client
    }
}

pub async fn paired(url: &str) -> (Client, Client) {
    let (mut sender, nameplate) = Client::create(url, &["iroh", "webrtc"]).await;
    let mut receiver = Client::join(url, &nameplate, &["webrtc", "iroh"]).await;
    let expected = json!({"type": "paired", "transport": "iroh"});
    assert_eq!(sender.recv().await, expected);
    assert_eq!(receiver.recv().await, expected);
    (sender, receiver)
}
