use std::time::Duration;

use fsend_relay::{Config, MAX_MESSAGE_BYTES, MAX_SIGNALS};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error, Message},
};

async fn start(config: Config) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/relay", listener.local_addr().unwrap());
    tokio::spawn(fsend_relay::serve(listener, config));
    url
}

struct Client(WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>);

impl Client {
    async fn connect(url: &str) -> Self {
        Self(connect_async(url).await.unwrap().0)
    }

    async fn send(&mut self, message: Value) {
        self.0
            .send(Message::text(message.to_string()))
            .await
            .unwrap();
    }

    async fn send_binary(&mut self, data: &[u8]) {
        self.0.send(Message::binary(data.to_vec())).await.unwrap();
    }

    /// The next text or binary frame, or `None` once the relay closed.
    async fn frame(&mut self) -> Option<Message> {
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
    async fn next(&mut self) -> Option<Value> {
        match self.frame().await? {
            Message::Text(text) => Some(serde_json::from_str(&text).unwrap()),
            other => panic!("expected a text frame, got {other:?}"),
        }
    }

    async fn recv(&mut self) -> Value {
        self.next().await.expect("a frame, not a close")
    }

    async fn recv_binary(&mut self) -> Vec<u8> {
        match self.frame().await {
            Some(Message::Binary(data)) => data.to_vec(),
            other => panic!("expected a binary frame, got {other:?}"),
        }
    }

    /// Asserts the relay answers with `code` and closes.
    async fn expect_error(&mut self, code: &str) {
        let frame = self.recv().await;
        assert_eq!(
            (frame["type"].as_str(), frame["code"].as_str()),
            (Some("error"), Some(code)),
            "{frame}"
        );
        assert_eq!(self.next().await, None, "the relay closes after an error");
    }

    async fn create(url: &str, transports: &[&str]) -> (Self, String) {
        let mut client = Self::connect(url).await;
        client
            .send(json!({"type": "create", "version": 1, "transports": transports}))
            .await;
        let created = client.recv().await;
        assert_eq!(created["type"], "created", "{created}");
        let nameplate = created["nameplate"].as_str().unwrap().to_owned();
        (client, nameplate)
    }

    async fn join(url: &str, nameplate: &str, transports: &[&str]) -> Self {
        let mut client = Self::connect(url).await;
        client
            .send(json!({"type": "join", "version": 1, "nameplate": nameplate, "transports": transports}))
            .await;
        client
    }
}

async fn paired(url: &str) -> (Client, Client) {
    let (mut sender, nameplate) = Client::create(url, &["iroh", "webrtc"]).await;
    let mut receiver = Client::join(url, &nameplate, &["webrtc", "iroh"]).await;
    let expected = json!({"type": "paired", "transport": "iroh"});
    assert_eq!(sender.recv().await, expected);
    assert_eq!(receiver.recv().await, expected);
    (sender, receiver)
}

#[tokio::test]
async fn pairs_and_forwards_signals_both_ways() {
    let url = start(Config::default()).await;
    let (mut sender, nameplate) = Client::create(&url, &["iroh", "webrtc"]).await;
    assert_eq!(nameplate.len(), 4);
    let mut receiver = Client::join(&url, &nameplate, &["webrtc", "iroh"]).await;
    // The sender's preference wins.
    let expected = json!({"type": "paired", "transport": "iroh"});
    assert_eq!(sender.recv().await, expected);
    assert_eq!(receiver.recv().await, expected);

    sender.send_binary(&[0, 1]).await;
    receiver.send_binary(&[1, 2, 3]).await;
    // Forwarded byte for byte.
    assert_eq!(receiver.recv_binary().await, [0, 1]);
    assert_eq!(sender.recv_binary().await, [1, 2, 3]);
}

#[tokio::test]
async fn sessions_report_their_lifetime() {
    let url = start(Config::default()).await;
    let mut client = Client::connect(&url).await;
    client
        .send(json!({"type": "create", "version": 1, "transports": ["webrtc"]}))
        .await;
    let created = client.recv().await;
    assert_eq!(created["expires_in"], 300);
}

#[tokio::test]
async fn an_unknown_nameplate_is_not_found() {
    let url = start(Config::default()).await;
    Client::join(&url, "ZZZZ", &["webrtc"])
        .await
        .expect_error("not_found")
        .await;
}

#[tokio::test]
async fn a_nameplate_takes_one_join_attempt() {
    let url = start(Config::default()).await;
    let (_sender, receiver) = paired(&url).await;
    drop(receiver);

    let (mut sender, nameplate) = Client::create(&url, &["iroh"]).await;
    Client::join(&url, &nameplate, &["webrtc"])
        .await
        .expect_error("no_common_transport")
        .await;
    sender.expect_error("no_common_transport").await;
    // Even a failed attempt consumes the nameplate.
    Client::join(&url, &nameplate, &["iroh"])
        .await
        .expect_error("not_found")
        .await;
}

#[tokio::test]
async fn the_version_is_checked_first() {
    let url = start(Config::default()).await;
    let frames = [
        json!({"type": "create", "transports": ["webrtc"]}),
        json!({"type": "create", "version": 2, "transports": ["webrtc"]}),
        json!({"type": "join", "version": "1", "nameplate": "ZZZZ", "transports": []}),
        json!({"type": "signal", "data": ""}),
        json!({"type": "hello"}),
    ];
    for frame in frames {
        let mut client = Client::connect(&url).await;
        client.send(frame).await;
        client.expect_error("version").await;
    }
}

#[tokio::test]
async fn malformed_first_messages_are_protocol_errors() {
    let url = start(Config::default()).await;
    for frame in [
        json!({"type": "signal", "version": 1, "data": ""}),
        json!({"type": "create", "version": 1}),
    ] {
        let mut client = Client::connect(&url).await;
        client.send(frame).await;
        client.expect_error("protocol").await;
    }
    let mut client = Client::connect(&url).await;
    client.send_binary(&[0, 1]).await;
    client.expect_error("protocol").await;
}

#[tokio::test]
async fn unknown_transports_are_protocol_errors() {
    let url = start(Config::default()).await;
    let mut client = Client::connect(&url).await;
    client
        .send(json!({"type": "create", "version": 1, "transports": ["webrtc", "quic"]}))
        .await;
    client.expect_error("protocol").await;

    let (_sender, nameplate) = Client::create(&url, &["webrtc"]).await;
    Client::join(&url, &nameplate, &["quic"])
        .await
        .expect_error("protocol")
        .await;
}

#[tokio::test]
async fn signals_before_pairing_are_rejected() {
    let url = start(Config::default()).await;
    let (mut sender, nameplate) = Client::create(&url, &["webrtc"]).await;
    sender.send_binary(&[0, 1]).await;
    sender.expect_error("protocol").await;
    // The session went with the sender.
    Client::join(&url, &nameplate, &["webrtc"])
        .await
        .expect_error("not_found")
        .await;

    let (mut sender, _) = Client::create(&url, &["webrtc"]).await;
    sender.send(json!({"type": "signal", "data": "AA=="})).await;
    sender.expect_error("protocol").await;
}

#[tokio::test]
async fn text_frames_after_pairing_are_rejected() {
    let url = start(Config::default()).await;
    let (mut sender, mut receiver) = paired(&url).await;
    sender.send(json!({"type": "signal", "data": "AA=="})).await;
    sender.expect_error("protocol").await;
    receiver.expect_error("peer_left").await;
}

#[tokio::test]
async fn unjoined_sessions_expire() {
    let url = start(Config {
        session_ttl: Duration::from_millis(100),
        ..Config::default()
    })
    .await;
    let (mut sender, nameplate) = Client::create(&url, &["webrtc"]).await;
    sender.expect_error("expired").await;
    Client::join(&url, &nameplate, &["webrtc"])
        .await
        .expect_error("expired")
        .await;
}

#[tokio::test]
async fn pairing_has_a_deadline() {
    let url = start(Config {
        pair_timeout: Duration::from_millis(100),
        ..Config::default()
    })
    .await;
    let (mut sender, mut receiver) = paired(&url).await;
    sender.expect_error("expired").await;
    receiver.expect_error("expired").await;
}

#[tokio::test]
async fn a_departing_peer_is_reported_after_its_signals() {
    let url = start(Config::default()).await;
    let (mut sender, mut receiver) = paired(&url).await;
    receiver.send_binary(&[1, 2]).await;
    receiver.0.close(None).await.unwrap();
    assert_eq!(sender.recv_binary().await, [1, 2]);
    sender.expect_error("peer_left").await;
}

#[tokio::test]
async fn a_sender_leaving_before_the_join_frees_the_nameplate() {
    let url = start(Config::default()).await;
    let (mut sender, nameplate) = Client::create(&url, &["webrtc"]).await;
    sender.0.close(None).await.unwrap();
    assert_eq!(sender.next().await, None);
    // The close handshake can finish before the relay drops the session.
    tokio::time::sleep(Duration::from_millis(100)).await;
    Client::join(&url, &nameplate, &["webrtc"])
        .await
        .expect_error("not_found")
        .await;
}

#[tokio::test]
async fn the_session_cap_answers_busy() {
    let url = start(Config {
        max_sessions: 1,
        ..Config::default()
    })
    .await;
    let (sender, _) = Client::create(&url, &["webrtc"]).await;
    let mut second = Client::connect(&url).await;
    second
        .send(json!({"type": "create", "version": 1, "transports": ["webrtc"]}))
        .await;
    second.expect_error("busy").await;

    // Capacity returns once the first session ends.
    drop(sender);
    tokio::time::sleep(Duration::from_millis(100)).await;
    Client::create(&url, &["webrtc"]).await;
}

#[tokio::test]
async fn signals_are_bounded_in_count_and_size() {
    let url = start(Config::default()).await;
    let (mut sender, mut receiver) = paired(&url).await;
    for _ in 0..=MAX_SIGNALS {
        sender.send_binary(&[1, 0]).await;
    }
    sender.expect_error("protocol").await;
    for _ in 0..MAX_SIGNALS {
        assert_eq!(receiver.recv_binary().await, [1, 0]);
    }
    receiver.expect_error("peer_left").await;

    let (mut sender, _receiver) = paired(&url).await;
    let sent = sender
        .0
        .send(Message::binary(vec![1; MAX_MESSAGE_BYTES + 1]))
        .await;
    // The relay drops the connection; the write itself may or may not notice.
    assert!(matches!(
        sent,
        Ok(()) | Err(Error::Io(_) | Error::ConnectionClosed | Error::Protocol(_))
    ));
    assert_eq!(sender.next().await, None);
}

#[tokio::test]
async fn the_relay_keeps_connections_alive_with_pings() {
    let url = start(Config {
        ping_interval: Duration::from_millis(50),
        ..Config::default()
    })
    .await;
    let (mut sender, _) = Client::create(&url, &["webrtc"]).await;
    let frame = timeout(Duration::from_secs(5), sender.0.next())
        .await
        .unwrap();
    assert!(matches!(frame, Some(Ok(Message::Ping(_)))), "{frame:?}");
}

#[tokio::test]
async fn a_sender_that_stops_answering_pings_loses_its_session() {
    let url = start(Config {
        ping_interval: Duration::from_millis(50),
        ..Config::default()
    })
    .await;
    // A client that reads answers pings; one that has gone quiet does not.
    let (mut alive, alive_plate) = Client::create(&url, &["webrtc"]).await;
    let (_gone, gone_plate) = Client::create(&url, &["webrtc"]).await;
    let reading = timeout(Duration::from_millis(400), alive.frame()).await;
    assert!(
        reading.is_err(),
        "the answering client is kept: {reading:?}"
    );

    Client::join(&url, &gone_plate, &["webrtc"])
        .await
        .expect_error("not_found")
        .await;
    let mut receiver = Client::join(&url, &alive_plate, &["webrtc"]).await;
    assert_eq!(receiver.recv().await["type"], "paired");
}
