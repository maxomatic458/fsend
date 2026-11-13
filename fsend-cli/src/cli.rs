// Command-line surface.

use std::path::PathBuf;

use clap::Parser;

pub const DEFAULT_RELAY_URL: &str = "wss://relay.fsend.sh";
pub const DEFAULT_DOWNLOAD_URL: &str = "https://fsend.sh";

#[derive(Parser)]
#[command(name = "fsend", about, long_about = None)]
pub struct Args {
    /// Log verbosity: error, warn, info, debug or trace.
    #[clap(long, short, default_value = "error")]
    pub log_level: tracing::Level,

    /// Relay server used to introduce the two peers. `/relay` is appended to it.
    #[clap(long, default_value = DEFAULT_RELAY_URL)]
    pub relay_url: String,

    /// Site the receiver opens. `/receive#<code>` is appended to it.
    #[clap(long, default_value = DEFAULT_DOWNLOAD_URL)]
    pub download_url: String,

    #[clap(subcommand)]
    pub mode: Mode,
}

#[derive(clap::Subcommand)]
pub enum Mode {
    /// Send files or folders, printing a share code for the receiver.
    #[clap(name = "send", aliases = &["s"])]
    Send {
        /// Files or folders to send.
        #[clap(required = true)]
        files: Vec<PathBuf>,
    },
    /// Receive files or folders using a share code.
    #[clap(name = "receive", aliases = &["r"])]
    Receive {
        /// What to do with existing files that differ from the offered ones.
        /// Partial copies are always resumed. Defaults to `ask`, or `skip` with `-y`.
        #[clap(long, value_enum)]
        on_conflict: Option<OnConflict>,

        /// Directory to write the received files into.
        #[clap(long, short, default_value = ".")]
        output_dir: PathBuf,

        /// Share code, e.g. K7Q29XMV4P
        code: String,

        /// Accept the offer without prompting.
        #[clap(long, short = 'y')]
        auto_accept: bool,
    },
    /// Print the CLI and protocol versions.
    #[clap(name = "version", aliases = &["v"])]
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OnConflict {
    Ask,
    Skip,
    Replace,
}
