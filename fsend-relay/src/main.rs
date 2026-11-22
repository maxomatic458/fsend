use std::io;
use std::net::SocketAddr;

use axum::http::HeaderName;
use clap::Parser;
use fsend_relay::Config;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Parser)]
#[command(name = "fsend-relay")]
#[command(about = "The relay server that pairs fsend peers")]
struct Args {
    #[arg(default_value = "0.0.0.0:3001")]
    bind: SocketAddr,

    /// Sessions open at once; more are answered with `busy`.
    #[arg(long, default_value_t = Config::default().max_sessions)]
    max_sessions: usize,

    /// Connections open at once from one address.
    #[arg(long, default_value_t = Config::default().connections_per_ip)]
    connections_per_ip: usize,

    /// Sessions one address may create or join per minute.
    #[arg(long, default_value_t = Config::default().sessions_per_minute)]
    sessions_per_minute: u32,

    /// Header with the client's address, as Cloudflare sets it; empty to use
    /// the connection's own when the relay is reached directly.
    #[arg(long, default_value = "cf-connecting-ip")]
    ip_header: String,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let listener = TcpListener::bind(args.bind).await?;
    info!("listening on {}", args.bind);
    let ip_header = (!args.ip_header.is_empty())
        .then(|| HeaderName::try_from(args.ip_header))
        .transpose()
        .map_err(io::Error::other)?;
    let config = Config {
        max_sessions: args.max_sessions,
        connections_per_ip: args.connections_per_ip,
        sessions_per_minute: args.sessions_per_minute,
        ip_header,
        ..Config::default()
    };
    fsend_relay::serve(listener, config).await
}
