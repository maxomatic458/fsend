use std::path::PathBuf;

use clap::Parser;
use fsend_proto::code::Code;
use url::Url;

pub const DEFAULT_RELAY_URL: &str = "wss://relay.fsend.sh";
pub const DEFAULT_DOWNLOAD_URL: &str = "https://fsend.sh";

#[derive(Parser)]
#[command(name = "fsend", about, long_about = None)]
pub struct Args {
    /// Log verbosity: error, warn, info, debug or trace.
    #[clap(long, short, default_value = "error")]
    pub log_level: tracing::Level,

    /// Relay server used to introduce the two peers. `/relay` is appended to it.
    #[clap(long, default_value = DEFAULT_RELAY_URL, value_parser = websocket_url)]
    pub relay_url: Url,

    /// Site the receiver opens. `/receive#<code>` is appended to it.
    #[clap(long, default_value = DEFAULT_DOWNLOAD_URL, value_parser = web_url)]
    pub download_url: Url,

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

        /// Send the data as is instead of zstd-compressing it where that helps.
        #[clap(long)]
        no_compression: bool,
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
        code: Code,

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

fn url_with_scheme(input: &str, schemes: [&str; 2]) -> Result<Url, String> {
    let url = Url::parse(input).map_err(|e| e.to_string())?;
    if !schemes.contains(&url.scheme()) {
        return Err(format!("expected a {} or {} URL", schemes[0], schemes[1]));
    }
    Ok(url)
}

fn websocket_url(input: &str) -> Result<Url, String> {
    url_with_scheme(input, ["wss", "ws"])
}

fn web_url(input: &str) -> Result<Url, String> {
    url_with_scheme(input, ["https", "http"])
}
