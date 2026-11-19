use serde::{Deserialize, Serialize, de::DeserializeOwned};

use checksum::{Algorithm, Digest};
use resume::Fingerprint;

pub mod block;
pub mod checksum;
pub mod frame;
pub mod limits;
pub mod offer;
pub mod progress;
pub mod resume;

pub const VERSION: u32 = 5;
pub const MIN_VERSION: u32 = 5;
pub const FSEND_ALPN: &[u8] = b"fsend";

/// What the sender sends. Any other `type` or an unknown value fails to parse; unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SenderMessage {
    /// Sent first.
    Hello(Hello),
    /// The fingerprint of each file in the receiver's `check`, in its order.
    Hashes {
        hashes: Vec<Fingerprint>,
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
    /// Files the receiver already has part of, in offer order; at most once, before `accept`.
    Check {
        files: Vec<CheckedFile>,
    },
    /// The files to send, in offer order.
    Accept {
        files: Vec<AcceptedFile>,
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
            Self::Hashes { .. } => "hashes",
            Self::Cancel(_) => "cancel",
            Self::Error(_) => "error",
        }
    }
}

impl Message for ReceiverMessage {
    fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Check { .. } => "check",
            Self::Accept { .. } => "accept",
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

/// What the sender offers: a tree of files and directories. An entry's id is
/// its index in [`Offer::flatten`], each directory before what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Offer {
    /// How the files are checksummed, if they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub checksums: Option<Algorithm>,
    /// The top-level entries.
    pub entries: Vec<Node>,
}

/// An entry of an [`Offer`] as it goes on the wire, a directory with what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Node {
    File {
        name: String,
        size: u64,
        /// Under the offer's algorithm; on every file if it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        checksum: Option<Digest>,
    },
    Dir {
        name: String,
        entries: Vec<Node>,
    },
}

impl Node {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Dir { name, .. } => name,
        }
    }

    /// For a directory, the bytes of every file below it.
    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Dir { entries, .. } => entries.iter().map(Self::size).sum(),
        }
    }

    /// The files it is, or holds.
    pub fn files(&self) -> u64 {
        match self {
            Self::File { .. } => 1,
            Self::Dir { entries, .. } => entries.iter().map(Self::files).sum(),
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Dir { .. })
    }
}

/// An entry of an [`Offer`] by its full path, as [`Offer::flatten`] lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Entry {
    File {
        path: Vec<String>,
        size: u64,
        /// Under the offer's algorithm; on every file if it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        checksum: Option<Digest>,
    },
    Dir {
        path: Vec<String>,
    },
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

    pub fn checksum(&self) -> Option<&Digest> {
        match self {
            Self::File { checksum, .. } => checksum.as_ref(),
            Self::Dir { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcceptedFile {
    pub id: u32,
    pub offset: u64,
}

/// The first `len` bytes of a file, to fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CheckedFile {
    pub id: u32,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "PeerErrorCode"))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Version,
    Protocol,
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
            code: ErrorCode::Protocol,
            message: "m".into(),
        }
    }

    #[test]
    fn sender_messages_keep_their_wire_format() {
        pinned(
            SenderMessage::Hello(hello(None)),
            r#"{"type":"hello","version":5,"min_version":5,"features":[]}"#,
        );
        pinned(
            SenderMessage::Hashes {
                hashes: vec![Fingerprint(0x0123456789abcdef_0011223344556677)],
            },
            r#"{"type":"hashes","hashes":["0123456789abcdef0011223344556677"]}"#,
        );
        pinned(SenderMessage::Cancel(cancel(None)), r#"{"type":"cancel"}"#);
        pinned(
            SenderMessage::Cancel(cancel(Some("bye"))),
            r#"{"type":"cancel","reason":"bye"}"#,
        );
        pinned(
            SenderMessage::Error(report()),
            r#"{"type":"error","code":"protocol","message":"m"}"#,
        );
    }

    #[test]
    fn receiver_messages_keep_their_wire_format() {
        pinned(
            ReceiverMessage::Hello(hello(Some(1024))),
            r#"{"type":"hello","version":5,"min_version":5,"features":[],"recv_window":1024}"#,
        );
        pinned(
            ReceiverMessage::Check {
                files: vec![CheckedFile { id: 1, len: 2 }],
            },
            r#"{"type":"check","files":[{"id":1,"len":2}]}"#,
        );
        pinned(
            ReceiverMessage::Accept {
                files: vec![AcceptedFile { id: 1, offset: 2 }],
            },
            r#"{"type":"accept","files":[{"id":1,"offset":2}]}"#,
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
            r#"{"type":"error","code":"protocol","message":"m"}"#,
        );
    }

    #[test]
    fn unknown_and_wrong_direction_types_fail_to_parse() {
        let from_sender = |json: &str| serde_json::from_str::<SenderMessage>(json);
        let from_receiver = |json: &str| serde_json::from_str::<ReceiverMessage>(json);
        for json in [
            r#"{"type":"accept","files":[]}"#,
            r#"{"type":"check","files":[]}"#,
            r#"{"type":"done"}"#,
        ] {
            assert!(from_sender(json).is_err(), "{json}");
        }
        assert!(from_receiver(r#"{"type":"hashes","hashes":[]}"#).is_err());
        for json in [
            r#"{"type":"resume","x":1}"#,
            r#"{"x":1}"#,
            r#"{"type":"ack"}"#,
            // Unknown values inside a message, too.
            r#"{"type":"error","code":"disk_on_fire","message":"m"}"#,
            // The offer and its answer travel in pairing now.
            r#"{"type":"offer","entries":[]}"#,
            r#"{"type":"reject"}"#,
            r#"{"type":"hashes","hashes":["0123"]}"#,
            r#"{"type":"hashes","hashes":["0123456789ABCDEF0011223344556677"]}"#,
        ] {
            assert!(from_sender(json).is_err(), "{json}");
            assert!(from_receiver(json).is_err(), "{json}");
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        assert_eq!(
            serde_json::from_str::<SenderMessage>(
                r#"{"type":"hello","version":6,"min_version":5,"extra":true}"#
            )
            .unwrap(),
            SenderMessage::Hello(Hello {
                version: 6,
                min_version: 5,
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
    fn nodes_keep_their_wire_format() {
        let checksum = Digest::from(&[0xab; 32][..]);
        let hex = "ab".repeat(32);
        pinned(
            Node::Dir {
                name: "d".into(),
                entries: vec![
                    Node::File {
                        name: "a.txt".into(),
                        size: 3,
                        checksum: None,
                    },
                    Node::Dir {
                        name: "e".into(),
                        entries: vec![],
                    },
                ],
            },
            r#"{"kind":"dir","name":"d","entries":[{"kind":"file","name":"a.txt","size":3},{"kind":"dir","name":"e","entries":[]}]}"#,
        );
        pinned(
            Node::File {
                name: "a".into(),
                size: 3,
                checksum: Some(checksum),
            },
            &format!(r#"{{"kind":"file","name":"a","size":3,"checksum":"{hex}"}}"#),
        );
        assert!(serde_json::from_str::<Node>(r#"{"kind":"link","name":"a"}"#).is_err());
        // A directory lists what it holds, if only nothing.
        assert!(serde_json::from_str::<Node>(r#"{"kind":"dir","name":"d"}"#).is_err());
    }

    #[test]
    fn nodes_sum_up_what_they_hold() {
        let file = |name: &str, size| Node::File {
            name: name.into(),
            size,
            checksum: None,
        };
        let dir = Node::Dir {
            name: "d".into(),
            entries: vec![
                file("a", 3),
                Node::Dir {
                    name: "e".into(),
                    entries: vec![file("b", 4), file("c", 0)],
                },
            ],
        };
        assert_eq!((dir.size(), dir.files()), (7, 3));
        assert_eq!((file("a", 3).size(), file("a", 3).files()), (3, 1));
    }

    #[test]
    fn negotiation_picks_the_highest_common_version() {
        assert_eq!(negotiate(5, 5), Some(5));
        // A newer peer that still speaks 5.
        assert_eq!(negotiate(7, 3), Some(5));
        // Too old, or only newer versions.
        assert_eq!(negotiate(4, 1), None);
        assert_eq!(negotiate(9, 6), None);
        // A nonsensical range.
        assert_eq!(negotiate(5, 6), None);
    }
}
