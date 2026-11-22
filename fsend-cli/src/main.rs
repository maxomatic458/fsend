use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use color_eyre::eyre::eyre;
use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use cli::{Args, Mode, OnConflict};
use config::{Config, ConfigError};
use fsend_core::{
    CancellationToken, Client, ConnectionKind, Error, Event, Outgoing, ReceiveHandler, async_trait,
    build_offer,
};
use fsend_proto::code::Code;
use fsend_proto::peer::checksum::{self, Algorithm};
use fsend_proto::peer::resume::{Conflict, ConflictPolicy};
use fsend_proto::peer::{AcceptedFile, Entry, Node, Offer};
use fsend_proto::relay;
use fsend_proto::stats::Platform;
use url::Url;

mod cli;
mod config;

const APP: &str = concat!("fsend-cli ", env!("CARGO_PKG_VERSION"));

#[tokio::main]
async fn main() -> ExitCode {
    let Err(report) = run().await else {
        return ExitCode::SUCCESS;
    };

    if let Some(error) = report.downcast_ref::<Error>() {
        eprintln!("{} {}", "error:".red(), printable(&error.to_string()));
    } else if let Some(error) = report.downcast_ref::<ConfigError>() {
        eprintln!("{} {error}", "error:".red());
    } else {
        eprintln!("{report:?}");
    }

    ExitCode::FAILURE
}

async fn run() -> color_eyre::Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    let config_path = args.config.clone().or_else(config::default_path);
    let config = Config::load(args.config.as_deref())?.with_args(&args);
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(log_filter(config.log_level))
        .init();

    let client = Client {
        server_url: config.server_url,
        app: APP.into(),
        platform: Platform::Cli,
        telemetry: config.telemetry,
    };
    match args.mode {
        Mode::Send {
            files,
            no_compression,
            ..
        } => {
            run_send(
                &client,
                &config.download_url,
                files,
                !no_compression,
                config.checksums,
            )
            .await?
        }
        Mode::Receive {
            on_conflict,
            output_dir,
            code,
            auto_accept,
        } => {
            let receiver = Receiver {
                auto_accept,
                on_conflict: on_conflict.unwrap_or(match auto_accept {
                    true => OnConflict::Skip,
                    false => OnConflict::Ask,
                }),
                progress: None,
                checksums: false,
            };
            run_receive(&client, code, output_dir, receiver).await?
        }
        Mode::Version => print_version(config_path.as_deref()),
        Mode::Config => match config_path {
            Some(path) => println!("{}", path.display()),
            None => return Err(eyre!("this system has no config directory")),
        },
    }

    Ok(())
}

/// `text` with control characters escaped: the peer may have written part of it.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| match c.is_control() {
            true => c.escape_unicode().to_string(),
            false => c.to_string(),
        })
        .collect()
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

/// The versions, and which config file is read.
fn print_version(config: Option<&Path>) {
    let config = match config {
        Some(path) if path.exists() => path.display().to_string(),
        Some(path) => format!("{} (not found)", path.display()),
        None => "none".into(),
    };
    let rows = [
        ("fsend-cli", env!("CARGO_PKG_VERSION").to_owned()),
        ("protocol", relay::VERSION.to_string()),
        ("config", config),
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
    checksums: Option<Algorithm>,
) -> Result<(), Error> {
    let mut outgoing = build_offer(&files)?;
    if let Some(algorithm) = checksums {
        add_checksums(&mut outgoing, algorithm).await?;
    }

    let mut progress = Progress::new(outgoing.offer());
    let mut on_event = |event: Event<'_>| match event {
        Event::Code(code) => show_code(code, download_url),
        Event::Paired => println!("Waiting for peer to accept files..."),
        event => progress.on_event(event),
    };
    let cancel = CancellationToken::new();
    client
        .send(&outgoing, compress, &mut on_event, cancel)
        .await?;

    println!("\nTransfer complete.");
    Ok(())
}

fn show_code(code: &Code, download_url: &Url) {
    let link = code.link(download_url.as_str());
    let code = code.to_string();
    println!("Code: {}\n", code.bright_white());
    println!("On the other peer, run:\n");
    println!("  {} {}\n", "fsend receive".yellow(), code.yellow());
    println!("or open in a browser:\n");
    println!("  {}\n", link.blue());
}

async fn run_receive(
    client: &Client,
    code: Code,
    output_dir: PathBuf,
    mut receiver: Receiver,
) -> Result<(), Error> {
    let cancel = CancellationToken::new();
    client
        .receive(&code, &output_dir, &mut receiver, cancel)
        .await?;

    if receiver.checksums {
        println!("\nChecksums verified for every received file.");
    }
    println!("\nTransfer complete.");
    Ok(())
}

struct Receiver {
    auto_accept: bool,
    on_conflict: OnConflict,
    progress: Option<Progress>,
    checksums: bool,
}

// The prompts block, so they run off the async workers.
#[async_trait]
impl ReceiveHandler for Receiver {
    async fn accept(&mut self, offer: &Offer) -> bool {
        self.progress = Some(Progress::new(offer));
        self.checksums = offer.checksums.is_some();
        if self.auto_accept {
            println!("Auto-accepting files.");
            return true;
        }
        tokio::task::block_in_place(|| accept_files(offer))
    }

    async fn resolve(&mut self, entries: &[Entry], conflicts: &[Conflict]) -> ConflictPolicy {
        tokio::task::block_in_place(|| resolve_conflicts(entries, conflicts, self.on_conflict))
    }

    fn event(&mut self, event: Event<'_>) {
        if let Some(progress) = &mut self.progress {
            progress.on_event(event);
        }
    }
}

async fn add_checksums(outgoing: &mut Outgoing, algorithm: Algorithm) -> Result<(), Error> {
    let bar = byte_bar(outgoing.size(), "Computing checksums", "green");
    outgoing
        .add_checksums(algorithm, &mut |done| bar.set_position(done))
        .await?;
    bar.finish_and_clear();
    Ok(())
}

fn byte_bar(total: u64, prefix: &'static str, color: &str) -> ProgressBar {
    let template = format!(
        "{{spinner:.green}} {{prefix}} [{{bar:40.{color}/blue}}] {{bytes}}/{{total_bytes}} ({{eta}})"
    );
    let bar = ProgressBar::new(total);
    bar.set_style(
        ProgressStyle::default_bar()
            .template(&template)
            .unwrap()
            .progress_chars("#>-"),
    );
    bar.set_prefix(prefix);
    bar
}

/// A bar per offered entry.
struct Progress {
    tops: Vec<(String, u64)>,
    /// By file id: its size and the offered entry it is under.
    files: Vec<(u64, usize)>,
    verifying: Option<ProgressBar>,
    bars: Option<CliProgressBars>,
}

impl Progress {
    fn new(offer: &Offer) -> Self {
        let tops = (offer.entries.iter())
            .map(|node| (node.name().to_owned(), node.size()))
            .collect();
        // Ids count every entry in offer order, each folder before what it holds.
        fn walk(nodes: &[Node], top: usize, files: &mut Vec<(u64, usize)>) {
            for node in nodes {
                match node {
                    Node::File { size, .. } => files.push((*size, top)),
                    Node::Dir { entries, .. } => {
                        files.push((0, top));
                        walk(entries, top, files);
                    }
                }
            }
        }
        let mut files = Vec::new();
        for (top, node) in offer.entries.iter().enumerate() {
            walk(std::slice::from_ref(node), top, &mut files);
        }
        Self {
            tops,
            files,
            verifying: None,
            bars: None,
        }
    }

    /// Bytes already there of each offered entry.
    fn present(&self, accepted: &[AcceptedFile]) -> Vec<u64> {
        let mut present: Vec<_> = self.tops.iter().map(|(_, size)| *size).collect();
        for file in accepted {
            let (size, top) = self.files[file.id as usize];
            present[top] -= size - file.offset;
        }
        present
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Connected(kind) => println!("Connection type: {}", colorize_conn_type(kind)),
            Event::Verifying { done, total } => self
                .verifying
                .get_or_insert_with(|| byte_bar(total, "Verifying existing files", "cyan"))
                .set_position(done),
            Event::Started(accepted) => {
                if let Some(bar) = self.verifying.take() {
                    bar.finish_and_clear();
                }
                let present = self.present(accepted);
                let here: u64 = present.iter().sum();
                if here > 0 {
                    let total = self.tops.iter().map(|(_, size)| size).sum();
                    println!(
                        "Already there: {} of {}",
                        HumanBytes(here),
                        HumanBytes(total)
                    );
                }
                let initial: Vec<_> = (self.tops.iter().zip(present))
                    .map(|((name, size), present)| (name.clone(), present, *size))
                    .collect();
                self.bars = Some(CliProgressBars::new(&initial));
            }
            Event::Progress { id, bytes } => {
                if let Some(bars) = &mut self.bars {
                    bars.update(self.files[id as usize].1, bytes);
                }
            }
            Event::Code(_) | Event::Paired => {}
        }
    }
}

fn accept_files(offer: &Offer) -> bool {
    println!("The following files will be received:\n");

    let longest = (offer.entries.iter())
        .map(|node| node.name().chars().count())
        .max()
        .unwrap_or(0)
        + 1;
    let total_size: u64 = offer.entries.iter().map(Node::size).sum();

    for node in &offer.entries {
        let size_str = HumanBytes(node.size()).to_string();
        let name = node.name();
        let display = if node.is_dir() {
            format!("{}/", name).blue()
        } else {
            format!("{} ", name).blue()
        };
        let line = format!(
            " - {:<width$} {:>10}",
            display,
            size_str.red(),
            width = longest
        );
        match (node, offer.checksums) {
            (Node::Dir { .. }, _) => println!("{line}  {}", files_in(node.files()).dimmed()),
            (
                Node::File {
                    checksum: Some(digest),
                    ..
                },
                Some(algorithm),
            ) => {
                println!("{line}\n     {} {digest}", algorithm.to_string().dimmed())
            }
            (Node::File { .. }, _) => println!("{line}"),
        }
    }

    println!("\nTotal size: {}", HumanBytes(total_size).to_string().red());
    if let Some(algorithm) = offer.checksums {
        println!("Checksums: {algorithm} for every file, checked as the files arrive.");
    }
    println!();

    // Top-level files show everything there is already.
    let mut choices = vec!["Accept", "Decline"];
    if offer.entries.iter().any(Node::is_dir) {
        choices.push("List All Files");
    }
    loop {
        let choice = dialoguer::Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Accept these files?")
            .items(&choices)
            .default(0)
            .interact()
            .unwrap_or(1);
        match choice {
            0 => return true,
            2 => list_files(offer),
            _ => return false,
        }
    }
}

fn files_in(count: u64) -> String {
    match count {
        1 => "1 file".into(),
        n => format!("{n} files"),
    }
}

/// In `sha256sum` format if it has checksums.
fn list_files(offer: &Offer) {
    println!();
    match checksum::listing(offer) {
        Some(listing) => print!("{listing}"),
        None => {
            for entry in offer.flatten().iter().filter(|entry| !entry.is_dir()) {
                let size = HumanBytes(entry.size()).to_string();
                println!("{size:>10}  {}", entry.path().join("/"));
            }
        }
    }
    println!();
}

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
        ConnectionKind::Direct => name.green().to_string(),
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
    fn files_count_toward_their_offered_entry() {
        let file = |name: &str, size| Node::File {
            name: name.into(),
            size,
            checksum: None,
        };
        let offer = Offer {
            checksums: None,
            entries: vec![
                Node::Dir {
                    name: "d".into(),
                    entries: vec![file("a", 10), file("b", 20)],
                },
                file("c", 5),
            ],
        };
        let mut progress = Progress::new(&offer);
        assert_eq!(progress.files, [(0, 0), (10, 0), (20, 0), (5, 1)]);

        // a is complete, b half there, c new.
        let at = |id, offset| AcceptedFile { id, offset };
        assert_eq!(progress.present(&[at(2, 10), at(3, 0)]), [20, 0]);

        progress.on_event(Event::Started(&[at(2, 10), at(3, 0)]));
        progress.on_event(Event::Progress { id: 2, bytes: 4 });
        progress.on_event(Event::Progress { id: 3, bytes: 5 });
        let bars = &progress.bars.unwrap().bars;
        assert_eq!(bars[0].position(), 24);
        assert_eq!(bars[1].position(), 5);
    }

    #[test]
    fn peer_text_cannot_steer_the_terminal() {
        assert_eq!(
            printable("left\u{1b}[2J\u{7}\nüber"),
            "left\\u{1b}[2J\\u{7}\\u{a}über"
        );
    }

    #[test]
    fn checksums_take_an_optional_algorithm() {
        let parse = |args: &[&str]| {
            let args = ["fsend", "send"].iter().chain(args);
            match Args::try_parse_from(args).map(|args| args.mode) {
                Ok(Mode::Send {
                    files, checksums, ..
                }) => Ok((files.len(), checksums)),
                Ok(_) => unreachable!("a send"),
                Err(e) => Err(e.kind()),
            }
        };
        let sha256 = Some(Algorithm::Sha256);
        assert_eq!(parse(&["a"]), Ok((1, None)));
        // A path after the bare flag is not its value.
        assert_eq!(parse(&["--checksums", "a"]), Ok((1, sha256)));
        assert_eq!(parse(&["a", "--checksums=sha256"]), Ok((1, sha256)));
        assert_eq!(parse(&["--checksums=none", "a"]), Ok((1, None)));
        assert_eq!(
            parse(&["--checksums=md5", "a"]),
            Err(clap::error::ErrorKind::ValueValidation)
        );
    }

    #[test]
    fn the_config_command_takes_the_named_file() {
        let parse = |args: &[&str]| Args::try_parse_from(args).unwrap();
        assert!(matches!(parse(&["fsend", "config"]).mode, Mode::Config));
        let named = parse(&["fsend", "config", "--config", "my.toml"]);
        assert_eq!(named.config, Some(PathBuf::from("my.toml")));
    }

    #[test]
    fn connection_types_keep_their_names() {
        colored::control::set_override(false);
        for (kind, name) in [
            (ConnectionKind::Direct, "Direct"),
            (ConnectionKind::Relay, "Relay"),
            (ConnectionKind::Unknown, "Unknown"),
        ] {
            assert_eq!(colorize_conn_type(kind), name);
        }
    }
}
