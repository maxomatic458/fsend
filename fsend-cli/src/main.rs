mod iroh;
mod relay;
mod transfer;
mod webrtc;

use std::path::PathBuf;

use clap::Parser;
use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};

use self::iroh::IrohTransfer;
use relay::{ConnectionInfo, Protocol, RelayClient};
use transfer::{FilesAvailable, ReceiveArgs, SendArgs, Transfer};

const DEFAULT_RELAY_URL: &str = "ws://127.0.0.1:3001/ws";

#[derive(Parser)]
struct Args {
    #[clap(long, short, default_value = "warn")]
    log_level: tracing::Level,

    #[clap(long, default_value = DEFAULT_RELAY_URL)]
    relay_url: String,

    #[clap(subcommand)]
    mode: Mode,
}

#[derive(clap::Subcommand)]
enum Mode {
    #[clap(name = "send", aliases = &["s"])]
    Send {
        #[clap(required = true)]
        files: Vec<PathBuf>,
    },
    #[clap(name = "receive", aliases = &["r"])]
    Receive {
        #[clap(long, short = 'f')]
        overwrite: bool,

        #[clap(long, short, default_value = ".")]
        output_dir: PathBuf,

        code: String,

        #[clap(long, short = 'y')]
        auto_accept: bool,
    },
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_max_level(args.log_level)
        .init();

    match args.mode {
        Mode::Send { files } => run_send(&args.relay_url, files).await?,
        Mode::Receive { overwrite, output_dir, code, auto_accept } => {
            run_receive(&args.relay_url, code, output_dir, !overwrite, auto_accept).await?
        }
    }

    Ok(())
}

/// Build a `Box<dyn Transfer>` for the sender side based on the negotiated protocol.
async fn create_sender_transfer(
    protocol: Protocol,
    relay: &mut RelayClient,
) -> color_eyre::Result<Box<dyn Transfer>> {
    match protocol {
        Protocol::Iroh => {
            let endpoint = IrohTransfer::create_endpoint().await?;
            let node_addr = endpoint.node_addr().await
                .map_err(|e| transfer::TransferError::Iroh(e.to_string()))?;
            let my_info = IrohTransfer::connection_info_from_node_addr(&node_addr);

            relay.send_exchange(my_info).await?;
            let _peer_info = relay.recv_exchange().await?;

            let transfer = IrohTransfer::accept(endpoint).await?;
            Ok(Box::new(transfer))
        }
        Protocol::WebRtc => {
            let (mut wrt, offer_sdp) = webrtc::WebRtcTransfer::create_offerer().await?;
            let my_info = ConnectionInfo::WebRtc { sdp: offer_sdp, ice_candidates: vec![] };

            relay.send_exchange(my_info).await?;
            let peer_info = relay.recv_exchange().await?;

            let answer_sdp = match &peer_info {
                ConnectionInfo::WebRtc { sdp, .. } => sdp.clone(),
                _ => return Err(transfer::TransferError::WebRtc("expected WebRTC answer".into()).into()),
            };
            wrt.set_answer(&answer_sdp).await?;
            wrt.wait_connected().await?;
            Ok(Box::new(wrt))
        }
    }
}

/// Build a `Box<dyn Transfer>` for the receiver side based on the negotiated protocol.
async fn create_receiver_transfer(
    protocol: Protocol,
    relay: &mut RelayClient,
) -> color_eyre::Result<Box<dyn Transfer>> {
    match protocol {
        Protocol::Iroh => {
            let endpoint = IrohTransfer::create_endpoint().await?;
            let node_addr = endpoint.node_addr().await
                .map_err(|e| transfer::TransferError::Iroh(e.to_string()))?;
            let my_info = IrohTransfer::connection_info_from_node_addr(&node_addr);

            let peer_info = relay.recv_exchange().await?;
            relay.send_exchange(my_info).await?;

            let transfer = IrohTransfer::connect(endpoint, peer_info).await?;
            Ok(Box::new(transfer))
        }
        Protocol::WebRtc => {
            let peer_info = relay.recv_exchange().await?;
            let offer_sdp = match &peer_info {
                ConnectionInfo::WebRtc { sdp, .. } => sdp.clone(),
                _ => return Err(transfer::TransferError::WebRtc("expected WebRTC offer".into()).into()),
            };

            let (mut wrt, answer_sdp) = webrtc::WebRtcTransfer::create_answerer(&offer_sdp).await?;
            let my_info = ConnectionInfo::WebRtc { sdp: answer_sdp, ice_candidates: vec![] };
            relay.send_exchange(my_info).await?;

            wrt.wait_connected().await?;
            Ok(Box::new(wrt))
        }
    }
}

async fn run_send(relay_url: &str, files: Vec<PathBuf>) -> color_eyre::Result<()> {
    for f in &files {
        if !f.exists() {
            return Err(transfer::TransferError::FileNotFound(f.clone()).into());
        }
    }

    let mut relay = RelayClient::connect(relay_url).await?;
    let code = relay.create_session(vec![Protocol::Iroh, Protocol::WebRtc]).await?;

    println!("Session code: {}\n", code.bright_white());
    println!("On the other peer, run:\n");
    println!("  {} {}\n", "fsend-cli receive".yellow(), code.yellow());

    let protocol = relay.wait_for_peer().await?;
    tracing::info!("peer joined, negotiated protocol: {:?}", protocol);

    let mut transfer = create_sender_transfer(protocol, &mut relay).await?;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let conn_type = transfer.connection_type_name().await;
    println!("Connection type: {}", colorize_conn_type(&conn_type));

    let progress_bars = std::sync::Mutex::new(None::<CliProgressBars>);

    transfer.send_files(
        SendArgs { files },
        &mut |initial| {
            *progress_bars.lock().unwrap() = Some(CliProgressBars::new(initial));
        },
        &mut || {
            print!("Waiting for peer to accept files...");
            std::io::Write::flush(&mut std::io::stdout()).unwrap();
        },
        &mut |n| {
            if let Some(pb) = &mut *progress_bars.lock().unwrap() {
                pb.update(n);
            }
        },
    ).await?;

    println!("\nTransfer complete.");
    Ok(())
}

async fn run_receive(
    relay_url: &str,
    code: String,
    output_dir: PathBuf,
    resume: bool,
    auto_accept: bool,
) -> color_eyre::Result<()> {
    let mut relay = RelayClient::connect(relay_url).await?;
    let protocol = relay.join_session(code, vec![Protocol::Iroh, Protocol::WebRtc]).await?;
    tracing::info!("joined session, negotiated protocol: {:?}", protocol);

    let mut transfer = create_receiver_transfer(protocol, &mut relay).await?;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let conn_type = transfer.connection_type_name().await;
    println!("Connection type: {}", colorize_conn_type(&conn_type));

    let progress_bars = std::sync::Mutex::new(None::<CliProgressBars>);

    transfer.receive_files(
        ReceiveArgs { resume },
        &mut |initial| {
            *progress_bars.lock().unwrap() = Some(CliProgressBars::new(initial));
        },
        &mut |files_offered| {
            if auto_accept {
                println!("Auto-accepting files.");
                Some(output_dir.clone())
            } else if accept_files(files_offered) {
                Some(output_dir.clone())
            } else {
                None
            }
        },
        &mut |n| {
            if let Some(pb) = &mut *progress_bars.lock().unwrap() {
                pb.update(n);
            }
        },
    ).await?;

    println!("\nTransfer complete.");
    Ok(())
}

fn accept_files(files: &[FilesAvailable]) -> bool {
    println!("The following files will be received:\n");

    let longest = files.iter().map(|f| f.name().len()).max().unwrap_or(0) + 1;
    let total_size: u64 = files.iter().map(|f| f.size()).sum();

    for file in files {
        let size_str = HumanBytes(file.size()).to_string();
        let name = file.name();
        let display = if matches!(file, FilesAvailable::Dir { .. }) {
            format!("{}/", name).blue()
        } else {
            format!("{} ", name).blue()
        };
        println!(" - {:<width$} {:>10}", display, size_str.red(), width = longest);
    }

    println!("\nTotal size: {}\n", HumanBytes(total_size).to_string().red());

    dialoguer::Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Accept these files?")
        .interact()
        .unwrap_or(false)
}

struct CliProgressBars {
    bars: Vec<ProgressBar>,
    total: Option<ProgressBar>,
}

impl CliProgressBars {
    fn new(data: &[(String, u64, u64)]) -> Self {
        let style = ProgressStyle::default_bar()
            .template("{spinner:.green} {prefix} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("#>-");

        let total_style = ProgressStyle::default_bar()
            .template("{spinner:.green} {prefix} [{bar:40.yellow/yellow}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("#>-");

        let (mut longest, total_progress, total_size) = data.iter().fold(
            (0usize, 0u64, 0u64),
            |(l, tp, ts), (name, prog, size)| (l.max(name.len()), tp + prog, ts + size),
        );
        longest = longest.max("Total".len());

        let mp = MultiProgress::new();
        let mut bars = Vec::new();
        for (name, progress, size) in data {
            let pb = mp.add(ProgressBar::new(*size));
            pb.set_prefix(format!("{:<width$}", name, width = longest));
            pb.set_style(style.clone());
            pb.set_position(*progress);
            pb.reset_eta();
            bars.push(pb);
        }

        let total = if bars.len() > 1 {
            let pb = mp.add(ProgressBar::new(total_size));
            pb.set_prefix(format!("{:<width$}", "Total", width = longest));
            pb.set_style(total_style);
            pb.set_position(total_progress);
            pb.reset_eta();
            Some(pb)
        } else {
            None
        };

        Self { bars, total }
    }

    fn update(&mut self, mut progress: u64) {
        if let Some(pb) = &self.total {
            pb.inc(progress);
        }
        for pb in &self.bars {
            let remaining = pb.length().unwrap_or(0).saturating_sub(pb.position());
            if remaining == 0 { continue; }
            let inc = progress.min(remaining);
            pb.inc(inc);
            progress -= inc;
            if progress == 0 { break; }
        }
    }
}

fn colorize_conn_type(conn_type: &str) -> String {
    match conn_type {
        "Direct" => conn_type.green().to_string(),
        "Relay" => conn_type.red().to_string(),
        "Mixed" => conn_type.yellow().to_string(),
        _ => conn_type.red().to_string(),
    }
}
