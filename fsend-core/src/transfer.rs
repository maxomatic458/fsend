use std::fmt::Debug;
use std::io;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fsend_proto::peer::block::{self, BlockDecoder};
use fsend_proto::peer::checksum::{Algorithm, Hasher};
use fsend_proto::peer::frame::{self, FrameDecoder};
use fsend_proto::peer::limits::{
    ACK_INTERVAL, ACK_STEP, GOODBYE_TIMEOUT, HELLO_TIMEOUT, IDLE_TIMEOUT, MAX_BLOCK_BYTES,
    MAX_DEPTH, MIN_RECV_WINDOW, RECV_WINDOW,
};
use fsend_proto::peer::offer::{self, EntryError, OfferError};
use fsend_proto::peer::progress::{self, Progress, TopEntry};
use fsend_proto::peer::resume::{self, Conflict, ConflictPolicy, Fingerprint, Local};
use fsend_proto::peer::{
    self, AcceptedFile, Cancel, CheckedFile, Entry, ErrorCode, ErrorReport, Hello, Message, Node,
    Offer, ReceiverMessage, SenderMessage,
};
use futures_util::StreamExt;
use futures_util::stream::FuturesOrdered;
use thiserror::Error;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::checksum;

/// Chunk size for stream reads and writes.
pub(crate) const BUF_SIZE: usize = 64 * 1024;
/// Data blocks compressed at once.
const COMPRESS_WORKERS: usize = 4;

const SENDER_HELLO: &str = "the sender did not say hello in time";
const RECEIVER_HELLO: &str = "the receiver did not say hello in time";
const SENDER_IDLE: &str = "the sender stopped sending";
const RECEIVER_IDLE: &str = "the receiver stopped responding";

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("io: {0}")]
    Io(io::Error),
    #[error("connection to the peer lost: {0}")]
    Disconnected(String),
    #[error("{} of the given paths do not exist", .0.len())]
    PathsNotFound(Vec<PathBuf>),
    /// The selection breaks the offer rules.
    #[error("cannot send these files: {0}")]
    Unsendable(OfferError),
    #[error(
        "no common protocol version: the peer speaks {} to {}, this client {} to {}",
        .min_version, .version, peer::MIN_VERSION, peer::VERSION
    )]
    Version { version: u32, min_version: u32 },
    /// We declined the offer.
    #[error("transfer declined")]
    Declined,
    /// We cancelled the transfer.
    #[error("transfer cancelled")]
    Cancelled,
    #[error("the peer declined the files")]
    PeerDeclined(Option<String>),
    #[error("the peer cancelled the transfer")]
    PeerCancelled(Option<String>),
    #[error("the peer failed ({code:?}): {message}")]
    Peer { code: ErrorCode, message: String },
    /// A received file is not what its checksum says; the peer is not told.
    #[error("{} does not match its checksum", .0.display())]
    ChecksumMismatch(PathBuf),
    /// Traffic from the peer that the protocol does not allow.
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("bind: {0}")]
    Bind(#[from] iroh::endpoint::BindError),
    #[error("connect: {0}")]
    Connect(#[from] iroh::endpoint::ConnectError),
    #[error("node id: {0}")]
    NodeId(#[from] iroh::KeyParsingError),
    #[error("the peer did not connect in time")]
    PeerTimeout,
    #[error("the endpoint closed while waiting for the peer")]
    EndpointClosed,
    #[error("the peer sent connection info for another transport")]
    UnexpectedConnectionInfo,
    #[error("webrtc: {0}")]
    WebRtc(#[from] webrtc::error::Error),
    #[error("internal: {0}")]
    Internal(&'static str),
}

impl TransferError {
    /// A message for the errors a user can act on.
    pub fn user_message(&self) -> Option<String> {
        let update = "Update fsend on both sides.";
        Some(match self {
            Self::Declined
            | Self::Cancelled
            | Self::PeerDeclined(_)
            | Self::PeerCancelled(_)
            | Self::Unsendable(_)
            | Self::Timeout(_) => self.to_string(),
            Self::Disconnected(reason) => format!(
                "the connection to the peer was lost ({reason}).\n\
                 Start a new session to resume the transfer where it stopped."
            ),
            Self::Version { .. } => format!("{self}.\n{update}"),
            Self::Peer {
                code: ErrorCode::Version,
                message,
            } => format!("the peer speaks another protocol version ({message}).\n{update}"),
            Self::Peer { message, .. } => format!("the peer failed: {message}"),
            Self::PathsNotFound(paths) => {
                let list: String = paths
                    .iter()
                    .map(|p| format!("\n - {}", p.display()))
                    .collect();
                format!("{}:{list}", missing_paths(paths.len()))
            }
            Self::PeerTimeout => {
                "the peer did not connect in time. Check that both sides are online and try again."
                    .to_owned()
            }
            Self::ChecksumMismatch(path) => format!(
                "{} does not match its checksum. Delete it to receive it again.",
                path.display()
            ),
            Self::Io(_)
            | Self::Protocol(_)
            | Self::Bind(_)
            | Self::Connect(_)
            | Self::NodeId(_)
            | Self::EndpointClosed
            | Self::UnexpectedConnectionInfo
            | Self::WebRtc(_)
            | Self::Internal(_) => return None,
        })
    }

    /// The `error` that tells the peer about a failure on this side.
    fn report(&self) -> Option<ErrorReport> {
        let (code, message) = match self {
            Self::Io(e) => (ErrorCode::Io, e.to_string()),
            Self::Version { .. } => (
                ErrorCode::Version,
                format!(
                    "versions {} to {} are supported",
                    peer::MIN_VERSION,
                    peer::VERSION
                ),
            ),
            Self::Protocol(message) => (ErrorCode::Protocol, message.clone()),
            Self::Timeout(what) => (ErrorCode::Timeout, what.to_string()),
            Self::Internal(what) => (ErrorCode::Internal, what.to_string()),
            _ => return None,
        };
        Some(ErrorReport { code, message })
    }
}

/// "`n` paths do not exist", for a list of the paths to follow.
pub fn missing_paths(n: usize) -> String {
    let noun = if n == 1 { "path does" } else { "paths do" };
    format!("{n} {noun} not exist")
}

/// A QUIC connection that fails has lost the peer.
impl From<iroh::endpoint::ConnectionError> for TransferError {
    fn from(e: iroh::endpoint::ConnectionError) -> Self {
        Self::Disconnected(e.to_string())
    }
}

/// The transports report a peer that is gone as `ConnectionAborted`, `ConnectionReset` or `NotConnected`.
impl From<io::Error> for TransferError {
    fn from(e: io::Error) -> Self {
        use io::ErrorKind::*;
        match e.kind() {
            ConnectionAborted | ConnectionReset | NotConnected => Self::Disconnected(e.to_string()),
            _ => Self::Io(e),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// Receiver: `done` of the `total` bytes already here are checked against
    /// the offer's checksums, before the transfer starts.
    Verifying { done: u64, total: u64 },
    /// The offer was accepted; `present` counts the bytes of each entry already there.
    Started {
        entries: &'a [TopEntry],
        present: &'a [u64],
    },
    /// `bytes` more of the entry at `entry` moved.
    Progress { entry: usize, bytes: u64 },
}

pub type EventCb<'a> = &'a mut (dyn FnMut(Event<'_>) + Send);

/// What to do with the entries of the offer that conflict with local ones,
/// which may take as long as a user does.
#[async_trait]
pub trait Resolve: Send {
    async fn resolve(&mut self, entries: &[Entry], conflicts: &[Conflict]) -> ConflictPolicy;
}

/// Answers right away.
#[async_trait]
impl<F> Resolve for F
where
    F: FnMut(&[Entry], &[Conflict]) -> ConflictPolicy + Send,
{
    async fn resolve(&mut self, entries: &[Entry], conflicts: &[Conflict]) -> ConflictPolicy {
        self(entries, conflicts)
    }
}

/// How the peers are connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionKind {
    Direct,
    Relay,
    WebRtc,
    Unknown,
}

impl std::fmt::Display for ConnectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Direct => "Direct",
            Self::Relay => "Relay",
            Self::WebRtc => "WebRTC",
            Self::Unknown => "Unknown",
        })
    }
}

/// A bidirectional bytestream.
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized> ByteStream for T {}

#[async_trait]
pub trait Transport: Send {
    fn connection_kind(&self) -> ConnectionKind;
    /// The control stream, carrying peer protocol frames.
    async fn control(&mut self) -> Result<Box<dyn ByteStream>, TransferError>;
    /// The data stream, opened once the offer has been accepted.
    async fn data(&mut self) -> Result<Box<dyn ByteStream>, TransferError>;
    /// Resolves once the peer has hung up. `Ok` if it did so on purpose.
    async fn closed(&mut self) -> Result<(), TransferError>;
    async fn close(&mut self, ok: bool);
}

/// The offer for the paths to send, and where each entry comes from.
#[derive(Debug, Default)]
pub struct Outgoing {
    pub offer: Offer,
    /// By entry id.
    pub(crate) sources: Vec<PathBuf>,
    /// The plaintext of the sealed offer.
    pub(crate) encoded: Vec<u8>,
}

/// Walks `paths` into a valid offer.
pub fn build_offer(paths: &[PathBuf]) -> Result<Outgoing, TransferError> {
    let missing: Vec<_> = paths.iter().filter(|p| !p.exists()).cloned().collect();
    if !missing.is_empty() {
        return Err(TransferError::PathsNotFound(missing));
    }
    let mut outgoing = Outgoing::default();
    for path in paths {
        let node = outgoing.add(path, entry_name(path)?, 1)?;
        outgoing.offer.entries.push(node);
    }
    outgoing.encode()?;
    Ok(outgoing)
}

impl Outgoing {
    /// Encodes the offer, once it is valid.
    pub(crate) fn encode(&mut self) -> Result<(), TransferError> {
        self.encoded = offer::encode(&self.offer).map_err(TransferError::Unsendable)?;
        Ok(())
    }

    /// Bytes of all its files.
    pub fn size(&self) -> u64 {
        self.offer.entries.iter().map(Node::size).sum()
    }

    /// The node of `source`, named `name` at `depth` components, with all
    /// below it; their sources follow in offer order.
    fn add(&mut self, source: &Path, name: String, depth: usize) -> Result<Node, TransferError> {
        // Symlinked directories can loop.
        if depth > MAX_DEPTH {
            return Err(TransferError::Unsendable(OfferError::Entry {
                id: self.sources.len(),
                reason: EntryError::TooDeep,
            }));
        }
        let metadata = std::fs::metadata(source)?;
        self.sources.push(source.to_path_buf());
        if metadata.is_file() {
            return Ok(Node::File {
                name,
                size: metadata.len(),
                checksum: None,
            });
        }
        let mut children = std::fs::read_dir(source)?.collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(|child| child.file_name());
        let entries = children
            .iter()
            .map(|child| {
                let name = child.file_name().to_string_lossy().into_owned();
                self.add(&child.path(), name, depth + 1)
            })
            .collect::<Result<_, _>>()?;
        Ok(Node::Dir { name, entries })
    }
}

/// The name a path is announced under.
fn entry_name(path: &Path) -> io::Result<String> {
    let name = match path.file_name() {
        Some(name) => name.to_owned(),
        None => std::fs::canonicalize(path)?
            .file_name()
            .map(|n| n.to_owned())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("cannot send {}: it has no name", path.display()),
                )
            })?,
    };
    Ok(name.to_string_lossy().into_owned())
}

/// Where an offered path lands under `root`; each name must be a plain file name here.
pub(crate) fn local_path(root: &Path, path: &[String]) -> io::Result<PathBuf> {
    let mut out = root.to_path_buf();
    for name in path {
        let mut components = Path::new(name).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(_)), None) => out.push(name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name:?} is not a valid file name on this system"),
                ));
            }
        }
    }
    Ok(out)
}

/// Opens a file for receiving; at offset 0 whatever was there is dropped.
pub(crate) async fn open_for_receive(path: &Path, offset: u64) -> io::Result<File> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(offset == 0)
        .open(path)
        .await?;
    if offset > 0 {
        file.seek(io::SeekFrom::Start(offset)).await?;
    }
    Ok(file)
}

/// The fingerprint of the first `len` bytes of each file.
async fn fingerprints(files: Vec<(PathBuf, u64)>) -> io::Result<Vec<Fingerprint>> {
    tokio::task::spawn_blocking(move || {
        files
            .iter()
            .map(|(path, len)| fingerprint(path, *len))
            .collect()
    })
    .await
    .map_err(io::Error::other)?
}

fn fingerprint(path: &Path, len: u64) -> io::Result<Fingerprint> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path)?;
    let mut sampled = Vec::new();
    for (start, end) in Fingerprint::ranges(len) {
        file.seek(io::SeekFrom::Start(start))?;
        let want = end - start;
        if (&mut file).take(want).read_to_end(&mut sampled)? as u64 != want {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{} is shorter than {len} bytes", path.display()),
            ));
        }
    }
    Ok(Fingerprint::of(&sampled))
}

/// Finishes a received file.
pub(crate) async fn finish_received_file(file: &mut File, size: u64) -> io::Result<()> {
    file.set_len(size).await?;
    file.shutdown().await
}

/// A message the peer sends.
pub(crate) trait PeerMessage: Message + Debug {
    /// What it means where the flow expects another one.
    fn unexpected(self) -> TransferError;
    /// Whether it is the peer's last word.
    fn is_final(&self) -> bool;
}

impl PeerMessage for SenderMessage {
    fn unexpected(self) -> TransferError {
        match self {
            Self::Error(report) => report.into(),
            Self::Cancel(Cancel { reason }) => TransferError::PeerCancelled(reason),
            other => TransferError::Protocol(format!("unexpected {} message", other.kind())),
        }
    }

    fn is_final(&self) -> bool {
        matches!(self, Self::Cancel(_) | Self::Error(_))
    }
}

impl PeerMessage for ReceiverMessage {
    fn unexpected(self) -> TransferError {
        match self {
            Self::Error(report) => report.into(),
            Self::Cancel(Cancel { reason }) => TransferError::PeerCancelled(reason),
            other => TransferError::Protocol(format!("unexpected {} message", other.kind())),
        }
    }

    fn is_final(&self) -> bool {
        matches!(self, Self::Done | Self::Cancel(_) | Self::Error(_))
    }
}

impl From<ErrorReport> for TransferError {
    fn from(ErrorReport { code, message }: ErrorReport) -> Self {
        Self::Peer { code, message }
    }
}

/// The control stream, one message per frame: we send `Out`, the peer sends `In`.
pub(crate) struct Control<Out, In> {
    stream: Box<dyn ByteStream>,
    decoder: FrameDecoder,
    buf: Vec<u8>,
    messages: PhantomData<fn(Out) -> In>,
}

pub(crate) type SenderControl = Control<SenderMessage, ReceiverMessage>;
pub(crate) type ReceiverControl = Control<ReceiverMessage, SenderMessage>;

impl<Out: Message + Debug, In: PeerMessage> Control<Out, In> {
    pub fn new(stream: Box<dyn ByteStream>) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::default(),
            buf: vec![0; BUF_SIZE],
            messages: PhantomData,
        }
    }

    pub async fn send(&mut self, message: &Out) -> Result<(), TransferError> {
        tracing::debug!("sending {message:?}");
        let frame = frame::encode(message)
            .map_err(|_| TransferError::Internal("a message does not fit in a frame"))?;
        self.send_frame(&frame).await
    }

    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), TransferError> {
        self.stream.write_all(frame).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// The next message; one the peer may not send is a protocol error. Cancel safe.
    pub async fn recv(&mut self) -> Result<In, TransferError> {
        loop {
            match self.decoder.next_message() {
                Ok(Some(message)) => {
                    tracing::debug!("received {message:?}");
                    return Ok(message);
                }
                Ok(None) => {
                    let n = self.stream.read(&mut self.buf).await?;
                    if n == 0 {
                        return Err(TransferError::Disconnected(
                            "the control stream ended".into(),
                        ));
                    }
                    self.decoder.push(&self.buf[..n]);
                }
                Err(e) => return Err(TransferError::Protocol(e.to_string())),
            }
        }
    }

    async fn recv_within(
        &mut self,
        limit: Duration,
        what: &'static str,
    ) -> Result<In, TransferError> {
        timeout(limit, self.recv())
            .await
            .map_err(|_| TransferError::Timeout(what))?
    }
}

/// Checks that the peer's `hello` leaves a common version.
fn check_version(hello: &Hello) -> Result<(), TransferError> {
    match peer::negotiate(hello.version, hello.min_version) {
        Some(_) => Ok(()),
        None => Err(TransferError::Version {
            version: hello.version,
            min_version: hello.min_version,
        }),
    }
}

/// Gives the peer a moment to read our last message, until it hangs up or says its last word.
async fn goodbye<Out: Message + Debug, In: PeerMessage>(
    transport: &mut dyn Transport,
    control: &mut Control<Out, In>,
) {
    let last_word = async {
        loop {
            match control.recv().await {
                Ok(message) if message.is_final() => return,
                Ok(_) => {}
                // A broken stream has nothing more to say; wait for the hang-up.
                Err(TransferError::Protocol(_)) => std::future::pending().await,
                Err(_) => return,
            }
        }
    };
    let _ = timeout(GOODBYE_TIMEOUT, async {
        tokio::select! {
            _ = transport.closed() => {}
            () = last_word => {}
        }
    })
    .await;
}

/// Tells the peer why `result` failed, if it should know, as `error(report)`
/// or as `cancel`.
async fn explain<Out: Message + Debug, In: PeerMessage>(
    transport: &mut dyn Transport,
    control: &mut Control<Out, In>,
    result: Result<(), TransferError>,
    error: fn(ErrorReport) -> Out,
    cancel: Out,
) -> Result<(), TransferError> {
    let last_word = match &result {
        Ok(()) => None,
        Err(TransferError::Cancelled) => Some(cancel),
        Err(failure) => failure.report().map(error),
    };
    if let Some(message) = last_word
        && control.send(&message).await.is_ok()
    {
        goodbye(transport, control).await;
    }
    result
}

/// `work`, unless `cancel` fires first.
async fn cancellable<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T, TransferError>>,
) -> Result<T, TransferError> {
    cancel
        .run_until_cancelled(work)
        .await
        .unwrap_or(Err(TransferError::Cancelled))
}

/// Bytes of the accepted files still to move.
fn remaining(entries: &[Entry], files: &[AcceptedFile]) -> u64 {
    files
        .iter()
        .map(|file| entries[file.id as usize].size() - file.offset)
        .sum()
}

/// Sends `outgoing`, zstd-compressing the data where that helps if `compress`.
/// Once `cancel` fires, the receiver is told and the transfer ends.
pub async fn send_files(
    transport: &mut dyn Transport,
    outgoing: &Outgoing,
    compress: bool,
    on_event: EventCb<'_>,
    cancel: CancellationToken,
) -> Result<(), TransferError> {
    let result = match cancellable(&cancel, transport.control()).await {
        Ok(stream) => {
            let mut control = Control::new(stream);
            let result = cancellable(
                &cancel,
                send(
                    transport,
                    &mut control,
                    &outgoing.offer.flatten(),
                    &outgoing.sources,
                    compress,
                    on_event,
                ),
            )
            .await;
            let cancel = SenderMessage::Cancel(Cancel { reason: None });
            explain(
                transport,
                &mut control,
                result,
                SenderMessage::Error,
                cancel,
            )
            .await
        }
        Err(e) => Err(e),
    };
    transport.close(result.is_ok()).await;
    result
}

async fn send(
    transport: &mut dyn Transport,
    control: &mut SenderControl,
    entries: &[Entry],
    sources: &[PathBuf],
    compress: bool,
    on_event: EventCb<'_>,
) -> Result<(), TransferError> {
    control
        .send(&SenderMessage::Hello(peer::hello(None)))
        .await?;
    let hello = match control.recv_within(HELLO_TIMEOUT, RECEIVER_HELLO).await? {
        ReceiverMessage::Hello(hello) => hello,
        other => return Err(other.unexpected()),
    };
    check_version(&hello)?;
    let size = hello
        .recv_window
        .filter(|size| *size >= MIN_RECV_WINDOW)
        .ok_or_else(|| TransferError::Protocol("the receiver announced no usable window".into()))?;
    let mut window = Window {
        size,
        sent: 0,
        acked: 0,
    };

    let mut answer = control.recv().await?;
    if let ReceiverMessage::Check { files } = answer {
        offer::check_lengths(entries, &files)
            .map_err(|e| TransferError::Protocol(e.to_string()))?;
        let sources = files
            .iter()
            .map(|file| (sources[file.id as usize].clone(), file.len))
            .collect();
        let hashes = fingerprints(sources).await?;
        control.send(&SenderMessage::Hashes { hashes }).await?;
        answer = control.recv().await?;
    }
    let files = match answer {
        ReceiverMessage::Accept { files } => files,
        other => return Err(other.unexpected()),
    };
    offer::check_accept(entries, &files).map_err(|e| TransferError::Protocol(e.to_string()))?;
    let (tops, top_of) = progress::top_entries(entries);
    on_event(Event::Started {
        entries: &tops,
        present: &progress::present(entries, &files),
    });

    let mut reader = BlockReader::new(entries, sources, &files);
    let mut progress = Progress::new(entries, &files, &top_of);
    if !reader.is_done() {
        let mut data = transport.data().await?;
        // Blocks in order, compressed a few at a time.
        let mut encoding = FuturesOrdered::new();
        loop {
            while encoding.len() < COMPRESS_WORKERS && !reader.is_done() && window.room() > 0 {
                let bytes = reader.read(window.room()).await?;
                window.sent += bytes.len() as u64;
                encoding.push_back(tokio::task::spawn_blocking(move || {
                    (block::encode(&bytes, compress), bytes.len() as u64)
                }));
            }
            let Some(encoded) = encoding.next().await else {
                if reader.is_done() {
                    break;
                }
                window.ack(control.recv_within(IDLE_TIMEOUT, RECEIVER_IDLE).await?)?;
                continue;
            };
            let (frame, raw) =
                encoded.map_err(|_| TransferError::Internal("encoding a block failed"))?;
            // The block's file bytes count in proportion to its bytes written.
            let mut counted = 0;
            let mut on_written = |written: usize| {
                let now = raw * written as u64 / frame.len() as u64;
                for (entry, bytes) in progress.advance(now - counted) {
                    on_event(Event::Progress { entry, bytes });
                }
                counted = now;
            };
            let write = window.write(data.as_mut(), &frame, control, &mut on_written);
            match timeout(IDLE_TIMEOUT, write).await {
                Ok(Ok(())) => {}
                // The receiver may say why it stopped the data stream.
                Ok(Err(lost @ TransferError::Disconnected(_))) => {
                    return Err(window.last_word(control, lost).await);
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(TransferError::Timeout(RECEIVER_IDLE)),
            }
        }
        timeout(IDLE_TIMEOUT, data.shutdown())
            .await
            .map_err(|_| TransferError::Timeout(RECEIVER_IDLE))??;
    }

    loop {
        match control.recv_within(IDLE_TIMEOUT, RECEIVER_IDLE).await? {
            ReceiverMessage::Done => return Ok(()),
            message => window.ack(message)?,
        }
    }
}

/// Reads the accepted bytes of the offered files in order, a block at a time.
struct BlockReader<'a> {
    entries: &'a [Entry],
    sources: &'a [PathBuf],
    files: std::slice::Iter<'a, AcceptedFile>,
    /// The file being read and the bytes left in it.
    current: Option<(File, u64)>,
    left: u64,
}

impl<'a> BlockReader<'a> {
    fn new(entries: &'a [Entry], sources: &'a [PathBuf], files: &'a [AcceptedFile]) -> Self {
        Self {
            entries,
            sources,
            files: files.iter(),
            current: None,
            left: remaining(entries, files),
        }
    }

    fn is_done(&self) -> bool {
        self.left == 0
    }

    /// Up to `max` (and a block's worth of) bytes.
    async fn read(&mut self, max: u64) -> io::Result<Vec<u8>> {
        let len = max.min(self.left).min(MAX_BLOCK_BYTES as u64) as usize;
        let mut bytes = vec![0; len];
        let mut filled = 0;
        while filled < len {
            let (file, left) = match &mut self.current {
                Some((file, left)) if *left > 0 => (file, left),
                _ => {
                    let next = self.files.next().expect("bytes are left");
                    let id = next.id as usize;
                    let mut file = File::open(&self.sources[id]).await?;
                    if next.offset > 0 {
                        file.seek(io::SeekFrom::Start(next.offset)).await?;
                    }
                    let left = self.entries[id].size() - next.offset;
                    self.current = Some((file, left));
                    continue;
                }
            };
            let n = (*left).min((len - filled) as u64);
            file.read_exact(&mut bytes[filled..filled + n as usize])
                .await?;
            filled += n as usize;
            *left -= n;
        }
        self.left -= len as u64;
        Ok(bytes)
    }
}

/// Keeps the bytes the receiver has not acknowledged within its window.
struct Window {
    size: u64,
    sent: u64,
    acked: u64,
}

impl Window {
    fn room(&self) -> u64 {
        self.size - (self.sent - self.acked)
    }

    /// Writes `frame` to the data stream a piece at a time, taking acks as
    /// they arrive and telling `on_written` how much of it is written.
    async fn write(
        &mut self,
        data: &mut dyn ByteStream,
        frame: &[u8],
        control: &mut SenderControl,
        on_written: &mut (dyn FnMut(usize) + Send),
    ) -> Result<(), TransferError> {
        let mut written = 0;
        while written < frame.len() {
            let piece = &frame[written..frame.len().min(written + BUF_SIZE)];
            tokio::select! {
                n = data.write(piece) => match n? {
                    0 => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                    n => {
                        written += n;
                        on_written(written);
                    }
                },
                message = control.recv() => self.ack(message?)?,
            }
        }
        Ok(())
    }

    /// What the receiver says past its acks, or `lost` if it says nothing in time.
    async fn last_word(
        &mut self,
        control: &mut SenderControl,
        lost: TransferError,
    ) -> TransferError {
        let said = async {
            loop {
                match control.recv().await {
                    Ok(message) => {
                        if let Err(e) = self.ack(message) {
                            return Some(e);
                        }
                    }
                    Err(_) => return None,
                }
            }
        };
        timeout(GOODBYE_TIMEOUT, said)
            .await
            .ok()
            .flatten()
            .unwrap_or(lost)
    }

    fn ack(&mut self, message: ReceiverMessage) -> Result<(), TransferError> {
        match message {
            ReceiverMessage::Ack { bytes } if (self.acked..=self.sent).contains(&bytes) => {
                self.acked = bytes;
                Ok(())
            }
            ReceiverMessage::Ack { .. } => Err(TransferError::Protocol(
                "an ack outside the bytes sent".into(),
            )),
            other => Err(other.unexpected()),
        }
    }
}

/// Receives the `offer`, which pairing brought, into `out`; `resolve` picks
/// what happens to local entries in the way. Once `cancel` fires, the sender
/// is told and the transfer ends.
pub async fn receive_files(
    transport: &mut dyn Transport,
    offer: &Offer,
    out: &Path,
    resolve: &mut dyn Resolve,
    on_event: EventCb<'_>,
    cancel: CancellationToken,
) -> Result<(), TransferError> {
    // Over iroh the stream only shows up once the sender has written to it.
    let control = async {
        timeout(HELLO_TIMEOUT, transport.control())
            .await
            .map_err(|_| TransferError::Timeout(SENDER_HELLO))?
    };
    let result = match cancellable(&cancel, control).await {
        Ok(stream) => {
            let mut control = Control::new(stream);
            let receive = receive(transport, &mut control, offer, out, resolve, on_event);
            let result = cancellable(&cancel, receive).await;
            let cancel = ReceiverMessage::Cancel(Cancel { reason: None });
            explain(
                transport,
                &mut control,
                result,
                ReceiverMessage::Error,
                cancel,
            )
            .await
        }
        Err(e) => Err(e),
    };
    transport.close(result.is_ok()).await;
    result
}

async fn receive(
    transport: &mut dyn Transport,
    control: &mut ReceiverControl,
    offer: &Offer,
    out: &Path,
    resolve: &mut dyn Resolve,
    on_event: EventCb<'_>,
) -> Result<(), TransferError> {
    let entries = &offer.flatten();
    control
        .send(&ReceiverMessage::Hello(peer::hello(Some(RECV_WINDOW))))
        .await?;
    match control.recv_within(HELLO_TIMEOUT, SENDER_HELLO).await? {
        SenderMessage::Hello(hello) => check_version(&hello)?,
        other => return Err(other.unexpected()),
    }
    let (tops, top_of) = progress::top_entries(entries);

    tokio::fs::create_dir_all(out).await?;
    let paths = entries
        .iter()
        .map(|entry| local_path(out, entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    let local = local_entries(&paths).await?;
    let checked = resume::to_check(entries, &local);
    let verified = if checked.is_empty() {
        Vec::new()
    } else {
        verify(control, &paths, checked).await?
    };
    let plan = resume::plan(entries, &local, &verified);
    let policy = match plan.conflicts.is_empty() {
        true => ConflictPolicy::Skip,
        false => resolve.resolve(entries, &plan.conflicts).await,
    };
    let files = plan.accept(policy);
    let mut hashers = match offer.checksums {
        Some(algorithm) => {
            let present = Present {
                entries,
                paths: &paths,
                complete: &plan.complete,
                files: &files,
            };
            Some(present.check(algorithm, on_event).await?.into_iter())
        }
        None => None,
    };
    control
        .send(&ReceiverMessage::Accept {
            files: files.clone(),
        })
        .await?;
    on_event(Event::Started {
        entries: &tops,
        present: &progress::present(entries, &files),
    });

    for &id in &plan.dirs {
        tokio::fs::create_dir_all(&paths[id as usize]).await?;
    }
    let mut incoming = if remaining(entries, &files) > 0 {
        let stream = tokio::select! {
            stream = timeout(IDLE_TIMEOUT, transport.data()) => {
                stream.map_err(|_| TransferError::Timeout(SENDER_IDLE))??
            }
            message = control.recv() => return Err(message?.unexpected()),
        };
        Some(Incoming::new(stream))
    } else {
        None
    };
    let mut progress = Progress::new(entries, &files, &top_of);
    for &AcceptedFile { id, offset } in &files {
        let id = id as usize;
        let size = entries[id].size();
        let mut file = open_for_receive(&paths[id], offset).await?;
        let mut hasher = hashers.as_mut().and_then(Iterator::next);
        let received = match &mut incoming {
            Some(incoming) => {
                let mut on_arrived = |bytes| {
                    for (entry, bytes) in progress.advance(bytes) {
                        on_event(Event::Progress { entry, bytes });
                    }
                };
                let len = size - offset;
                incoming
                    .write_to(&mut file, len, control, &mut on_arrived, hasher.as_mut())
                    .await
            }
            None => Ok(()),
        };
        // Hands even a partial file to the OS, for a later session to resume.
        let flushed = file.flush().await;
        received?;
        flushed?;
        finish_received_file(&mut file, size).await?;
        if let Some(hasher) = hasher
            && Some(&hasher.finish()) != entries[id].checksum()
        {
            return Err(TransferError::ChecksumMismatch(paths[id].clone()));
        }
    }
    if incoming.is_some_and(|incoming| !incoming.is_empty()) {
        return Err(TransferError::Protocol(
            "the sender sent more bytes than accepted".into(),
        ));
    }

    control.send(&ReceiverMessage::Done).await?;
    goodbye(transport, control).await;
    Ok(())
}

/// What the receiver already has of an offer, by the plan for it.
struct Present<'a> {
    entries: &'a [Entry],
    paths: &'a [PathBuf],
    /// Files already complete.
    complete: &'a [u32],
    /// The accepted files, each from its offset.
    files: &'a [AcceptedFile],
}

impl Present<'_> {
    /// Checks the complete files against their checksums, and hashes what is
    /// here of each accepted file; returns a hasher per accepted file to go on with.
    async fn check(
        &self,
        algorithm: Algorithm,
        on_event: EventCb<'_>,
    ) -> Result<Vec<Hasher>, TransferError> {
        let path = |id: u32| self.paths[id as usize].clone();
        let jobs: Vec<_> = (self.complete.iter())
            .map(|&id| (path(id), self.entries[id as usize].size()))
            .chain(self.files.iter().map(|file| (path(file.id), file.offset)))
            .collect();
        let total = jobs.iter().map(|(_, len)| len).sum();
        let mut on_progress = |done| {
            if total > 0 {
                on_event(Event::Verifying { done, total });
            }
        };
        let mut hashers = checksum::hash_files(algorithm, jobs, &mut on_progress).await?;
        let resumed = hashers.split_off(self.complete.len());
        for (&id, hasher) in self.complete.iter().zip(hashers) {
            if Some(&hasher.finish()) != self.entries[id as usize].checksum() {
                return Err(TransferError::ChecksumMismatch(path(id)));
            }
        }
        Ok(resumed)
    }
}

/// What is already at each path, looked up in one blocking task.
async fn local_entries(paths: &[PathBuf]) -> io::Result<Vec<Local>> {
    let paths = paths.to_vec();
    tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|path| match std::fs::metadata(path) {
                Err(_) => Local::Missing,
                Ok(m) if m.is_file() => Local::File { size: m.len() },
                Ok(m) if m.is_dir() => Local::Dir,
                Ok(_) => Local::Other,
            })
            .collect()
    })
    .await
    .map_err(io::Error::other)
}

/// Asks the sender for fingerprints of the `checked` files; returns the ids of those that match ours.
async fn verify(
    control: &mut ReceiverControl,
    paths: &[PathBuf],
    checked: Vec<CheckedFile>,
) -> Result<Vec<u32>, TransferError> {
    control
        .send(&ReceiverMessage::Check {
            files: checked.clone(),
        })
        .await?;
    let hashes = match control.recv().await? {
        SenderMessage::Hashes { hashes } if hashes.len() == checked.len() => hashes,
        SenderMessage::Hashes { .. } => {
            return Err(TransferError::Protocol(
                "the hashes do not match the check".into(),
            ));
        }
        other => return Err(other.unexpected()),
    };
    let ours = fingerprints(
        checked
            .iter()
            .map(|file| (paths[file.id as usize].clone(), file.len))
            .collect(),
    )
    .await?;
    Ok(checked
        .iter()
        .zip(ours.iter().zip(&hashes))
        .filter(|(_, (ours, theirs))| ours == theirs)
        .map(|(file, _)| file.id)
        .collect())
}

/// The receiving end of the data stream, acking what it wrote.
struct Incoming {
    stream: Box<dyn ByteStream>,
    buf: Vec<u8>,
    decoder: BlockDecoder,
    /// The block being written, and how much of it is.
    block: Vec<u8>,
    taken: usize,
    /// File bytes of the blocks decoded so far, and of those counted as arrived.
    decoded: u64,
    arrived: u64,
    written: u64,
    acked: u64,
    acked_at: Instant,
}

impl Incoming {
    fn new(stream: Box<dyn ByteStream>) -> Self {
        Self {
            stream,
            buf: vec![0; BUF_SIZE],
            decoder: BlockDecoder::default(),
            block: Vec::new(),
            taken: 0,
            decoded: 0,
            arrived: 0,
            written: 0,
            acked: 0,
            acked_at: Instant::now(),
        }
    }

    /// Whether nothing past the accepted bytes arrived.
    fn is_empty(&self) -> bool {
        self.taken == self.block.len() && self.decoder.is_empty()
    }

    /// Decodes the next block once the current one is written, telling
    /// `on_arrived` about file bytes as their blocks arrive, in proportion.
    async fn fill(
        &mut self,
        control: &mut ReceiverControl,
        on_arrived: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), TransferError> {
        while self.taken == self.block.len() {
            if let Some(block) = self
                .decoder
                .next_block()
                .map_err(|e| TransferError::Protocol(e.to_string()))?
            {
                self.decoded += block.len() as u64;
                self.block = block;
                self.taken = 0;
                self.count_arrived(on_arrived);
                return Ok(());
            }
            let n = tokio::select! {
                read = timeout(IDLE_TIMEOUT, self.stream.read(&mut self.buf)) => {
                    read.map_err(|_| TransferError::Timeout(SENDER_IDLE))??
                }
                // The sender says nothing while it sends.
                message = control.recv() => return Err(message?.unexpected()),
            };
            if n == 0 {
                // The sender may say why it stopped.
                return Err(match timeout(GOODBYE_TIMEOUT, control.recv()).await {
                    Ok(Ok(message)) => message.unexpected(),
                    _ => TransferError::Disconnected("the data stream ended early".into()),
                });
            }
            self.decoder.push(&self.buf[..n]);
            self.count_arrived(on_arrived);
        }
        Ok(())
    }

    fn count_arrived(&mut self, on_arrived: &mut (dyn FnMut(u64) + Send)) {
        let arrived = self.decoded + self.decoder.partial();
        if arrived > self.arrived {
            on_arrived(arrived - self.arrived);
            self.arrived = arrived;
        }
    }

    /// Writes the next `len` bytes of the stream to `file`, and to `hasher`.
    async fn write_to(
        &mut self,
        file: &mut File,
        mut len: u64,
        control: &mut ReceiverControl,
        on_arrived: &mut (dyn FnMut(u64) + Send),
        mut hasher: Option<&mut Hasher>,
    ) -> Result<(), TransferError> {
        while len > 0 {
            self.fill(control, on_arrived).await?;
            let n = len.min((self.block.len() - self.taken) as u64) as usize;
            let bytes = &self.block[self.taken..self.taken + n];
            file.write_all(bytes).await?;
            if let Some(hasher) = hasher.as_deref_mut() {
                hasher.update(bytes);
            }
            self.taken += n;
            len -= n as u64;
            self.written += n as u64;
            if self.written - self.acked >= ACK_STEP || self.acked_at.elapsed() >= ACK_INTERVAL {
                control
                    .send(&ReceiverMessage::Ack {
                        bytes: self.written,
                    })
                    .await?;
                self.acked = self.written;
                self.acked_at = Instant::now();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "tests/transfer.rs"]
mod tests;
