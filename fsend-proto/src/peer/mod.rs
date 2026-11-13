use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub mod frame;
pub mod limits;
pub mod offer;

pub const VERSION: u32 = 4;
pub const MIN_VERSION: u32 = 4;
pub const FSEND_ALPN: &[u8] = b"fsend";

/// What the sender sends. Any other `type` or an unknown value fails to parse; unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SenderMessage {
    /// Sent first.
    Hello(Hello),
    /// The entries on offer; an entry's id is its index.
    Offer {
        entries: Vec<Entry>,
    },
    Cancel(Cancel),
    Error(ErrorReport),
}

/// What the receiver sends, parsed like [`SenderMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ReceiverMessage {
    /// Sent first.
    Hello(Hello),
    /// The files to send, in offer order.
    Accept {
        files: Vec<AcceptedFile>,
    },
    Reject {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        reason: Option<String>,
    },
    /// Bytes written so far, cumulative.
    Ack {
        bytes: u64,
    },
    /// Every accepted file is complete.
    Done,
    Cancel(Cancel),
    Error(ErrorReport),
}

/// A message of either side, as the frame codec carries it.
pub trait Message: Serialize + DeserializeOwned {
    /// Its `type`.
    fn kind(&self) -> &'static str;
}

impl Message for SenderMessage {
    fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Offer { .. } => "offer",
            Self::Cancel(_) => "cancel",
            Self::Error(_) => "error",
        }
    }
}

impl Message for ReceiverMessage {
    fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Accept { .. } => "accept",
            Self::Reject { .. } => "reject",
            Self::Ack { .. } => "ack",
            Self::Done => "done",
            Self::Cancel(_) => "cancel",
            Self::Error(_) => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Hello {
    pub version: u32,
    pub min_version: u32,
    #[serde(default)]
    pub features: Vec<String>,
    /// Receiver only: how many unacknowledged bytes it buffers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub recv_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Cancel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
}

/// The payload of an `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ErrorReport {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Entry {
    File { path: Vec<String>, size: u64 },
    Dir { path: Vec<String> },
}

impl Entry {
    pub fn path(&self) -> &[String] {
        match self {
            Self::File { path, .. } | Self::Dir { path } => path,
        }
    }

    /// Zero for a directory.
    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Dir { .. } => 0,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Dir { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcceptedFile {
    pub id: u32,
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "PeerErrorCode"))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Version,
    Protocol,
    InvalidOffer,
    Io,
    Timeout,
    Internal,
}

/// Our `hello`; only a receiver announces a `recv_window`.
pub fn hello(recv_window: Option<u64>) -> Hello {
    Hello {
        version: VERSION,
        min_version: MIN_VERSION,
        features: Vec::new(),
        recv_window,
    }
}

/// The highest version both our range and the peer's contain.
pub fn negotiate(version: u32, min_version: u32) -> Option<u32> {
    let common = version.min(VERSION);
    (common >= min_version.max(MIN_VERSION)).then_some(common)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::pinned;

    fn cancel(reason: Option<&str>) -> Cancel {
        Cancel {
            reason: reason.map(Into::into),
        }
    }

    fn report() -> ErrorReport {
        ErrorReport {
            code: ErrorCode::InvalidOffer,
            message: "m".into(),
        }
    }

    #[test]
    fn sender_messages_keep_their_wire_format() {
        pinned(
            SenderMessage::Hello(hello(None)),
            r#"{"type":"hello","version":4,"min_version":4,"features":[]}"#,
        );
        pinned(
            SenderMessage::Offer {
                entries: vec![
                    Entry::Dir {
                        path: vec!["d".into()],
                    },
                    Entry::File {
                        path: vec!["d".into(), "a.txt".into()],
                        size: 3,
                    },
                ],
            },
            r#"{"type":"offer","entries":[{"kind":"dir","path":["d"]},{"kind":"file","path":["d","a.txt"],"size":3}]}"#,
        );
        pinned(SenderMessage::Cancel(cancel(None)), r#"{"type":"cancel"}"#);
        pinned(
            SenderMessage::Cancel(cancel(Some("bye"))),
            r#"{"type":"cancel","reason":"bye"}"#,
        );
        pinned(
            SenderMessage::Error(report()),
            r#"{"type":"error","code":"invalid_offer","message":"m"}"#,
        );
    }

    #[test]
    fn receiver_messages_keep_their_wire_format() {
        pinned(
            ReceiverMessage::Hello(hello(Some(1024))),
            r#"{"type":"hello","version":4,"min_version":4,"features":[],"recv_window":1024}"#,
        );
        pinned(
            ReceiverMessage::Accept {
                files: vec![AcceptedFile { id: 1, offset: 2 }],
            },
            r#"{"type":"accept","files":[{"id":1,"offset":2}]}"#,
        );
        pinned(
            ReceiverMessage::Reject { reason: None },
            r#"{"type":"reject"}"#,
        );
        pinned(
            ReceiverMessage::Reject {
                reason: Some("no".into()),
            },
            r#"{"type":"reject","reason":"no"}"#,
        );
        pinned(
            ReceiverMessage::Ack { bytes: 9 },
            r#"{"type":"ack","bytes":9}"#,
        );
        pinned(ReceiverMessage::Done, r#"{"type":"done"}"#);
        pinned(
            ReceiverMessage::Cancel(cancel(None)),
            r#"{"type":"cancel"}"#,
        );
        pinned(
            ReceiverMessage::Cancel(cancel(Some("bye"))),
            r#"{"type":"cancel","reason":"bye"}"#,
        );
        pinned(
            ReceiverMessage::Error(report()),
            r#"{"type":"error","code":"invalid_offer","message":"m"}"#,
        );
    }

    #[test]
    fn unknown_and_wrong_direction_types_fail_to_parse() {
        let from_sender = |json: &str| serde_json::from_str::<SenderMessage>(json);
        let from_receiver = |json: &str| serde_json::from_str::<ReceiverMessage>(json);
        for json in [r#"{"type":"accept","files":[]}"#, r#"{"type":"done"}"#] {
            assert!(from_sender(json).is_err(), "{json}");
        }
        assert!(from_receiver(r#"{"type":"offer","entries":[]}"#).is_err());
        for json in [
            r#"{"type":"resume","x":1}"#,
            r#"{"x":1}"#,
            r#"{"type":"ack"}"#,
            // Unknown values inside a message, too.
            r#"{"type":"error","code":"disk_on_fire","message":"m"}"#,
            r#"{"type":"offer","entries":[{"kind":"link","path":["a"]}]}"#,
        ] {
            assert!(from_sender(json).is_err(), "{json}");
            assert!(from_receiver(json).is_err(), "{json}");
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        assert_eq!(
            serde_json::from_str::<SenderMessage>(
                r#"{"type":"hello","version":5,"min_version":4,"extra":true}"#
            )
            .unwrap(),
            SenderMessage::Hello(Hello {
                version: 5,
                min_version: 4,
                features: vec![],
                recv_window: None,
            })
        );
        assert_eq!(
            serde_json::from_str::<ReceiverMessage>(r#"{"type":"done","extra":1}"#).unwrap(),
            ReceiverMessage::Done
        );
    }

    #[test]
    fn negotiation_picks_the_highest_common_version() {
        assert_eq!(negotiate(4, 4), Some(4));
        // A newer peer that still speaks 4.
        assert_eq!(negotiate(7, 3), Some(4));
        // Too old, or only newer versions.
        assert_eq!(negotiate(3, 1), None);
        assert_eq!(negotiate(9, 5), None);
        // A nonsensical range.
        assert_eq!(negotiate(4, 5), None);
    }
}
