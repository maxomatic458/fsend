use serde::{Deserialize, Serialize};

pub const PROTO_VERSION: &str = "3";
pub const FSEND_ALPN: &[u8] = b"fsend";
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Clone, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FileSendRecvTree {
    File {
        name: String,
        skip: u64,
        size: u64,
    },
    Dir {
        name: String,
        files: Vec<FileSendRecvTree>,
    },
}

impl FileSendRecvTree {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Dir { name, .. } => name,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Dir { files, .. } => files.iter().map(|f| f.size()).sum(),
        }
    }

    pub fn skip(&self) -> u64 {
        match self {
            Self::File { skip, .. } => *skip,
            Self::Dir { files, .. } => files.iter().map(|f| f.skip()).sum(),
        }
    }
}

#[derive(Debug, PartialEq, Clone, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FilesAvailable {
    File {
        name: String,
        size: u64,
    },
    Dir {
        name: String,
        files: Vec<FilesAvailable>,
    },
}

impl FilesAvailable {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Dir { name, .. } => name,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Dir { files, .. } => files.iter().map(|f| f.size()).sum(),
        }
    }

    pub fn to_send_recv_tree(&self) -> FileSendRecvTree {
        match self {
            Self::File { name, size } => FileSendRecvTree::File {
                name: name.clone(),
                skip: 0,
                size: *size,
            },
            Self::Dir { name, files } => FileSendRecvTree::Dir {
                name: name.clone(),
                files: files.iter().map(|f| f.to_send_recv_tree()).collect(),
            },
        }
    }

    pub fn remove_skipped(&self, to_skip: &FilesToSkip) -> Option<FileSendRecvTree> {
        match (self, to_skip) {
            (Self::File { name, size }, FilesToSkip::File { name: sn, skip }) => {
                if name == sn && size <= skip {
                    None
                } else {
                    Some(FileSendRecvTree::File {
                        name: name.clone(),
                        skip: if name == sn { *skip } else { 0 },
                        size: *size,
                    })
                }
            }
            (
                Self::Dir { name, files },
                FilesToSkip::Dir {
                    name: sn,
                    files: sf,
                },
            ) => {
                if name != sn {
                    return Some(self.to_send_recv_tree());
                }
                let mut remaining = Vec::new();
                for file in files {
                    if let Some(skip_file) = sf.iter().find(|s| s.name() == file.name()) {
                        if let Some(r) = file.remove_skipped(skip_file) {
                            remaining.push(r);
                        }
                    } else {
                        remaining.push(file.to_send_recv_tree());
                    }
                }
                if remaining.is_empty() {
                    None
                } else {
                    Some(FileSendRecvTree::Dir {
                        name: name.clone(),
                        files: remaining,
                    })
                }
            }
            // A file offered where a directory exists locally (e.g file.txt offered, but file.txt/ exists, or vice versa)
            // cannot be resumed; transfer it in full.
            _ => Some(self.to_send_recv_tree()),
        }
    }

    pub fn get_skippable(&self, local: &FilesAvailable) -> Option<FilesToSkip> {
        match (self, local) {
            (Self::File { name, size }, Self::File { name: ln, size: ls }) => {
                if name == ln {
                    Some(FilesToSkip::File {
                        name: name.clone(),
                        skip: if ls > size { 0 } else { *ls },
                    })
                } else {
                    None
                }
            }
            (
                Self::Dir { name, files },
                Self::Dir {
                    name: ln,
                    files: lf,
                },
            ) => {
                if name != ln {
                    return None;
                }
                let mut skippable = Vec::new();
                for file in files {
                    if let Some(lm) = lf.iter().find(|l| l.name() == file.name())
                        && let Some(s) = file.get_skippable(lm)
                    {
                        skippable.push(s);
                    }
                }
                if skippable.is_empty() {
                    None
                } else {
                    Some(FilesToSkip::Dir {
                        name: name.clone(),
                        files: skippable,
                    })
                }
            }
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Clone, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FilesToSkip {
    File {
        name: String,
        skip: u64,
    },
    Dir {
        name: String,
        files: Vec<FilesToSkip>,
    },
}

impl FilesToSkip {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Dir { name, .. } => name,
        }
    }

    pub fn skip(&self) -> u64 {
        match self {
            Self::File { skip, .. } => *skip,
            Self::Dir { files, .. } => files.iter().map(|f| f.skip()).sum(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SenderToReceiver {
    ConnRequest { version: String },
    FileInfo { files: Vec<FilesAvailable> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReceiverToSender {
    WrongVersion { expected: String },
    Ok,
    RejectFiles,
    AcceptFilesSkip { files: Vec<Option<FilesToSkip>> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::pinned;

    fn file(name: &str, size: u64) -> FilesAvailable {
        FilesAvailable::File {
            name: name.into(),
            size,
        }
    }

    fn dir(name: &str, files: Vec<FilesAvailable>) -> FilesAvailable {
        FilesAvailable::Dir {
            name: name.into(),
            files,
        }
    }

    fn skip_file(name: &str, skip: u64) -> FilesToSkip {
        FilesToSkip::File {
            name: name.into(),
            skip,
        }
    }

    fn skip_dir(name: &str, files: Vec<FilesToSkip>) -> FilesToSkip {
        FilesToSkip::Dir {
            name: name.into(),
            files,
        }
    }

    #[test]
    fn sizes_sum_over_the_tree() {
        let tree = dir(
            "d",
            vec![file("a", 10), dir("e", vec![file("b", 5), file("c", 0)])],
        );
        assert_eq!(tree.size(), 15);
        assert_eq!(tree.name(), "d");

        let send = tree.to_send_recv_tree();
        assert_eq!(send.size(), 15);
        assert_eq!(send.skip(), 0);
        assert_eq!(send.name(), "d");
    }

    #[test]
    fn to_send_recv_tree_starts_every_file_at_zero() {
        let tree = dir("d", vec![file("a", 10), dir("e", vec![file("b", 5)])]);
        assert_eq!(
            tree.to_send_recv_tree(),
            FileSendRecvTree::Dir {
                name: "d".into(),
                files: vec![
                    FileSendRecvTree::File {
                        name: "a".into(),
                        skip: 0,
                        size: 10
                    },
                    FileSendRecvTree::Dir {
                        name: "e".into(),
                        files: vec![FileSendRecvTree::File {
                            name: "b".into(),
                            skip: 0,
                            size: 5
                        }],
                    },
                ],
            }
        );
    }

    #[test]
    fn skippable_file_resumes_from_local_size() {
        assert_eq!(
            file("a", 100).get_skippable(&file("a", 40)),
            Some(skip_file("a", 40))
        );
        assert_eq!(file("a", 100).get_skippable(&file("b", 40)), None);
        // A file where a directory is offered, or the reverse, is not resumable.
        assert_eq!(file("a", 100).get_skippable(&dir("a", vec![])), None);
        assert_eq!(dir("a", vec![]).get_skippable(&file("a", 1)), None);
    }

    #[test]
    fn skippable_file_larger_than_the_offer_starts_over() {
        // A bigger local file cannot be a prefix of the offer.
        assert_eq!(
            file("a", 100).get_skippable(&file("a", 150)),
            Some(skip_file("a", 0))
        );
        // Exactly complete is skipped in full.
        assert_eq!(
            file("a", 100).get_skippable(&file("a", 100)),
            Some(skip_file("a", 100))
        );
    }

    #[test]
    fn skippable_dir_only_lists_files_present_locally() {
        let offered = dir(
            "d",
            vec![
                file("a", 10),
                file("missing", 10),
                dir("e", vec![file("b", 5), file("c", 7)]),
                dir("untouched", vec![file("x", 1)]),
            ],
        );
        let local = dir(
            "d",
            vec![
                file("a", 4),
                file("extra", 99),
                dir("e", vec![file("c", 7)]),
            ],
        );
        assert_eq!(
            offered.get_skippable(&local),
            Some(skip_dir(
                "d",
                vec![skip_file("a", 4), skip_dir("e", vec![skip_file("c", 7)])]
            ))
        );
        // Nothing in common means nothing to skip.
        assert_eq!(offered.get_skippable(&dir("d", vec![file("zzz", 1)])), None);
        assert_eq!(
            offered.get_skippable(&dir("other", vec![file("a", 4)])),
            None
        );
    }

    #[test]
    fn remove_skipped_drops_complete_files_and_keeps_offsets() {
        assert_eq!(file("a", 100).remove_skipped(&skip_file("a", 100)), None);
        assert_eq!(file("a", 100).remove_skipped(&skip_file("a", 200)), None);
        assert_eq!(
            file("a", 100).remove_skipped(&skip_file("a", 30)),
            Some(FileSendRecvTree::File {
                name: "a".into(),
                skip: 30,
                size: 100
            })
        );
        // An empty local copy of an empty file is already complete.
        assert_eq!(file("a", 0).remove_skipped(&skip_file("a", 0)), None);
    }

    #[test]
    fn remove_skipped_over_a_tree() {
        let offered = dir(
            "d",
            vec![
                file("done", 10),
                file("partial", 10),
                file("fresh", 10),
                dir("e", vec![file("b", 5)]),
                dir("f", vec![file("c", 5)]),
            ],
        );
        let skip = skip_dir(
            "d",
            vec![
                skip_file("done", 10),
                skip_file("partial", 4),
                skip_dir("e", vec![skip_file("b", 5)]),
            ],
        );
        let remaining = offered.remove_skipped(&skip).unwrap();
        assert_eq!(
            remaining,
            FileSendRecvTree::Dir {
                name: "d".into(),
                files: vec![
                    FileSendRecvTree::File {
                        name: "partial".into(),
                        skip: 4,
                        size: 10
                    },
                    FileSendRecvTree::File {
                        name: "fresh".into(),
                        skip: 0,
                        size: 10
                    },
                    FileSendRecvTree::Dir {
                        name: "f".into(),
                        files: vec![FileSendRecvTree::File {
                            name: "c".into(),
                            skip: 0,
                            size: 5
                        }],
                    },
                ],
            }
        );
        assert_eq!(remaining.skip(), 4);
        assert_eq!(remaining.size(), 25);

        // Everything already present. the whole directory is dropped.
        let all = skip_dir("e", vec![skip_file("b", 5)]);
        assert_eq!(dir("e", vec![file("b", 5)]).remove_skipped(&all), None);
    }

    #[test]
    fn remove_skipped_with_mismatched_shapes_sends_everything() {
        let offered = dir("d", vec![file("a", 3)]);
        assert_eq!(
            offered.remove_skipped(&skip_file("d", 3)),
            Some(offered.to_send_recv_tree())
        );
        assert_eq!(
            offered.remove_skipped(&skip_dir("other", vec![])),
            Some(offered.to_send_recv_tree())
        );
        assert_eq!(
            file("a", 3).remove_skipped(&skip_file("b", 3)),
            Some(FileSendRecvTree::File {
                name: "a".into(),
                skip: 0,
                size: 3
            })
        );
    }

    #[test]
    fn sender_messages_keep_their_wire_format() {
        pinned(
            SenderToReceiver::ConnRequest {
                version: PROTO_VERSION.into(),
            },
            r#"{"type":"ConnRequest","version":"3"}"#,
        );
        pinned(
            SenderToReceiver::FileInfo {
                files: vec![file("a.txt", 3), dir("d", vec![file("b", 1)])],
            },
            r#"{"type":"FileInfo","files":[{"type":"File","name":"a.txt","size":3},{"type":"Dir","name":"d","files":[{"type":"File","name":"b","size":1}]}]}"#,
        );
    }

    #[test]
    fn receiver_messages_keep_their_wire_format() {
        pinned(
            ReceiverToSender::WrongVersion {
                expected: "3".into(),
            },
            r#"{"type":"WrongVersion","expected":"3"}"#,
        );
        pinned(ReceiverToSender::Ok, r#"{"type":"Ok"}"#);
        pinned(ReceiverToSender::RejectFiles, r#"{"type":"RejectFiles"}"#);
        pinned(
            ReceiverToSender::AcceptFilesSkip {
                files: vec![Some(skip_dir("d", vec![skip_file("b", 1)])), None],
            },
            r#"{"type":"AcceptFilesSkip","files":[{"type":"Dir","name":"d","files":[{"type":"File","name":"b","skip":1}]},null]}"#,
        );
    }
}
