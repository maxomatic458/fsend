use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use async_compression::tokio::write::GzipEncoder;
use async_trait::async_trait;
use bytes::Bytes;
use colored::Colorize;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Read/write chunk on the data stream.
pub const BUF_SIZE: usize = 64 * 1024;
const HASH_BUF_SIZE: usize = 1024 * 1024;
pub const PROTO_VERSION: &str = "4";
pub const FSEND_ALPN: &[u8] = b"fsend";
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// How long a incompatible peer gets to read our last packet.
const GOODBYE_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

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

    /// The local files that could be a prefix of this offer.
    pub fn get_skippable(&self, local: &FilesAvailable) -> Option<FilesToSkip> {
        match (self, local) {
            (Self::File { name, size }, Self::File { name: ln, size: ls }) => {
                (name == ln && (1..=*size).contains(ls)).then(|| FilesToSkip::File {
                    name: name.clone(),
                    skip: *ls,
                })
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

/// Whether `name` is a single plain path component that displays as written.
pub fn is_safe_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains('/')
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
}

/// Rejects offers that could escape the output directory or misreport their size.
pub fn check_offer(files: &[FilesAvailable]) -> Result<u64, TransferError> {
    files.iter().try_fold(0u64, |total, file| {
        if !is_safe_name(file.name()) {
            return Err(TransferError::InvalidOffer(format!(
                "unsafe file name \"{}\"",
                file.name().escape_debug()
            )));
        }
        let size = match file {
            FilesAvailable::File { size, .. } => *size,
            FilesAvailable::Dir { files, .. } => check_offer(files)?,
        };
        total
            .checked_add(size)
            .ok_or_else(|| TransferError::InvalidOffer("the total size overflows".into()))
    })
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

/// Local files that may be resumed, pending a prefix hash check.
pub fn resume_candidates(
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

/// Every file in `candidates` as `(path, skip)`, in tree order. `root` maps a
/// top-level entry to its path; anything that does not resolve maps to `None`.
fn candidate_files(
    candidates: &[Option<FilesToSkip>],
    root: impl Fn(usize, &str) -> Option<PathBuf>,
) -> Vec<(Option<PathBuf>, u64)> {
    fn walk(node: &FilesToSkip, path: Option<PathBuf>, out: &mut Vec<(Option<PathBuf>, u64)>) {
        match node {
            FilesToSkip::File { skip, .. } => out.push((path, *skip)),
            FilesToSkip::Dir { files, .. } => {
                for file in files {
                    let child = path
                        .as_ref()
                        .filter(|_| is_safe_name(file.name()))
                        .map(|p| p.join(file.name()));
                    walk(file, child, out);
                }
            }
        }
    }

    let mut out = Vec::new();
    for (i, candidate) in candidates.iter().enumerate() {
        if let Some(candidate) = candidate {
            walk(candidate, root(i, candidate.name()), &mut out);
        }
    }
    out
}

/// Keeps the candidate files whose flag in `keep` (in tree order) is set.
fn retain_files(candidates: Vec<Option<FilesToSkip>>, keep: &[bool]) -> Vec<Option<FilesToSkip>> {
    fn walk(node: FilesToSkip, keep: &mut impl Iterator<Item = bool>) -> Option<FilesToSkip> {
        match node {
            FilesToSkip::File { .. } => keep.next().unwrap_or(false).then_some(node),
            FilesToSkip::Dir { name, files } => {
                let files: Vec<_> = files.into_iter().filter_map(|f| walk(f, keep)).collect();
                (!files.is_empty()).then_some(FilesToSkip::Dir { name, files })
            }
        }
    }

    let mut keep = keep.iter().copied();
    candidates
        .into_iter()
        .map(|candidate| candidate.and_then(|c| walk(c, &mut keep)))
        .collect()
}

/// xxh3 of the first `len` bytes of `path`, or `None` if it has fewer.
pub async fn prefix_hash(path: &Path, len: u64) -> Option<String> {
    let mut file = tokio::fs::File::open(path).await.ok()?.take(len);
    let mut hasher = xxhash_rust::xxh3::Xxh3Default::new();
    let mut buf = vec![0u8; HASH_BUF_SIZE];
    let mut read = 0u64;
    loop {
        let n = file.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        read += n as u64;
    }
    (read == len).then(|| format!("{:016x}", hasher.digest()))
}

async fn prefix_hashes(files: Vec<(Option<PathBuf>, u64)>) -> Vec<Option<String>> {
    let mut hashes = Vec::with_capacity(files.len());
    for (path, len) in files {
        hashes.push(match path {
            Some(path) => prefix_hash(&path, len).await,
            None => None,
        });
    }
    hashes
}

/// Opens a file for receiving.
pub async fn open_for_receive(path: &Path, skip: u64) -> std::io::Result<tokio::fs::File> {
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
    ConnRequest {
        version: String,
    },
    FileInfo {
        files: Vec<FilesAvailable>,
    },
    /// The sender's hash of each resume candidate, in tree order.
    PrefixHashes {
        hashes: Vec<Option<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReceiverToSender {
    WrongVersion {
        expected: String,
    },
    Ok,
    RejectFiles,
    ResumeCandidates {
        files: Vec<Option<FilesToSkip>>,
    },
    AcceptFilesSkip {
        files: Vec<Option<FilesToSkip>>,
    },
    /// Every file is on disk.
    Done,
}

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
    Io(std::io::Error),
    #[error("connection: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("wrong version: expected {0}, got {1}")]
    WrongVersion(String, String),
    #[error("transfer declined")]
    Declined,
    #[error("invalid offer: {0}")]
    InvalidOffer(String),
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
    #[error("the peer did not complete the handshake in time")]
    HandshakeTimeout,
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
            Self::InvalidOffer(reason) => {
                Some(format!("{} refused the offer: {reason}", "error:".red()))
            }
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
            Self::HandshakeTimeout => Some(format!(
                "{} the peer did not answer the handshake. It may run an incompatible version of fsend; update both sides.",
                "error:".red()
            )),
            Self::PeerTimeout => Some(format!(
                "{} the peer did not connect in time. Check that both sides are online and try again.",
                "error:".red()
            )),
            Self::Io(_)
            | Self::Connection(_)
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

/// A stream that reports the peer as gone is a lost connection, not an I/O
/// fault; the transports mark those with `ConnectionAborted`/`NotConnected`.
impl From<std::io::Error> for TransferError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match e.kind() {
            ConnectionAborted | ConnectionReset | NotConnected => Self::Disconnected(e.to_string()),
            _ => Self::Io(e),
        }
    }
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

/// A bidirectional bytestream.
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized> ByteStream for T {}

#[async_trait]
pub trait Transport: Send {
    fn connection_type_name(&self) -> String;
    /// The control stream. Packets are length-prefixed frames of gzipped JSON.
    async fn control(&mut self) -> Result<Box<dyn ByteStream>, TransferError>;
    /// The data stream, opened once the offer has been accepted.
    async fn data(&mut self) -> Result<Box<dyn ByteStream>, TransferError>;
    /// Resolves once the peer has hung up. `Ok` if it did so on purpose.
    async fn closed(&mut self) -> Result<(), TransferError>;
    async fn close(&mut self, ok: bool);
}

pub(crate) type Control = Framed<Box<dyn ByteStream>, LengthDelimitedCodec>;

pub(crate) fn frame(stream: Box<dyn ByteStream>) -> Control {
    Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(MAX_FRAME_BYTES)
            .new_codec(),
    )
}

pub(crate) async fn send_packet<P: Serialize + std::fmt::Debug>(
    control: &mut Control,
    packet: &P,
) -> Result<(), TransferError> {
    tracing::debug!("sending packet: {packet:?}");
    let json = serde_json::to_vec(packet)?;
    let compressed = compress_gzip(&json).await?;
    control.send(Bytes::from(compressed)).await?;
    Ok(())
}

pub(crate) async fn recv_packet<P: DeserializeOwned + std::fmt::Debug>(
    control: &mut Control,
) -> Result<P, TransferError> {
    let frame = control
        .next()
        .await
        .ok_or_else(|| TransferError::Disconnected("the control stream ended".into()))??;
    let json = decompress_gzip(&frame).await?;
    let packet: P = serde_json::from_slice(&json)?;
    tracing::debug!("received packet: {packet:?}");
    Ok(packet)
}

/// Copies `skip..size` of `file` into `stream`.
async fn send_file_data<S, R>(
    stream: &mut S,
    file: &mut R,
    skip: u64,
    size: u64,
    write_cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
    R: AsyncRead + AsyncSeek + Unpin,
{
    file.seek(std::io::SeekFrom::Start(skip)).await?;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut read = skip;
    while read < size {
        let to_read = std::cmp::min(BUF_SIZE as u64, size - read) as usize;
        let n = file.read(&mut buf[..to_read]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected eof",
            ));
        }
        stream.write_all(&buf[..n]).await?;
        read += n as u64;
        write_cb(n as u64);
    }
    Ok(())
}

/// Reads `skip..size` from `stream` into `file`.
async fn receive_file_data<S, W>(
    stream: &mut S,
    file: &mut W,
    skip: u64,
    size: u64,
    read_cb: &mut (dyn FnMut(u64) + Send),
) -> std::io::Result<()>
where
    S: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; BUF_SIZE];
    let mut written = skip;
    while written < size {
        let to_read = std::cmp::min(BUF_SIZE as u64, size - written) as usize;
        let n = stream.read(&mut buf[..to_read]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the stream ended before the file was complete",
            ));
        }
        file.write_all(&buf[..n]).await?;
        written += n as u64;
        read_cb(n as u64);
    }
    Ok(())
}

/// Gives the peer a moment to read our last packet before we close on it.
async fn goodbye(transport: &mut dyn Transport) {
    let _ = tokio::time::timeout(GOODBYE_TIMEOUT, transport.closed()).await;
}

pub async fn send_files(
    transport: &mut dyn Transport,
    args: SendArgs,
    initial_progress_cb: ProgressCb<'_>,
    waiting_cb: WaitingCb<'_>,
    write_cb: DataCb<'_>,
) -> Result<(), TransferError> {
    let result = run_send(transport, args, initial_progress_cb, waiting_cb, write_cb).await;
    transport.close(result.is_ok()).await;
    result
}

async fn run_send(
    transport: &mut dyn Transport,
    args: SendArgs,
    initial_progress_cb: ProgressCb<'_>,
    waiting_cb: WaitingCb<'_>,
    write_cb: DataCb<'_>,
) -> Result<(), TransferError> {
    let mut control = frame(transport.control().await?);

    send_packet(
        &mut control,
        &SenderToReceiver::ConnRequest {
            version: PROTO_VERSION.to_string(),
        },
    )
    .await?;

    let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, recv_packet(&mut control))
        .await
        .map_err(|_| TransferError::HandshakeTimeout)??;
    match reply {
        ReceiverToSender::Ok => {}
        ReceiverToSender::WrongVersion { expected } => {
            return Err(TransferError::WrongVersion(expected, PROTO_VERSION.into()));
        }
        _ => return Err(TransferError::UnexpectedPacket),
    }

    let mut files_available = Vec::new();
    for path in &args.files {
        if !path.exists() {
            return Err(TransferError::FileNotFound(path.clone()));
        }
        files_available.push(get_files_available(path)?);
    }

    send_packet(
        &mut control,
        &SenderToReceiver::FileInfo {
            files: files_available.clone(),
        },
    )
    .await?;

    waiting_cb();

    let mut reply = recv_packet(&mut control).await?;
    if let ReceiverToSender::ResumeCandidates { files } = &reply {
        let paths = candidate_files(files, |i, name| {
            (files_available.get(i)?.name() == name).then(|| args.files[i].clone())
        });
        let hashes = prefix_hashes(paths).await;
        send_packet(&mut control, &SenderToReceiver::PrefixHashes { hashes }).await?;
        reply = recv_packet(&mut control).await?;
    }
    let to_skip = match reply {
        ReceiverToSender::AcceptFilesSkip { files } => files,
        ReceiverToSender::RejectFiles => return Err(TransferError::PeerDeclined),
        _ => return Err(TransferError::UnexpectedPacket),
    };
    if to_skip.len() != files_available.len() {
        return Err(TransferError::UnexpectedPacket);
    }

    let plan = plan_transfer(&files_available, &to_skip);
    initial_progress_cb(&plan.progress);

    let mut data = transport.data().await?;
    for (path, tree) in args.files.iter().zip(plan.to_transfer) {
        let Some(tree) = tree else { continue };
        for entry in flatten_trees(std::slice::from_ref(&tree), path) {
            if let TransferEntry::File { path, skip, size } = entry {
                let mut file = tokio::fs::File::open(&path).await?;
                send_file_data(&mut data, &mut file, skip, size, write_cb).await?;
            }
        }
    }
    data.shutdown().await?;

    match recv_packet(&mut control).await? {
        ReceiverToSender::Done => Ok(()),
        _ => Err(TransferError::UnexpectedPacket),
    }
}

pub async fn receive_files(
    transport: &mut dyn Transport,
    args: ReceiveArgs,
    initial_progress_cb: ProgressCb<'_>,
    accept_files_cb: AcceptFilesCb<'_>,
    read_cb: DataCb<'_>,
) -> Result<(), TransferError> {
    let result = run_receive(
        transport,
        args,
        initial_progress_cb,
        accept_files_cb,
        read_cb,
    )
    .await;
    transport.close(result.is_ok()).await;
    result
}

async fn run_receive(
    transport: &mut dyn Transport,
    args: ReceiveArgs,
    initial_progress_cb: ProgressCb<'_>,
    accept_files_cb: AcceptFilesCb<'_>,
    read_cb: DataCb<'_>,
) -> Result<(), TransferError> {
    let handshake = async {
        let mut control = frame(transport.control().await?);
        let request: SenderToReceiver = recv_packet(&mut control).await?;
        Ok::<_, TransferError>((control, request))
    };
    let (mut control, request) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| TransferError::HandshakeTimeout)??;

    match request {
        SenderToReceiver::ConnRequest { version } => {
            if version != PROTO_VERSION {
                send_packet(
                    &mut control,
                    &ReceiverToSender::WrongVersion {
                        expected: PROTO_VERSION.into(),
                    },
                )
                .await?;
                goodbye(transport).await;
                return Err(TransferError::WrongVersion(PROTO_VERSION.into(), version));
            }
            send_packet(&mut control, &ReceiverToSender::Ok).await?;
        }
        _ => return Err(TransferError::UnexpectedPacket),
    }

    let files_offered = match recv_packet::<SenderToReceiver>(&mut control).await? {
        SenderToReceiver::FileInfo { files } => files,
        _ => return Err(TransferError::UnexpectedPacket),
    };
    if let Err(e) = check_offer(&files_offered) {
        send_packet(&mut control, &ReceiverToSender::RejectFiles).await?;
        goodbye(transport).await;
        return Err(e);
    }

    let output_path = match accept_files_cb(&files_offered) {
        Some(p) => p,
        None => {
            send_packet(&mut control, &ReceiverToSender::RejectFiles).await?;
            goodbye(transport).await;
            return Err(TransferError::Declined);
        }
    };

    let candidates = resume_candidates(&files_offered, &output_path, args.resume);
    let files_to_skip = if candidates.iter().any(Option::is_some) {
        verify_candidates(&mut control, &output_path, candidates).await?
    } else {
        candidates
    };
    let plan = plan_transfer(&files_offered, &files_to_skip);
    initial_progress_cb(&plan.progress);

    send_packet(
        &mut control,
        &ReceiverToSender::AcceptFilesSkip {
            files: files_to_skip,
        },
    )
    .await?;

    let mut data = transport.data().await?;
    tokio::fs::create_dir_all(&output_path).await?;
    for tree in plan.to_transfer.into_iter().flatten() {
        let entries = flatten_trees(std::slice::from_ref(&tree), &output_path.join(tree.name()));
        for entry in entries {
            match entry {
                TransferEntry::Dir { path } => tokio::fs::create_dir_all(path).await?,
                TransferEntry::File { path, skip, size } => {
                    let mut file = open_for_receive(&path, skip).await?;
                    let received =
                        receive_file_data(&mut data, &mut file, skip, size, read_cb).await;
                    // Even a partial file must be on disk before the next session resumes it.
                    file.flush().await?;
                    received?;
                    finish_received_file(&mut file, size).await?;
                }
            }
        }
    }

    send_packet(&mut control, &ReceiverToSender::Done).await?;
    goodbye(transport).await;
    Ok(())
}

/// Keeps the candidates whose local prefix hashes the same as the sender's.
async fn verify_candidates(
    control: &mut Control,
    output_path: &Path,
    candidates: Vec<Option<FilesToSkip>>,
) -> Result<Vec<Option<FilesToSkip>>, TransferError> {
    send_packet(
        control,
        &ReceiverToSender::ResumeCandidates {
            files: candidates.clone(),
        },
    )
    .await?;
    let SenderToReceiver::PrefixHashes { hashes } = recv_packet(control).await? else {
        return Err(TransferError::UnexpectedPacket);
    };

    let local = prefix_hashes(candidate_files(&candidates, |_, name| {
        Some(output_path.join(name))
    }))
    .await;
    if local.len() != hashes.len() {
        return Err(TransferError::UnexpectedPacket);
    }
    let keep: Vec<bool> = local
        .iter()
        .zip(&hashes)
        .map(|(ours, theirs)| ours.is_some() && ours == theirs)
        .collect();
    Ok(retain_files(candidates, &keep))
}

#[cfg(test)]
#[path = "tests/support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "tests/transfer.rs"]
mod tests;
