use std::path::{Path, PathBuf};

use async_compression::tokio::write::GzipEncoder;
use async_trait::async_trait;
use colored::Colorize;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

pub const BUF_SIZE: usize = 8192;
pub const PROTO_VERSION: &str = "0.1.0";
pub const FSEND_ALPN: &[u8] = b"fsend/0.2.0";

#[derive(
    Debug, PartialEq, Clone, Hash, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
#[serde(tag = "type")]
#[rkyv(
    serialize_bounds(__S: rkyv::ser::Writer + rkyv::ser::Allocator, __S::Error: rkyv::rancor::Source),
    deserialize_bounds(__D::Error: rkyv::rancor::Source),
    bytecheck(bounds(__C: rkyv::validation::ArchiveContext, __C::Error: rkyv::rancor::Source)),
)]
pub enum FileSendRecvTree {
    File {
        name: String,
        skip: u64,
        size: u64,
    },
    Dir {
        name: String,
        #[rkyv(omit_bounds)]
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

#[derive(
    Debug, PartialEq, Clone, Hash, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
#[serde(tag = "type")]
#[rkyv(
    serialize_bounds(__S: rkyv::ser::Writer + rkyv::ser::Allocator, __S::Error: rkyv::rancor::Source),
    deserialize_bounds(__D::Error: rkyv::rancor::Source),
    bytecheck(bounds(__C: rkyv::validation::ArchiveContext, __C::Error: rkyv::rancor::Source)),
)]
pub enum FilesAvailable {
    File {
        name: String,
        size: u64,
    },
    Dir {
        name: String,
        #[rkyv(omit_bounds)]
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
                    if let Some(lm) = lf.iter().find(|l| l.name() == file.name()) {
                        if let Some(s) = file.get_skippable(lm) {
                            skippable.push(s);
                        }
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

/// The name a path is announced under.
fn entry_name(path: &Path) -> std::io::Result<String> {
    let name = match path.file_name() {
        Some(name) => name.to_owned(),
        None => std::fs::canonicalize(path)?
            .file_name()
            .map(|n| n.to_owned())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("cannot send {}: it has no name", path.display()),
                )
            })?,
    };
    Ok(name.to_string_lossy().into_owned())
}

pub fn get_files_available(path: &Path) -> std::io::Result<FilesAvailable> {
    let name = entry_name(path)?;
    if path.is_file() {
        Ok(FilesAvailable::File {
            name,
            size: path.metadata()?.len(),
        })
    } else {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(path)? {
            files.push(get_files_available(&entry?.path())?);
        }
        Ok(FilesAvailable::Dir { name, files })
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum TransferEntry {
    Dir { path: PathBuf },
    File { path: PathBuf, skip: u64, size: u64 },
}

/// Flattens the trees into the order of the wire bytes.
pub fn flatten_trees(trees: &[FileSendRecvTree], root: &Path) -> Vec<TransferEntry> {
    fn walk(tree: &FileSendRecvTree, parent: &Path, out: &mut Vec<TransferEntry>) {
        match tree {
            FileSendRecvTree::File { name, skip, size } => out.push(TransferEntry::File {
                path: parent.join(name),
                skip: *skip,
                size: *size,
            }),
            FileSendRecvTree::Dir { name, files } => {
                let dir = parent.join(name);
                out.push(TransferEntry::Dir { path: dir.clone() });
                for file in files {
                    walk(file, &dir, out);
                }
            }
        }
    }

    let mut out = Vec::new();
    for tree in trees {
        match tree {
            FileSendRecvTree::File { skip, size, .. } => out.push(TransferEntry::File {
                path: root.to_path_buf(),
                skip: *skip,
                size: *size,
            }),
            FileSendRecvTree::Dir { files, .. } => {
                out.push(TransferEntry::Dir {
                    path: root.to_path_buf(),
                });
                for file in files {
                    walk(file, root, &mut out);
                }
            }
        }
    }
    out
}

/// What the receiver keeps of the offer, after skip is applied.
pub struct TransferPlan {
    pub to_transfer: Vec<Option<FileSendRecvTree>>,
    /// `(name, bytes present locally, total bytes)` per offered entry.
    pub progress: Vec<(String, u64, u64)>,
}

pub fn plan_transfer(offered: &[FilesAvailable], to_skip: &[Option<FilesToSkip>]) -> TransferPlan {
    let to_transfer = offered
        .iter()
        .zip(to_skip)
        .map(|(file, skip)| match skip {
            Some(s) => file.remove_skipped(s),
            None => Some(file.to_send_recv_tree()),
        })
        .collect();

    let progress = offered
        .iter()
        .zip(to_skip)
        .map(|(file, skip)| {
            (
                file.name().to_string(),
                skip.as_ref().map(|s| s.skip()).unwrap_or(0),
                file.size(),
            )
        })
        .collect();

    TransferPlan {
        to_transfer,
        progress,
    }
}

/// What the receiver want's to skip based on whats locally available.
pub fn skip_list(
    offered: &[FilesAvailable],
    output_dir: &Path,
    resume: bool,
) -> Vec<Option<FilesToSkip>> {
    if !resume {
        return vec![None; offered.len()];
    }
    offered
        .iter()
        .map(|offer| {
            get_files_available(&output_dir.join(offer.name()))
                .ok()
                .and_then(|local| offer.get_skippable(&local))
        })
        .collect()
}

/// Opens a file for receiving.
pub async fn open_for_receive(path: &Path, skip: u64) -> std::io::Result<tokio::fs::File> {
    use tokio::io::AsyncSeekExt;

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await?;
    file.seek(std::io::SeekFrom::Start(skip)).await?;
    Ok(file)
}

/// Finishes a received file.
pub async fn finish_received_file(file: &mut tokio::fs::File, size: u64) -> std::io::Result<()> {
    file.set_len(size).await?;
    file.sync_all().await?;
    file.shutdown().await
}

#[derive(
    Debug, PartialEq, Clone, Hash, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
#[serde(tag = "type")]
#[rkyv(
    serialize_bounds(__S: rkyv::ser::Writer + rkyv::ser::Allocator, __S::Error: rkyv::rancor::Source),
    deserialize_bounds(__D::Error: rkyv::rancor::Source),
    bytecheck(bounds(__C: rkyv::validation::ArchiveContext, __C::Error: rkyv::rancor::Source)),
)]
pub enum FilesToSkip {
    File {
        name: String,
        skip: u64,
    },
    Dir {
        name: String,
        #[rkyv(omit_bounds)]
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

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
#[serde(tag = "type")]
pub enum SenderToReceiver {
    ConnRequest { version: String },
    FileInfo { files: Vec<FilesAvailable> },
}

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
#[serde(tag = "type")]
pub enum ReceiverToSender {
    WrongVersion { expected: String },
    Ok,
    RejectFiles,
    AcceptFilesSkip { files: Vec<Option<FilesToSkip>> },
}

/// Binary encoding of the control packets on the iroh transport (rkyv).
pub trait WirePacket: Sized + std::fmt::Debug {
    fn to_wire(&self) -> Result<Vec<u8>, rkyv::rancor::Error>;
    fn from_wire(bytes: &[u8]) -> Result<Self, rkyv::rancor::Error>;
}

macro_rules! impl_wire_packet {
    ($($t:ty),* $(,)?) => {$(
        impl WirePacket for $t {
            fn to_wire(&self) -> Result<Vec<u8>, rkyv::rancor::Error> {
                rkyv::to_bytes::<rkyv::rancor::Error>(self).map(|b| b.to_vec())
            }

            fn from_wire(bytes: &[u8]) -> Result<Self, rkyv::rancor::Error> {
                let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
                aligned.extend_from_slice(bytes);
                rkyv::from_bytes::<Self, rkyv::rancor::Error>(&aligned)
            }
        }
    )*};
}

impl_wire_packet!(SenderToReceiver, ReceiverToSender);

pub async fn compress_gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = GzipEncoder::new(&mut out);
    encoder.write_all(data).await?;
    encoder.shutdown().await?;
    Ok(out)
}

pub async fn decompress_gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = async_compression::tokio::write::GzipDecoder::new(&mut out);
    decoder.write_all(data).await?;
    decoder.shutdown().await?;
    Ok(out)
}

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("connection: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("packet: {0}")]
    Packet(#[from] PacketError),
    #[error("wrong version: expected {0}, got {1}")]
    WrongVersion(String, String),
    #[error("transfer declined")]
    Declined,
    #[error("the peer declined the files")]
    PeerDeclined,
    #[error("{} of the given paths do not exist", .0.len())]
    PathsNotFound(Vec<PathBuf>),
    #[error("unexpected packet")]
    UnexpectedPacket,
    #[error("file does not exist: {0}")]
    FileNotFound(PathBuf),
    #[error("connection to the peer lost: {0}")]
    Disconnected(String),
    #[error("bind: {0}")]
    Bind(#[from] iroh::endpoint::BindError),
    #[error("connect: {0}")]
    Connect(#[from] iroh::endpoint::ConnectError),
    #[error("accept: {0}")]
    Accept(#[from] iroh::endpoint::ConnectingError),
    #[error("node id: {0}")]
    NodeId(#[from] iroh::KeyParsingError),
    #[error("the peer did not connect in time")]
    PeerTimeout,
    #[error("the endpoint closed while waiting for the peer")]
    EndpointClosed,
    #[error("the relay handed over connection info for another transport")]
    UnexpectedConnectionInfo,
    #[error("webrtc: {0}")]
    WebRtc(#[from] webrtc::error::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Traffic from the peer that the framing does not allow.
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("internal: {0}")]
    Internal(&'static str),
}

impl TransferError {
    /// Generate a user-facing error messages for user-facing "errors"
    pub fn user_error(&self) -> Option<String> {
        match self {
            Self::Declined => Some("Transfer declined.".to_owned()),
            Self::PeerDeclined => Some(format!("{} the peer declined the files", "error:".red())),
            Self::Disconnected(reason) => Some(format!(
                "{} the connection to the peer was lost ({reason}).\n\
                 Start a new session to resume the transfer where it stopped.",
                "error:".red()
            )),
            Self::WrongVersion(expected, got) => Some(format!(
                "{} protocol version mismatch: this client speaks {got}, the peer expects {expected}.\n\
                 Update fsend on both sides.",
                "error:".red()
            )),
            Self::PathsNotFound(paths) => {
                let noun = if paths.len() == 1 {
                    "path does"
                } else {
                    "paths do"
                };
                let list: String = paths
                    .iter()
                    .map(|p| format!("\n - {}", p.display().to_string().blue()))
                    .collect();

                Some(format!(
                    "{} {} {} not exist:{}",
                    "error:".red(),
                    paths.len(),
                    noun,
                    list
                ))
            }
            Self::PeerTimeout => Some(format!(
                "{} the peer did not connect in time. Check that both sides are online and try again.",
                "error:".red()
            )),
            Self::Io(_)
            | Self::Connection(_)
            | Self::Packet(_)
            | Self::UnexpectedPacket
            | Self::FileNotFound(_)
            | Self::Bind(_)
            | Self::Connect(_)
            | Self::Accept(_)
            | Self::NodeId(_)
            | Self::EndpointClosed
            | Self::UnexpectedConnectionInfo
            | Self::WebRtc(_)
            | Self::Json(_)
            | Self::Protocol(_)
            | Self::Internal(_) => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum PacketError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(rkyv::rancor::Error),
    #[error("decode: {0}")]
    Decode(rkyv::rancor::Error),
    #[error("connection: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("read: {0}")]
    Read(#[from] iroh::endpoint::ReadToEndError),
    #[error("write: {0}")]
    Write(#[from] iroh::endpoint::WriteError),
    #[error("stream closed before the packet was delivered")]
    Stopped,
}

pub struct SendArgs {
    pub files: Vec<PathBuf>,
}

pub struct ReceiveArgs {
    pub resume: bool,
}

pub type ProgressCb<'a> = &'a mut (dyn FnMut(&[(String, u64, u64)]) + Send);
pub type WaitingCb<'a> = &'a mut (dyn FnMut() + Send);
pub type DataCb<'a> = &'a mut (dyn FnMut(u64) + Send);
pub type AcceptFilesCb<'a> = &'a mut (dyn FnMut(&[FilesAvailable]) -> Option<PathBuf> + Send);

#[async_trait]
pub trait Transfer: Send {
    async fn connection_type_name(&self) -> String;

    async fn send_files(
        &mut self,
        args: SendArgs,
        initial_progress_cb: ProgressCb<'_>,
        waiting_cb: WaitingCb<'_>,
        write_cb: DataCb<'_>,
    ) -> Result<(), TransferError>;

    async fn receive_files(
        &mut self,
        args: ReceiveArgs,
        initial_progress_cb: ProgressCb<'_>,
        accept_files_cb: AcceptFilesCb<'_>,
        read_cb: DataCb<'_>,
    ) -> Result<(), TransferError>;

    async fn close(&mut self);
}

#[cfg(test)]
#[path = "tests/support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "tests/transfer.rs"]
mod tests;
