use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use cli::{Args, Mode, OnConflict};
use fsend_core::connect::{connect_as_receiver, connect_as_sender};
use fsend_core::relay::{pair_as_receiver, pair_as_sender};
use fsend_core::transfer::{
    self, Answer, ConnectionKind, Event, TransferError, build_offer, missing_paths,
};
use fsend_core::{CancellationToken, Error};
use fsend_proto::code::Code;
use fsend_proto::peer::resume::{Conflict, ConflictPolicy};
use fsend_proto::peer::{self, Entry, progress::TopEntry};
use url::Url;

mod cli;

/// How this client introduces itself to the peer.
const APP: &str = concat!("fsend-cli ", env!("CARGO_PKG_VERSION"));

#[tokio::main]
async fn main() -> ExitCode {
    let Err(report) = run().await else {
        return ExitCode::SUCCESS;
    };

    match report.downcast_ref::<Error>().and_then(user_error) {
        Some(msg) => eprintln!("{msg}"),
        None => eprintln!("{report:?}"),
    }

    ExitCode::FAILURE
}

async fn run() -> color_eyre::Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(log_filter(args.log_level))
        .init();

    match args.mode {
        Mode::Send {
            files,
            no_compression,
        } => run_send(&args.relay_url, &args.download_url, files, !no_compression).await?,
        Mode::Receive {
            on_conflict,
            output_dir,
            code,
            auto_accept,
        } => {
            let on_conflict = on_conflict.unwrap_or(match auto_accept {
                true => OnConflict::Skip,
                false => OnConflict::Ask,
            });
            run_receive(&args.relay_url, code, output_dir, on_conflict, auto_accept).await?
        }
        Mode::Version => print_version(),
    }

    Ok(())
}

/// What to tell the user about `error`, if they can act on it.
fn user_error(error: &Error) -> Option<String> {
    let message = match error {
        Error::Transfer(TransferError::Declined) => return Some("Transfer declined.".into()),
        Error::Transfer(TransferError::PathsNotFound(paths)) => {
            let list: String = paths
                .iter()
                .map(|p| format!("\n - {}", p.display().to_string().blue()))
                .collect();
            format!("{}:{list}", missing_paths(paths.len()))
        }
        error => error.user_message()?,
    };
    Some(format!("{} {message}", "error:".red()))
}

/// At error level, libraries are silenced: webrtc logs errors that are not.
fn log_filter(level: tracing::Level) -> Targets {
    let own = LevelFilter::from_level(level);
    let libraries = if level == tracing::Level::ERROR {
        LevelFilter::OFF
    } else {
        own
    };
    Targets::new()
        .with_default(libraries)
        .with_target("fsend", own)
}

fn print_version() {
    let rows = [
        ("fsend-cli", env!("CARGO_PKG_VERSION").to_owned()),
        ("protocol", peer::VERSION.to_string()),
    ];

    for (label, value) in rows {
        println!("{:<10} {}", label, value.bright_white());
    }
}

async fn run_send(
    relay_url: &Url,
    download_url: &Url,
    files: Vec<PathBuf>,
    compress: bool,
) -> Result<(), Error> {
    let outgoing = build_offer(&files)?;

    let mut relay = pair_as_sender(relay_url, APP, |code| {
        let link = code.link(download_url.as_str());
        let code = code.to_string();
        println!("Code: {}\n", code.bright_white());
        println!("On the other peer, run:\n");
        println!("  {} {}\n", "fsend receive".yellow(), code.yellow());
        println!("or open in a browser:\n");
        println!("  {}\n", link.blue());
    })
    .await?;

    let mut transport = connect_as_sender(&mut relay).await?;
    drop(relay);
    announce_connection(transport.as_ref()).await;

    let mut progress = Progress::default();
    transfer::send_files(
        transport.as_mut(),
        &outgoing,
        compress,
        &mut |event| progress.on_event(event),
        CancellationToken::new(),
    )
    .await?;

    println!("\nTransfer complete.");
    Ok(())
}

async fn run_receive(
    relay_url: &Url,
    code: Code,
    output_dir: PathBuf,
    on_conflict: OnConflict,
    auto_accept: bool,
) -> Result<(), Error> {
    let mut relay = pair_as_receiver(relay_url, APP, &code).await?;
    let mut transport = connect_as_receiver(&mut relay).await?;
    drop(relay);
    announce_connection(transport.as_ref()).await;

    let mut progress = Progress::default();
    // The prompts block, so they leave the async workers to the transfer.
    let mut decide = Answer {
        accept: |offered: &[TopEntry]| {
            if auto_accept {
                println!("Auto-accepting files.");
                Some(output_dir.clone())
            } else if tokio::task::block_in_place(|| accept_files(offered)) {
                Some(output_dir.clone())
            } else {
                None
            }
        },
        resolve: |entries: &[Entry], conflicts: &[Conflict]| {
            tokio::task::block_in_place(|| resolve_conflicts(entries, conflicts, on_conflict))
        },
    };
    transfer::receive_files(
        transport.as_mut(),
        &mut decide,
        &mut |event| progress.on_event(event),
        CancellationToken::new(),
    )
    .await?;

    println!("\nTransfer complete.");
    Ok(())
}

async fn announce_connection(transport: &dyn transfer::Transport) {
    tokio::time::sleep(Duration::from_secs(2)).await;
    println!(
        "Connection type: {}",
        colorize_conn_type(transport.connection_kind())
    );
}

#[derive(Default)]
struct Progress(Option<CliProgressBars>);

impl Progress {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Offered => println!("Waiting for peer to accept files..."),
            Event::Started { entries, present } => {
                let (here, total) = (
                    present.iter().sum::<u64>(),
                    entries.iter().map(|e| e.size).sum(),
                );
                if here > 0 {
                    println!(
                        "Already there: {} of {}",
                        HumanBytes(here),
                        HumanBytes(total)
                    );
                }
                let initial: Vec<_> = entries
                    .iter()
                    .zip(present)
                    .map(|(entry, present)| (entry.name.clone(), *present, entry.size))
                    .collect();
                self.0 = Some(CliProgressBars::new(&initial));
            }
            Event::Progress { entry, bytes } => {
                if let Some(bars) = &mut self.0 {
                    bars.update(entry, bytes);
                }
            }
        }
    }
}

fn accept_files(files: &[TopEntry]) -> bool {
    println!("The following files will be received:\n");

    let longest = files
        .iter()
        .map(|f| f.name.chars().count())
        .max()
        .unwrap_or(0)
        + 1;
    let total_size: u64 = files.iter().map(|f| f.size).sum();

    for file in files {
        let size_str = HumanBytes(file.size).to_string();
        let name = &file.name;
        let display = if file.dir {
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

/// Lists the conflicts and picks what to do with them.
fn resolve_conflicts(
    entries: &[Entry],
    conflicts: &[Conflict],
    on_conflict: OnConflict,
) -> ConflictPolicy {
    const SHOWN: usize = 10;
    let list = |conflicts: &[&Conflict]| {
        for conflict in conflicts.iter().take(SHOWN) {
            println!(
                " - {}",
                entries[conflict.id as usize].path().join("/").blue()
            );
        }
        if conflicts.len() > SHOWN {
            println!(" ... and {} more", conflicts.len() - SHOWN);
        }
    };
    let (replaceable, blocked): (Vec<_>, Vec<_>) = conflicts.iter().partition(|c| c.replaceable);
    if !blocked.is_empty() {
        println!("\nSkipping these, a file or folder of another kind is in the way:\n");
        list(&blocked);
    }
    if replaceable.is_empty() {
        return ConflictPolicy::Skip;
    }
    println!("\nThese files already exist and differ from the offered ones:\n");
    list(&replaceable);
    println!();
    match on_conflict {
        OnConflict::Skip => {
            println!("Skipping them.");
            ConflictPolicy::Skip
        }
        OnConflict::Replace => {
            println!("Replacing them.");
            ConflictPolicy::Replace
        }
        OnConflict::Ask => {
            let choice = dialoguer::Select::with_theme(&ColorfulTheme::default())
                .with_prompt("What should happen to them?")
                .items(["Skip", "Replace"])
                .default(0)
                .interact()
                .unwrap_or(0);
            match choice {
                1 => ConflictPolicy::Replace,
                _ => ConflictPolicy::Skip,
            }
        }
    }
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
                (l.max(name.chars().count()), tp + prog, ts + size)
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

    fn update(&mut self, entry: usize, bytes: u64) {
        if let Some(pb) = &self.total {
            pb.inc(bytes);
        }
        if let Some(pb) = self.bars.get(entry) {
            pb.inc(bytes);
        }
    }
}

fn colorize_conn_type(kind: ConnectionKind) -> String {
    let name = kind.to_string();
    match kind {
        ConnectionKind::Direct | ConnectionKind::WebRtc => name.green().to_string(),
        ConnectionKind::Relay => name.yellow().to_string(),
        ConnectionKind::Unknown => name.red().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lands_on_its_entry() {
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

        bars.update(1, 30);
        assert_eq!(positions(&bars), [0, 80, 100]);
        bars.update(0, 100);
        assert_eq!(positions(&bars), [100, 80, 100]);
        bars.update(1, 20);
        assert_eq!(positions(&bars), [100, 100, 100]);
        assert_eq!(bars.total.as_ref().unwrap().position(), 300);
    }

    #[test]
    fn a_single_file_has_no_total_bar() {
        let mut bars = CliProgressBars::new(&[("only".into(), 10, 40)]);
        assert!(bars.total.is_none());
        bars.update(0, 30);
        assert_eq!(bars.bars[0].position(), 40);
    }

    #[test]
    fn progress_bars_survive_no_files() {
        let mut bars = CliProgressBars::new(&[]);
        assert!(bars.bars.is_empty() && bars.total.is_none());
        bars.update(0, 5);
    }

    #[test]
    fn connection_types_keep_their_names() {
        colored::control::set_override(false);
        for (kind, name) in [
            (ConnectionKind::Direct, "Direct"),
            (ConnectionKind::Relay, "Relay"),
            (ConnectionKind::WebRtc, "WebRTC"),
            (ConnectionKind::Unknown, "Unknown"),
        ] {
            assert_eq!(colorize_conn_type(kind), name);
        }
    }
}
