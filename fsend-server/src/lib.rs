mod limits;
mod relay;
mod stats;

use std::{io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::{http::HeaderName, routing::get};
use tokio::net::TcpListener;

use crate::stats::Stats;

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
    /// The header a proxy in front sets to the client's address.
    pub ip_header: Option<HeaderName>,
    /// The SQLite database for transfer stats.
    pub database: Option<PathBuf>,
    /// How long a receiver may take to report its transfer after pairing.
    pub stats_token_ttl: Duration,
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
            database: None,
            stats_token_ttl: Duration::from_secs(48 * 60 * 60),
        }
    }
}

pub async fn serve(listener: TcpListener, config: Config) -> io::Result<()> {
    let stats = (config.database.as_deref())
        .map(|path| Stats::open(path, config.stats_token_ttl))
        .transpose()?
        .map(Arc::new);
    let mut router = relay::router(config, stats.clone()).route("/health", get(|| async { "ok" }));
    if let Some(stats) = stats {
        router = router.merge(stats::router(stats));
    }
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}
