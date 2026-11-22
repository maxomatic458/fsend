use std::path::{Path, PathBuf};
use std::{fs, io};

use clap::ValueEnum;
use fsend_proto::peer::checksum::Algorithm;
use serde::{Deserialize, Deserializer, de};
use tracing::Level;
use url::Url;

use crate::cli::{self, Args, Mode};

/// Permanent fsend config that overrides the defaults.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(deserialize_with = "level")]
    pub log_level: Level,
    #[serde(deserialize_with = "url")]
    pub server_url: Url,
    #[serde(deserialize_with = "url")]
    pub download_url: Url,
    pub telemetry: bool,
    #[serde(deserialize_with = "checksums")]
    pub checksums: Option<Algorithm>,
}

impl Default for Config {
    fn default() -> Self {
        let url = |url| Url::parse(url).expect("the default URLs are valid");
        Self {
            log_level: Level::ERROR,
            server_url: url("https://api.fsend.sh"), // TODO: use shared constants
            download_url: url("https://fsend.sh"),
            telemetry: false,
            checksums: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read the config file {}: {error}", path.display())]
    Read { path: PathBuf, error: io::Error },
    #[error("the config file {} is invalid: {error}", path.display())]
    Invalid {
        path: PathBuf,
        error: toml::de::Error,
    },
}

pub fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("fsend").join("config.toml"))
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        match path {
            Some(path) => Self::read(path, false),
            None => default_path().map_or(Ok(Self::default()), |path| Self::read(&path, true)),
        }
    }

    fn read(path: &Path, create: bool) -> Result<Self, ConfigError> {
        let path = path.to_owned();
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|error| ConfigError::Invalid { path, error }),
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                let config = Self::default();
                // Only a convenience: without it, the defaults still apply.
                if let Some(dir) = path.parent() {
                    let _ = fs::create_dir_all(dir).and_then(|()| fs::write(&path, config.file()));
                }
                Ok(config)
            }
            Err(error) => Err(ConfigError::Read { path, error }),
        }
    }

    /// With what `args` gives in place of the file's.
    pub fn with_args(self, args: &Args) -> Self {
        Self {
            log_level: args.log_level.unwrap_or(self.log_level),
            server_url: args.server_url.clone().unwrap_or(self.server_url),
            download_url: args.download_url.clone().unwrap_or(self.download_url),
            telemetry: self.telemetry,
            checksums: match args.mode {
                Mode::Send { checksums, .. } => checksums.or(self.checksums),
                _ => self.checksums,
            },
        }
    }

    /// The config file that holds `self`.
    fn file(&self) -> String {
        let checksums = self.checksums.map_or("none".into(), |algorithm| {
            let name = algorithm
                .to_possible_value()
                .expect("every algorithm has a name");
            name.get_name().to_owned()
        });
        format!(
            "# Defaults for fsend.\n\
             log_level = \"{}\"\n\
             server_url = \"{}\"\n\
             download_url = \"{}\"\n\
             # The telemetry collected is minimal and anonymous and cannot be traced back to a user.\n\
             telemetry = {}\n\
             checksums = \"{checksums}\"\n",
            self.log_level.as_str().to_ascii_lowercase(),
            self.server_url,
            self.download_url,
            self.telemetry,
        )
    }
}

fn level<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Level, D::Error> {
    (String::deserialize(deserializer)?.parse())
        .map_err(|_| de::Error::custom("expected error, warn, info, debug or trace"))
}

fn url<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Url, D::Error> {
    cli::web_url(&String::deserialize(deserializer)?).map_err(de::Error::custom)
}

fn checksums<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Algorithm>, D::Error> {
    cli::checksums(&String::deserialize(deserializer)?).map_err(de::Error::custom)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn parse(toml: &str) -> Result<Config, String> {
        toml::from_str(toml).map_err(|e| e.message().to_owned())
    }

    /// The config for `fsend <args> receive`, or for `<args>` if it names a command.
    fn with_args(config: &str, args: &[&str]) -> Config {
        let command = ["send", "receive"].iter().any(|c| args.contains(c));
        let receive: &[&str] = if command {
            &[]
        } else {
            &["receive", "K7Q29XMV4P"]
        };
        let args = ["fsend"].iter().chain(args).chain(receive);
        parse(config).unwrap().with_args(&Args::parse_from(args))
    }

    #[test]
    fn missing_keys_keep_their_defaults() {
        assert_eq!(parse(""), Ok(Config::default()));
        assert_eq!(
            parse(
                r#"
                log_level = "debug"
                server_url = "http://localhost:3001"
                download_url = "https://example.org/fsend"
                telemetry = false
                checksums = "sha256"
                "#
            ),
            Ok(Config {
                log_level: Level::DEBUG,
                server_url: "http://localhost:3001".parse().unwrap(),
                download_url: "https://example.org/fsend".parse().unwrap(),
                telemetry: false,
                checksums: Some(Algorithm::Sha256),
            })
        );
        let only_level = parse(r#"log_level = "info""#).unwrap();
        assert_eq!(only_level.server_url, Config::default().server_url);
    }

    #[test]
    fn values_are_checked_like_the_options() {
        assert_eq!(
            parse(r#"server_url = "ftp://example.org""#),
            Err("expected a https or http URL".into())
        );
        assert_eq!(
            parse(r#"log_level = "loud""#),
            Err("expected error, warn, info, debug or trace".into())
        );
        assert!(parse("telemetry = \"no\"").is_err());
        assert!(parse(r#"checksums = "md5""#).is_err());
        assert_eq!(parse(r#"checksums = "none""#), Ok(Config::default()));
        // A typo would otherwise be ignored.
        assert!(parse("server-url = \"https://example.org\"").is_err());
    }

    #[test]
    fn the_command_line_wins_over_the_file() {
        let file = r#"
            log_level = "info"
            server_url = "https://file.example"
            telemetry = false
            checksums = "sha256"
        "#;
        let from_file = with_args(file, &[]);
        assert_eq!(from_file.log_level, Level::INFO);
        assert_eq!(from_file.server_url.as_str(), "https://file.example/");
        assert_eq!(from_file.download_url, Config::default().download_url);
        assert!(!from_file.telemetry);

        let args = [
            "--log-level",
            "warn",
            "--server-url",
            "https://flag.example",
        ];
        let from_flags = with_args(file, &args);
        assert_eq!(from_flags.log_level, Level::WARN);
        assert_eq!(from_flags.server_url.as_str(), "https://flag.example/");
    }

    #[test]
    fn telemetry_is_off_unless_the_file_turns_it_on() {
        assert!(!with_args("", &[]).telemetry);
        assert!(with_args("telemetry = true", &[]).telemetry);
    }

    #[test]
    fn checksums_come_from_the_file_unless_the_send_names_one() {
        let sha256 = Some(Algorithm::Sha256);
        let file = r#"checksums = "sha256""#;
        assert_eq!(with_args("", &["send", "a"]).checksums, None);
        assert_eq!(
            with_args("", &["send", "--checksums", "a"]).checksums,
            sha256
        );
        assert_eq!(with_args(file, &["send", "a"]).checksums, sha256);
        // `none` is the same as leaving the flag out.
        let none = with_args(file, &["send", "--checksums=none", "a"]);
        assert_eq!(none.checksums, sha256);
    }

    #[test]
    fn a_missing_default_file_is_created_with_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fsend/config.toml");
        assert_eq!(Config::read(&path, true).unwrap(), Config::default());
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(parse(&written), Ok(Config::default()));
        assert!(written.contains("log_level = \"error\""), "{written}");

        // Read, not replaced, from then on.
        fs::write(&path, "telemetry = false").unwrap();
        assert!(!Config::read(&path, true).unwrap().telemetry);
    }

    #[test]
    fn a_named_file_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("config.toml");
        assert!(matches!(
            Config::load(Some(&missing)),
            Err(ConfigError::Read { .. })
        ));
        assert!(!missing.exists());

        fs::write(&missing, "telemetry = 1").unwrap();
        let invalid = Config::load(Some(&missing)).unwrap_err().to_string();
        assert!(invalid.starts_with("the config file "), "{invalid}");
    }
}
