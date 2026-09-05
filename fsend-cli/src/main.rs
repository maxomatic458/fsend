use fsend_cli::{cli, iroh, relay, transfer, webrtc};

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use self::iroh::IrohTransfer;
use cli::{Args, Mode};
use relay::{ConnectionInfo, Protocol, RelayClient};
use transfer::{FilesAvailable, ReceiveArgs, SendArgs, Transfer, TransferError};

#[tokio::main]
async fn main() -> ExitCode {
    let Err(report) = run().await else {
        return ExitCode::SUCCESS;
    };

    match report
        .downcast_ref::<TransferError>()
        .and_then(TransferError::user_error)
    {
        Some(msg) => eprintln!("{msg}"),
        None => eprintln!("{report:?}"),
    }

    ExitCode::FAILURE
}

async fn run() -> color_eyre::Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_max_level(args.log_level)
        .init();

    match args.mode {
        Mode::Send { files } => run_send(&args.relay_url, &args.download_url, files).await?,
        Mode::Receive {
            overwrite,
            output_dir,
            code,
            auto_accept,
        } => run_receive(&args.relay_url, code, output_dir, !overwrite, auto_accept).await?,
        Mode::Version => print_version(),
    }

    Ok(())
}

fn print_version() {
    let rows = [
        ("fsend-cli", env!("CARGO_PKG_VERSION").to_owned()),
        ("protocol", transfer::PROTO_VERSION.to_owned()),
        (
            "alpn",
            String::from_utf8_lossy(transfer::FSEND_ALPN).into_owned(),
        ),
    ];

    for (label, value) in rows {
        println!("{:<10} {}", label, value.bright_white());
    }
}

/// Accepts a bare code or a share link like `https://fsend.sh/receive/AB12CD34`.
/// Codes are uppercase, so a hand-typed lowercase one is normalised too.
fn parse_code(input: &str) -> String {
    let cleaned = input.trim();
    let cleaned = cleaned.split(['?', '#']).next().unwrap_or(cleaned);
    let cleaned = cleaned.trim_end_matches('/');
    cleaned.rsplit('/').next().unwrap_or(cleaned).to_uppercase()
}

/// The link a receiver can open in a browser, e.g.
/// `https://fsend.sh` + `AB12CD34` -> `https://fsend.sh/receive/AB12CD34`.
fn download_link(base: &str, code: &str) -> String {
    format!("{}/receive/{}", base.trim_end_matches('/'), code)
}

async fn create_sender_transfer(
    protocol: Protocol,
    relay: &mut RelayClient,
) -> color_eyre::Result<Box<dyn Transfer>> {
    match protocol {
        Protocol::Iroh => {
            let endpoint = IrohTransfer::create_endpoint().await?;
            let my_info = IrohTransfer::local_connection_info(&endpoint).await;

            relay.send_exchange(my_info).await?;
            let _peer_info = relay.recv_exchange().await?;

            let transfer = IrohTransfer::accept(endpoint).await?;
            Ok(Box::new(transfer))
        }
        Protocol::WebRtc => {
            let (mut wrt, offer_sdp) = webrtc::WebRtcTransfer::create_offerer().await?;
            let my_info = ConnectionInfo::WebRtc {
                sdp: offer_sdp,
                ice_candidates: vec![],
            };

            relay.send_exchange(my_info).await?;
            let peer_info = relay.recv_exchange().await?;

            let answer_sdp = match &peer_info {
                ConnectionInfo::WebRtc { sdp, .. } => sdp.clone(),
                _ => return Err(transfer::TransferError::UnexpectedConnectionInfo.into()),
            };
            wrt.set_answer(&answer_sdp).await?;
            wrt.wait_connected().await?;
            Ok(Box::new(wrt))
        }
    }
}

async fn create_receiver_transfer(
    protocol: Protocol,
    relay: &mut RelayClient,
) -> color_eyre::Result<Box<dyn Transfer>> {
    match protocol {
        Protocol::Iroh => {
            let endpoint = IrohTransfer::create_endpoint().await?;
            let my_info = IrohTransfer::local_connection_info(&endpoint).await;

            let peer_info = relay.recv_exchange().await?;
            relay.send_exchange(my_info).await?;

            let transfer = IrohTransfer::connect(endpoint, peer_info).await?;
            Ok(Box::new(transfer))
        }
        Protocol::WebRtc => {
            let peer_info = relay.recv_exchange().await?;
            let offer_sdp = match &peer_info {
                ConnectionInfo::WebRtc { sdp, .. } => sdp.clone(),
                _ => return Err(transfer::TransferError::UnexpectedConnectionInfo.into()),
            };

            let (mut wrt, answer_sdp) = webrtc::WebRtcTransfer::create_answerer(&offer_sdp).await?;
            let my_info = ConnectionInfo::WebRtc {
                sdp: answer_sdp,
                ice_candidates: vec![],
            };
            relay.send_exchange(my_info).await?;

            wrt.wait_connected().await?;
            Ok(Box::new(wrt))
        }
    }
}

async fn run_send(
    relay_url: &str,
    download_url: &str,
    files: Vec<PathBuf>,
) -> color_eyre::Result<()> {
    let missing: Vec<_> = files.iter().filter(|f| !f.exists()).cloned().collect();

    if !missing.is_empty() {
        return Err(TransferError::PathsNotFound(missing).into());
    }

    let mut relay = RelayClient::connect(relay_url).await?;
    let code = relay
        .create_session(vec![Protocol::Iroh, Protocol::WebRtc])
        .await?;

    println!("Session code: {}\n", code.bright_white());
    println!("On the other peer, run:\n");
    println!("  {} {}\n", "fsend receive".yellow(), code.yellow());

    println!("or open in a browser:\n");
    println!("  {}\n", download_link(download_url, &code).blue());

    let protocol = relay.wait_for_peer().await?;
    tracing::info!("peer joined, negotiated protocol: {:?}", protocol);

    let mut transfer = create_sender_transfer(protocol, &mut relay).await?;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let conn_type = transfer.connection_type_name().await;
    println!("Connection type: {}", colorize_conn_type(&conn_type));

    let progress_bars = std::sync::Mutex::new(None::<CliProgressBars>);

    transfer
        .send_files(
            SendArgs { files },
            &mut |initial| {
                *progress_bars.lock().unwrap() = Some(CliProgressBars::new(initial));
            },
            &mut || {
                println!("Waiting for peer to accept files...");
            },
            &mut |n| {
                if let Some(pb) = &mut *progress_bars.lock().unwrap() {
                    pb.update(n);
                }
            },
        )
        .await?;

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
    let code = parse_code(&code);

    let mut relay = RelayClient::connect(relay_url).await?;
    let protocol = relay
        .join_session(code, vec![Protocol::Iroh, Protocol::WebRtc])
        .await?;
    tracing::info!("joined session, negotiated protocol: {:?}", protocol);

    let mut transfer = create_receiver_transfer(protocol, &mut relay).await?;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let conn_type = transfer.connection_type_name().await;
    println!("Connection type: {}", colorize_conn_type(&conn_type));

    let progress_bars = std::sync::Mutex::new(None::<CliProgressBars>);

    transfer
        .receive_files(
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
        )
        .await?;

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
        println!(
            " - {:<width$} {:>10}",
            display,
            size_str.red(),
            width = longest
        );
    }

    println!(
        "\nTotal size: {}\n",
        HumanBytes(total_size).to_string().red()
    );

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
            .template(
                "{spinner:.green} {prefix} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-");

        let total_style = ProgressStyle::default_bar()
            .template(
                "{spinner:.green} {prefix} [{bar:40.yellow/yellow}] {bytes}/{total_bytes} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-");

        let (mut longest, total_progress, total_size) = data
            .iter()
            .fold((0usize, 0u64, 0u64), |(l, tp, ts), (name, prog, size)| {
                (l.max(name.len()), tp + prog, ts + size)
            });
        longest = longest.max("Total".len());

        let mp = if cfg!(test) {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        } else {
            MultiProgress::new()
        };
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
            if remaining == 0 {
                continue;
            }
            let inc = progress.min(remaining);
            pb.inc(inc);
            progress -= inc;
            if progress == 0 {
                break;
            }
        }
    }
}

fn colorize_conn_type(conn_type: &str) -> String {
    match conn_type {
        "Direct" => conn_type.green().to_string(),
        "Relay" => conn_type.yellow().to_string(),
        "WebRTC" => conn_type.green().to_string(),
        _ => conn_type.red().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli::DEFAULT_DOWNLOAD_URL;

    #[test]
    fn accepts_codes_and_share_links() {
        assert_eq!(parse_code("AB12CD34"), "AB12CD34");
        assert_eq!(parse_code("ab12cd34"), "AB12CD34");
        assert_eq!(parse_code("  AB12CD34\n"), "AB12CD34");
        assert_eq!(parse_code("https://fsend.sh/receive/AB12CD34"), "AB12CD34");
        assert_eq!(
            parse_code("http://localhost:3000/receive/YJ6HM1DC"),
            "YJ6HM1DC"
        );
        // Trailing slash, query and fragment must not end up in the code.
        assert_eq!(parse_code("https://fsend.sh/receive/AB12CD34/"), "AB12CD34");
        assert_eq!(
            parse_code("https://fsend.sh/receive/AB12CD34?ref=x"),
            "AB12CD34"
        );
        assert_eq!(
            parse_code("https://fsend.sh/receive/AB12CD34#top"),
            "AB12CD34"
        );
    }

    #[test]
    fn builds_the_download_link() {
        assert_eq!(
            download_link("https://fsend.sh", "AB12CD34"),
            "https://fsend.sh/receive/AB12CD34"
        );
        // A trailing slash must not double up.
        assert_eq!(
            download_link("http://localhost:3000/", "AB12CD34"),
            "http://localhost:3000/receive/AB12CD34"
        );
    }

    #[test]
    fn a_printed_link_is_accepted_back_as_a_code() {
        let code = "AB12CD34";
        let link = download_link(DEFAULT_DOWNLOAD_URL, code);
        assert_eq!(parse_code(&link), code);
    }

    #[test]
    fn progress_bars_fill_files_in_order() {
        let mut bars = CliProgressBars::new(&[
            ("a".into(), 0, 100),
            ("b".into(), 50, 100),
            ("c".into(), 100, 100),
        ]);
        let positions = |bars: &CliProgressBars| -> Vec<u64> {
            bars.bars.iter().map(|b| b.position()).collect()
        };
        assert_eq!(positions(&bars), [0, 50, 100]);
        let total = bars.total.as_ref().expect("several files get a total bar");
        assert_eq!(total.position(), 150);
        assert_eq!(total.length(), Some(300));

        // Bytes land in the first file that still has room.
        bars.update(30);
        assert_eq!(positions(&bars), [30, 50, 100]);
        // then they move to the next bar
        bars.update(100);
        assert_eq!(positions(&bars), [100, 80, 100]);
        bars.update(20);
        assert_eq!(positions(&bars), [100, 100, 100]);
        assert_eq!(bars.total.as_ref().unwrap().position(), 300);
    }

    #[test]
    fn a_single_file_has_no_total_bar() {
        let mut bars = CliProgressBars::new(&[("only".into(), 10, 40)]);
        assert!(bars.total.is_none());
        bars.update(30);
        assert_eq!(bars.bars[0].position(), 40);
    }

    #[test]
    fn progress_bars_survive_no_files() {
        let mut bars = CliProgressBars::new(&[]);
        assert!(bars.bars.is_empty() && bars.total.is_none());
        bars.update(5);
    }

    #[test]
    fn connection_types_keep_their_names() {
        colored::control::set_override(false);
        for kind in ["Direct", "Relay", "WebRTC", "None", "Unknown"] {
            assert_eq!(colorize_conn_type(kind), kind);
        }
    }
}
