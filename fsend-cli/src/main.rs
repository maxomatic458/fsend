use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use fsend_core::transfer::{Callbacks, ConnectionKind, Event, TransferError, build_offer};
use fsend_core::{CancellationToken, Client, Error};
use fsend_proto::code::Code;
use fsend_proto::peer::resume::{Conflict, ConflictPolicy};
use fsend_proto::peer::{self, Entry, progress::TopEntry};
use url::Url;

use cli::{Args, Mode, OnConflict};

mod cli;

const APP: &str = concat!("fsend-cli ", env!("CARGO_PKG_VERSION"));
/// What a shell reports for a command stopped by Ctrl+C.
const INTERRUPTED: u8 = 130;

#[tokio::main]
async fn main() -> ExitCode {
    let Err(report) = run().await else {
        return ExitCode::SUCCESS;
    };

    let Some(error) = report.downcast_ref::<Error>() else {
        eprintln!("{report:?}");
        return ExitCode::FAILURE;
    };
    match error.user_error() {
        Some(message) if error.is_deliberate() => eprintln!("{message}"),
        Some(message) => eprintln!("{} {message}", "error:".red()),
        None => eprintln!("{report:?}"),
    }
    match error {
        Error::Transfer(TransferError::Cancelled) => ExitCode::from(INTERRUPTED),
        _ => ExitCode::FAILURE,
    }
}

async fn run() -> color_eyre::Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(log_filter(args.log_level))
        .init();

    let client = Client::new(args.relay_url, APP);
    let cancel = cancel_on_ctrl_c();
    match args.mode {
        Mode::Send {
            files,
            no_compression,
        } => run_send(&client, &args.download_url, files, !no_compression, &cancel).await?,
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
            run_receive(&client, code, output_dir, on_conflict, auto_accept, &cancel).await?
        }
        Mode::Version => print_version(),
    }

    Ok(())
}

/// The first Ctrl+C cancels the transfer, telling the peer; a second one quits at once.
fn cancel_on_ctrl_c() -> CancellationToken {
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            token.cancel();
            let _ = tokio::signal::ctrl_c().await;
            std::process::exit(INTERRUPTED.into());
        }
    });
    cancel
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
    client: &Client,
    download_url: &Url,
    files: Vec<PathBuf>,
    compress: bool,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let outgoing = build_offer(&files)?;
    let show_code = |code: &Code| {
        let link = code.link(download_url.as_str());
        let code = code.to_string();
        println!("Code: {}\n", code.bright_white());
        println!("On the other peer, run:\n");
        println!("  {} {}\n", "fsend receive".yellow(), code.yellow());
        println!("or open in a browser:\n");
        println!("  {}\n", link.blue());
    };

    let mut progress = Progress::default();
    let mut on_event = |event: Event| progress.on_event(event);
    client
        .send(&outgoing, compress, show_code, &mut on_event, cancel)
        .await?;

    println!("\nTransfer complete.");
    Ok(())
}

async fn run_receive(
    client: &Client,
    code: Code,
    output_dir: PathBuf,
    on_conflict: OnConflict,
    auto_accept: bool,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut progress = Progress::default();
    let mut handler = Callbacks {
        accept: |offered: &[TopEntry]| {
            if auto_accept {
                println!("Auto-accepting files.");
                Some(output_dir.clone())
            } else if accept_files(offered) {
                Some(output_dir.clone())
            } else {
                None
            }
        },
        resolve: |entries: &[Entry], conflicts: &[Conflict]| {
            resolve_conflicts(entries, conflicts, on_conflict)
        },
        event: |event: Event| progress.on_event(event),
    };
    client.receive(&code, &mut handler, cancel).await?;

    println!("\nTransfer complete.");
    Ok(())
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
            Event::Connected(kind) => println!("Connection type: {}", colorize_conn_type(kind)),
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
