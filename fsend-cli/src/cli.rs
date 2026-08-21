// Command-line surface.
//
// This module is also `include!`d by `build.rs` to generate shell completions
// and the man page. That means it must stay free of `mod` declarations, of any
// reference to the rest of the crate, and of inner (`//!`) doc comments, which
// are not legal at an `include!` site.

use std::path::PathBuf;

use clap::Parser;

pub const DEFAULT_RELAY_URL: &str = "wss://relay.fsend.sh/ws";
pub const DEFAULT_DOWNLOAD_URL: &str = "https://fsend.sh";

#[derive(Parser)]
#[command(name = "fsend", about, long_about = None)]
pub struct Args {
    /// Log verbosity: error, warn, info, debug or trace.
    #[clap(long, short, default_value = "error")]
    pub log_level: tracing::Level,

    /// Relay server used to introduce the two peers.
    #[clap(long, default_value = DEFAULT_RELAY_URL)]
    pub relay_url: String,

    /// Site the receiver opens. `/receive/<code>` is appended to it.
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
    /// Receive files or folders using a share code or link.
    #[clap(name = "receive", aliases = &["r"])]
    Receive {
        /// Overwrite existing files instead of resuming them.
        #[clap(long, short = 'f')]
        overwrite: bool,

        /// Directory to write the received files into.
        #[clap(long, short, default_value = ".")]
        output_dir: PathBuf,

        /// Share code, or a link such as https://fsend.sh/receive/AB12CD34
        code: String,

        /// Accept the offer without prompting.
        #[clap(long, short = 'y')]
        auto_accept: bool,
    },
    /// Print the CLI, protocol and ALPN versions.
    #[clap(name = "version", aliases = &["v"])]
    Version,
}
