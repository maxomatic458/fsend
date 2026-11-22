use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use fsend_proto::code::Code;
use fsend_proto::peer::checksum::Algorithm;
use url::Url;

#[derive(Parser)]
#[command(name = "fsend", about, long_about = None)]
pub struct Args {
    /// Log verbosity: error, warn, info, debug or trace. Defaults to error.
    #[clap(long, short)]
    pub log_level: Option<tracing::Level>,

    /// Server that introduces the two peers.
    /// Defaults to https://api.fsend.sh.
    #[clap(long, value_parser = web_url)]
    pub server_url: Option<Url>,

    /// The base download URL of the fsend webapp instance.
    /// Defaults to https://fsend.sh.
    #[clap(long, value_parser = web_url)]
    pub download_url: Option<Url>,

    /// Path of the fsend config file.
    /// To print the default path, run `fsend config`.
    #[clap(long, global = true)]
    pub config: Option<PathBuf>,

    #[clap(subcommand)]
    pub mode: Mode,
}

#[derive(clap::Subcommand)]
pub enum Mode {
    /// Send files or folders, printing a share code for pairing.
    #[clap(name = "send", aliases = &["s"])]
    Send {
        /// Files or folders to send.
        #[clap(required = true)]
        files: Vec<PathBuf>,

        /// Send the data as is instead of zstd-compressed.
        #[clap(long)]
        no_compression: bool,

        /// Offer checksums of all files. `sha256` or `none` (default).
        /// This can be set in the config file.
        #[clap(
            long,
            value_name = "ALGORITHM",
            num_args = 0..=1,
            require_equals = true,
            default_value = "none",
            default_missing_value = "sha256",
            value_parser = checksums
        )]
        checksums: std::option::Option<Algorithm>,
    },
    /// Receive files or folders using a share code.
    #[clap(name = "receive", aliases = &["r"])]
    Receive {
        /// What to do with existing files that differ from the offered ones.
        /// Partial copies are always resumed. Defaults to `ask`, or `skip` with `-y`.
        #[clap(long, value_enum)]
        on_conflict: Option<OnConflict>,

        /// Directory to write the received files into. Defaults to the current working directory.
        #[clap(long, short, default_value = ".")]
        output_dir: PathBuf,

        /// Share code, e.g. K7Q29XMV4P
        code: Code,

        /// Accept the offer without prompting.
        #[clap(long, short = 'y')]
        auto_accept: bool,
    },
    /// Print the CLI and protocol versions, and which config file is read.
    #[clap(name = "version", aliases = &["v"])]
    Version,
    /// Print the path of the config file.
    #[clap(name = "config", aliases = &["c"])]
    Config,
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

pub fn checksums(input: &str) -> Result<Option<Algorithm>, String> {
    match input {
        "none" => Ok(None),
        _ => Algorithm::from_str(input, false).map(Some),
    }
}

pub fn web_url(input: &str) -> Result<Url, String> {
    url_with_scheme(input, ["https", "http"])
}
