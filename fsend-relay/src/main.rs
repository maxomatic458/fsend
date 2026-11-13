use std::net::SocketAddr;

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
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let listener = TcpListener::bind(args.bind).await?;
    info!("listening on {}", args.bind);
    let config = Config {
        max_sessions: args.max_sessions,
        ..Config::default()
    };
    fsend_relay::serve(listener, config).await
}
