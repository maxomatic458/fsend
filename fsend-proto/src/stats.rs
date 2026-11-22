use serde::{Deserialize, Serialize};

use crate::relay::Transport;

pub const PATH: &str = "/stats";

/// The app a receiver ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Web,
    Cli,
}

/// How the bytes came: straight from the sender, or through a relay (iroh's or a TURN server).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Direct,
    Relay,
    Unknown,
}

/// What a receiver reports once its transfer ends, with the token it got in `paired`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "StatsReport"))]
pub struct Report {
    pub token: String,
    pub platform: Platform,
    pub transport: Transport,
    pub route: Route,
    /// File bytes received, whether the transfer finished or not.
    pub bytes: u64,
}
