use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use fsend_server::Config;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

mod support;
use support::{Client, start_at};

/// A server keeping stats in a fresh database, and that database's path.
async fn with_stats(dir: &Path) -> (SocketAddr, PathBuf) {
    with_stats_for(dir, Config::default().stats_token_ttl).await
}

/// The same, with tokens that last `ttl`.
async fn with_stats_for(dir: &Path, ttl: Duration) -> (SocketAddr, PathBuf) {
    let database = dir.join("stats.db");
    let config = Config {
        database: Some(database.clone()),
        stats_token_ttl: ttl,
        ..Config::default()
    };
    (start_at(config).await, database)
}

/// Pairs two clients; returns the token the receiver got.
async fn receiver_token(addr: SocketAddr) -> String {
    let url = format!("ws://{addr}/relay");
    let (mut sender, nameplate) = Client::create(&url, &["webrtc"]).await;
    let mut receiver = Client::join(&url, &nameplate, &["webrtc"]).await;
    assert_eq!(
        sender.recv().await,
        json!({"type": "paired", "transport": "webrtc"}),
        "the sender reports nothing, so it gets no token"
    );
    let paired = receiver.recv().await;
    assert_eq!(paired["type"], "paired");
    let token = paired["stats_token"].as_str().expect("a token").to_owned();
    assert!(token.len() == 32 && token.chars().all(|c| c.is_ascii_hexdigit()));
    token
}

/// The status and body of a plain HTTP request.
async fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: text/plain\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all((head + body).as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status = response[9..12].parse().unwrap();
    let body = response.split_once("\r\n\r\n").unwrap().1.to_owned();
    (status, body)
}

/// A browser's report of `bytes` over a direct WebRTC connection.
fn web_report(token: &str, bytes: i64) -> Value {
    json!({"token": token, "platform": "web", "transport": "webrtc", "route": "direct",
        "bytes": bytes})
}

async fn report(addr: SocketAddr, report: Value) -> u16 {
    request(addr, "POST", "/stats", &report.to_string()).await.0
}

/// The rows as `platform transport route bytes`, checking each was stamped with this hour.
fn rows(database: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open(database).unwrap();
    let mut rows = db
        .prepare(
            "SELECT hour, unixepoch() - unixepoch(hour), platform || ' ' || transport || ' ' || \
             route || ' ' || bytes FROM transfers",
        )
        .unwrap();
    rows.query_map([], |row| {
        let (hour, age): (String, i64) = (row.get(0)?, row.get(1)?);
        assert!(hour.ends_with(":00") && (0..3600).contains(&age), "{hour}");
        row.get(2)
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[tokio::test]
async fn each_token_counts_one_report() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, database) = with_stats(dir.path()).await;

    let first = receiver_token(addr).await;
    let second = receiver_token(addr).await;
    let third = receiver_token(addr).await;
    for (token, platform, transport, route, bytes) in [
        (&first, "cli", "iroh", "direct", 100),
        (&second, "cli", "webrtc", "relay", 23),
        (&third, "web", "webrtc", "unknown", 7),
    ] {
        let sent = json!({"token": token, "platform": platform, "transport": transport,
            "route": route, "bytes": bytes});
        assert_eq!(report(addr, sent).await, 204);
    }
    let again = web_report(&first, 100);
    assert_eq!(report(addr, again).await, 403, "a token is used up");

    assert_eq!(
        rows(&database),
        [
            "cli iroh direct 100",
            "cli webrtc relay 23",
            "web webrtc unknown 7"
        ]
    );
}

#[tokio::test]
async fn reports_without_a_valid_token_count_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, database) = with_stats(dir.path()).await;

    assert_eq!(report(addr, web_report(&"0".repeat(32), 1)).await, 403);
    let token = receiver_token(addr).await;
    for (field, value) in [
        ("platform", json!("gameboy")),
        ("transport", json!("pigeon")),
        ("route", json!("detour")),
        ("bytes", json!(-1)),
        ("bytes", Value::Null),
    ] {
        let mut malformed = web_report(&token, 1);
        malformed[field] = value;
        assert_eq!(report(addr, malformed).await, 400, "{field}");
    }
    let (status, _) = request(addr, "POST", "/stats", "not json").await;
    assert_eq!(status, 400);
    assert_eq!(rows(&database), Vec::<String>::new());

    // A malformed report leaves the token for a good one.
    assert_eq!(report(addr, web_report(&token, 1)).await, 204);
}

#[tokio::test]
async fn an_expired_token_counts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, database) = with_stats_for(dir.path(), Duration::ZERO).await;
    let token = receiver_token(addr).await;
    assert_eq!(report(addr, web_report(&token, 1)).await, 403);
    assert_eq!(rows(&database), Vec::<String>::new());
}

#[tokio::test]
async fn totals_count_the_hours_each_window_touches() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, database) = with_stats(dir.path()).await;
    // Answering means the server has made its tables.
    assert_eq!(request(addr, "GET", "/health", "").await.0, 200);
    let db = rusqlite::Connection::open(&database).unwrap();
    // Bytes stamped this many hours before the current hour.
    let rows = [
        (0, 1),
        (1, 2),
        (2, 4),
        (24, 8),
        (24 * 7, 16),
        (24 * 30, 32),
        (24 * 365, 64),
        (24 * 400, 128),
    ];
    for (hours, bytes) in rows {
        db.execute(
            "INSERT INTO transfers VALUES (strftime('%Y-%m-%d %H:00', 'now', ?1), 'cli', 'iroh', 'direct', ?2)",
            rusqlite::params![format!("-{hours} hours"), bytes],
        )
        .unwrap();
    }
    let hour_of_day: i64 = db
        .query_row("SELECT CAST(strftime('%H', 'now') AS INTEGER)", [], |row| {
            row.get(0)
        })
        .unwrap();
    let today: i64 = (rows.iter())
        .filter(|(hours, _)| *hours <= hour_of_day)
        .map(|(_, bytes)| bytes)
        .sum();

    let (status, body) = request(addr, "GET", "/transfer-stats", "").await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({
            // This hour and the one before, which the last 60 minutes reach into.
            "last_hour": 1 + 2,
            "today": today,
            "last_day": 1 + 2 + 4 + 8,
            "last_7_days": 1 + 2 + 4 + 8 + 16,
            "last_month": 1 + 2 + 4 + 8 + 16 + 32,
            "last_year": 1 + 2 + 4 + 8 + 16 + 32 + 64,
        })
    );
}

#[tokio::test]
async fn without_a_database_there_are_no_tokens_or_reports() {
    let addr = start_at(Config::default()).await;
    let url = format!("ws://{addr}/relay");
    let (mut sender, nameplate) = Client::create(&url, &["webrtc"]).await;
    let mut receiver = Client::join(&url, &nameplate, &["webrtc"]).await;
    let paired = json!({"type": "paired", "transport": "webrtc"});
    assert_eq!(sender.recv().await, paired);
    assert_eq!(receiver.recv().await, paired);

    assert_eq!(report(addr, web_report(&"0".repeat(32), 1)).await, 404);
    assert_eq!(request(addr, "GET", "/transfer-stats", "").await.0, 404);
    assert_eq!(
        request(addr, "GET", "/health", "").await,
        (200, "ok".into())
    );
}
