use std::fmt::Debug;
use std::io;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use colored::Colorize;
use fsend_proto::peer::block::{self, BlockDecoder, Progress};
use fsend_proto::peer::frame::{self, FrameDecoder};
use fsend_proto::peer::limits::{
    ACK_INTERVAL, ACK_STEP, GOODBYE_TIMEOUT, HELLO_TIMEOUT, IDLE_TIMEOUT, MAX_BLOCK_BYTES,
    MAX_DEPTH, MIN_RECV_WINDOW, RECV_WINDOW,
};
use fsend_proto::peer::offer::{self, EntryError, OfferError, TopEntry};
use fsend_proto::peer::resume::{self, Conflict, ConflictPolicy, Fingerprint, Local};
use fsend_proto::peer::{
    self, AcceptedFile, Cancel, CheckedFile, Entry, ErrorCode, ErrorReport, Hello, Message,
    ReceiverMessage, SenderMessage,
};
use futures_util::StreamExt;
use futures_util::stream::FuturesOrdered;
use thiserror::Error;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

/// Read chunk on the streams.
pub const BUF_SIZE: usize = 64 * 1024;
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
    #[error("connection: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("connection to the peer lost: {0}")]
    Disconnected(String),
    #[error("{} of the given paths do not exist", .0.len())]
    PathsNotFound(Vec<PathBuf>),
    #[error("file does not exist: {0}")]
    FileNotFound(PathBuf),
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
    #[error("the peer declined the files")]
    PeerDeclined(Option<String>),
    #[error("the peer cancelled the transfer")]
    PeerCancelled(Option<String>),
    #[error("the peer failed ({code:?}): {message}")]
    Peer { code: ErrorCode, message: String },
    #[error("the peer sent an invalid offer: {0}")]
    InvalidOffer(OfferError),
    /// Traffic from the peer that the protocol does not allow.
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timed out: {0}")]
    Timeout(&'static str),
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
    #[error("the peer sent connection info for another transport")]
    UnexpectedConnectionInfo,
    #[error("webrtc: {0}")]
    WebRtc(#[from] webrtc::error::Error),
    #[error("internal: {0}")]
    Internal(&'static str),
}

impl TransferError {
    /// Generate a user-facing error messages for user-facing "errors"
    pub fn user_error(&self) -> Option<String> {
        let error = "error:".red();
        let update = "Update fsend on both sides.";
        Some(match self {
            Self::Declined => "Transfer declined.".to_owned(),
            Self::PeerDeclined(_) => format!("{error} the peer declined the files"),
            Self::PeerCancelled(_) => format!("{error} the peer cancelled the transfer"),
            Self::Disconnected(reason) => format!(
                "{error} the connection to the peer was lost ({reason}).\n\
                 Start a new session to resume the transfer where it stopped."
            ),
            Self::Version { .. } => format!("{error} {self}.\n{update}"),
            Self::Peer {
                code: ErrorCode::Version,
                message,
            } => format!("{error} the peer speaks another protocol version ({message}).\n{update}"),
            Self::Peer { message, .. } => format!("{error} the peer failed: {message}"),
            Self::Unsendable(e) => format!("{error} cannot send these files: {e}"),
            Self::Timeout(what) => format!("{error} timed out: {what}"),
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
                format!("{error} {} {noun} not exist:{list}", paths.len())
            }
            Self::PeerTimeout => format!(
                "{error} the peer did not connect in time. Check that both sides are online and try again."
            ),
            Self::Io(_)
            | Self::Connection(_)
            | Self::FileNotFound(_)
            | Self::InvalidOffer(_)
            | Self::Protocol(_)
            | Self::Bind(_)
            | Self::Connect(_)
            | Self::Accept(_)
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
            Self::InvalidOffer(e) => (ErrorCode::InvalidOffer, e.to_string()),
            Self::Protocol(message) => (ErrorCode::Protocol, message.clone()),
            Self::Timeout(what) => (ErrorCode::Timeout, what.to_string()),
            Self::Internal(what) => (ErrorCode::Internal, what.to_string()),
            _ => return None,
        };
        Some(ErrorReport { code, message })
    }
}

/// A stream that reports the peer as gone is a lost connection, not an I/O
/// fault; the transports mark those with `ConnectionAborted`/`NotConnected`.
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
    /// Sender: the offer is out, the receiver is deciding.
    Offered,
    /// The offer was accepted; `present` counts the bytes of each entry already there.
    Started {
        entries: &'a [TopEntry],
        present: &'a [u64],
    },
    /// `bytes` more of the entry at `entry` moved.
    Progress { entry: usize, bytes: u64 },
}

pub type EventCb<'a> = &'a mut (dyn FnMut(Event<'_>) + Send);
/// The directory to receive into, or `None` to decline.
pub type AcceptCb<'a> = &'a mut (dyn FnMut(&[TopEntry]) -> Option<PathBuf> + Send);
/// What to do with the entries of the offer that conflict with local ones.
pub type ConflictCb<'a> = &'a mut (dyn FnMut(&[Entry], &[Conflict]) -> ConflictPolicy + Send);

/// A bidirectional bytestream.
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized> ByteStream for T {}

#[async_trait]
pub trait Transport: Send {
    fn connection_type_name(&self) -> String;
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
    pub entries: Vec<Entry>,
    sources: Vec<PathBuf>,
    frame: Vec<u8>,
}

/// Walks `paths` into a valid offer.
pub fn build_offer(paths: &[PathBuf]) -> Result<Outgoing, TransferError> {
    let mut outgoing = Outgoing::default();
    for path in paths {
        if !path.exists() {
            return Err(TransferError::FileNotFound(path.clone()));
        }
        outgoing.add(path, vec![entry_name(path)?])?;
    }
    outgoing.frame = offer::encode(&outgoing.entries).map_err(TransferError::Unsendable)?;
    Ok(outgoing)
}

impl Outgoing {
    /// Adds `source` under `path`, parents before children.
    fn add(&mut self, source: &Path, path: Vec<String>) -> Result<(), TransferError> {
        // Symlinked directories can loop.
        if path.len() > MAX_DEPTH {
            return Err(TransferError::Unsendable(OfferError::Entry {
                id: self.entries.len(),
                reason: EntryError::TooDeep,
            }));
        }
        let metadata = std::fs::metadata(source)?;
        self.sources.push(source.to_path_buf());
        if metadata.is_file() {
            self.entries.push(Entry::File {
                path,
                size: metadata.len(),
            });
            return Ok(());
        }
        self.entries.push(Entry::Dir { path: path.clone() });
        let mut children = std::fs::read_dir(source)?.collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let mut child_path = path.clone();
            child_path.push(child.file_name().to_string_lossy().into_owned());
            self.add(&child.path(), child_path)?;
        }
        Ok(())
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
pub fn local_path(root: &Path, path: &[String]) -> io::Result<PathBuf> {
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
pub async fn open_for_receive(path: &Path, offset: u64) -> io::Result<File> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(offset == 0)
        .open(path)
        .await?;
    file.seek(io::SeekFrom::Start(offset)).await?;
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
pub async fn finish_received_file(file: &mut File, size: u64) -> io::Result<()> {
    file.set_len(size).await?;
    file.shutdown().await
}

/// A message the peer sends.
pub trait PeerMessage: Message + Debug {
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
        matches!(
            self,
            Self::Done | Self::Reject { .. } | Self::Cancel(_) | Self::Error(_)
        )
    }
}

impl From<ErrorReport> for TransferError {
    fn from(ErrorReport { code, message }: ErrorReport) -> Self {
        Self::Peer { code, message }
    }
}

/// The control stream, one message per frame: we send `Out`, the peer sends `In`.
pub struct Control<Out, In> {
    stream: Box<dyn ByteStream>,
    decoder: FrameDecoder,
    buf: Vec<u8>,
    messages: PhantomData<fn(Out) -> In>,
}

pub type SenderControl = Control<SenderMessage, ReceiverMessage>;
pub type ReceiverControl = Control<ReceiverMessage, SenderMessage>;

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

/// Tells the peer why `result` failed, if it should know, as `error(report)`.
async fn explain<Out: Message + Debug, In: PeerMessage>(
    transport: &mut dyn Transport,
    control: &mut Control<Out, In>,
    result: Result<(), TransferError>,
    error: fn(ErrorReport) -> Out,
) -> Result<(), TransferError> {
    if let Err(failure) = &result
        && let Some(report) = failure.report()
        && control.send(&error(report)).await.is_ok()
    {
        goodbye(transport, control).await;
    }
    result
}

/// Bytes of the accepted files still to move.
fn remaining(entries: &[Entry], files: &[AcceptedFile]) -> u64 {
    files
        .iter()
        .map(|file| entries[file.id as usize].size() - file.offset)
        .sum()
}

/// Sends `outgoing`, zstd-compressing the data where that helps if `compress`.
pub async fn send_files(
    transport: &mut dyn Transport,
    outgoing: &Outgoing,
    compress: bool,
    on_event: EventCb<'_>,
) -> Result<(), TransferError> {
    let result = match transport.control().await {
        Ok(stream) => {
            let mut control = Control::new(stream);
            let result = send(transport, &mut control, outgoing, compress, on_event).await;
            explain(transport, &mut control, result, SenderMessage::Error).await
        }
        Err(e) => Err(e),
    };
    transport.close(result.is_ok()).await;
    result
}

async fn send(
    transport: &mut dyn Transport,
    control: &mut SenderControl,
    outgoing: &Outgoing,
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

    control.send_frame(&outgoing.frame).await?;
    on_event(Event::Offered);
    let entries = &outgoing.entries;
    let mut answer = control.recv().await?;
    if let ReceiverMessage::Check { files } = answer {
        offer::check_lengths(entries, &files)
            .map_err(|e| TransferError::Protocol(e.to_string()))?;
        let sources = files
            .iter()
            .map(|file| (outgoing.sources[file.id as usize].clone(), file.len))
            .collect();
        let hashes = fingerprints(sources).await?;
        control.send(&SenderMessage::Hashes { hashes }).await?;
        answer = control.recv().await?;
    }
    let files = match answer {
        ReceiverMessage::Accept { files } => files,
        ReceiverMessage::Reject { reason } => return Err(TransferError::PeerDeclined(reason)),
        other => return Err(other.unexpected()),
    };
    offer::check_accept(entries, &files).map_err(|e| TransferError::Protocol(e.to_string()))?;
    let (tops, top_of) = offer::top_entries(entries);
    on_event(Event::Started {
        entries: &tops,
        present: &offer::present(entries, &files),
    });

    let mut reader = BlockReader::new(outgoing, &files);
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
            timeout(IDLE_TIMEOUT, write)
                .await
                .map_err(|_| TransferError::Timeout(RECEIVER_IDLE))??;
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
    outgoing: &'a Outgoing,
    files: std::slice::Iter<'a, AcceptedFile>,
    /// The file being read and the bytes left in it.
    current: Option<(File, u64)>,
    left: u64,
}

impl<'a> BlockReader<'a> {
    fn new(outgoing: &'a Outgoing, files: &'a [AcceptedFile]) -> Self {
        Self {
            outgoing,
            files: files.iter(),
            current: None,
            left: remaining(&outgoing.entries, files),
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
                    let mut file = File::open(&self.outgoing.sources[id]).await?;
                    file.seek(io::SeekFrom::Start(next.offset)).await?;
                    let left = self.outgoing.entries[id].size() - next.offset;
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

pub async fn receive_files(
    transport: &mut dyn Transport,
    accept: AcceptCb<'_>,
    on_conflict: ConflictCb<'_>,
    on_event: EventCb<'_>,
) -> Result<(), TransferError> {
    // Over iroh the stream only shows up once the sender has written to it.
    let result = match timeout(HELLO_TIMEOUT, transport.control()).await {
        Ok(Ok(stream)) => {
            let mut control = Control::new(stream);
            let result = receive(transport, &mut control, accept, on_conflict, on_event).await;
            explain(transport, &mut control, result, ReceiverMessage::Error).await
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(TransferError::Timeout(SENDER_HELLO)),
    };
    transport.close(result.is_ok()).await;
    result
}

async fn receive(
    transport: &mut dyn Transport,
    control: &mut ReceiverControl,
    accept: AcceptCb<'_>,
    on_conflict: ConflictCb<'_>,
    on_event: EventCb<'_>,
) -> Result<(), TransferError> {
    control
        .send(&ReceiverMessage::Hello(peer::hello(Some(RECV_WINDOW))))
        .await?;
    match control.recv_within(HELLO_TIMEOUT, SENDER_HELLO).await? {
        SenderMessage::Hello(hello) => check_version(&hello)?,
        other => return Err(other.unexpected()),
    }
    let entries = match control
        .recv_within(HELLO_TIMEOUT, "the sender did not make an offer in time")
        .await?
    {
        SenderMessage::Offer { entries } => entries,
        other => return Err(other.unexpected()),
    };
    offer::validate(&entries).map_err(TransferError::InvalidOffer)?;
    let (tops, top_of) = offer::top_entries(&entries);

    let Some(out) = accept(&tops) else {
        control
            .send(&ReceiverMessage::Reject { reason: None })
            .await?;
        goodbye(transport, control).await;
        return Err(TransferError::Declined);
    };
    tokio::fs::create_dir_all(&out).await?;
    let paths = entries
        .iter()
        .map(|entry| local_path(&out, entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    let local = local_entries(&paths).await;
    let checked = resume::to_check(&entries, &local);
    let verified = if checked.is_empty() {
        Vec::new()
    } else {
        verify(control, &paths, checked).await?
    };
    let plan = resume::plan(&entries, &local, &verified);
    let policy = match plan.conflicts.is_empty() {
        true => ConflictPolicy::Skip,
        false => on_conflict(&entries, &plan.conflicts),
    };
    let files = plan.accept(policy);
    control
        .send(&ReceiverMessage::Accept {
            files: files.clone(),
        })
        .await?;
    on_event(Event::Started {
        entries: &tops,
        present: &offer::present(&entries, &files),
    });

    for &id in &plan.dirs {
        tokio::fs::create_dir_all(&paths[id as usize]).await?;
    }
    let mut incoming = if remaining(&entries, &files) > 0 {
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
    let mut progress = Progress::new(&entries, &files, &top_of);
    for &AcceptedFile { id, offset } in &files {
        let id = id as usize;
        let size = entries[id].size();
        let mut file = open_for_receive(&paths[id], offset).await?;
        let received = match &mut incoming {
            Some(incoming) => {
                let mut on_arrived = |bytes| {
                    for (entry, bytes) in progress.advance(bytes) {
                        on_event(Event::Progress { entry, bytes });
                    }
                };
                incoming
                    .write_to(&mut file, size - offset, control, &mut on_arrived)
                    .await
            }
            None => Ok(()),
        };
        // Even a partial file must be on disk before the next session resumes it.
        file.flush().await?;
        received?;
        finish_received_file(&mut file, size).await?;
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

/// What is already at each path.
async fn local_entries(paths: &[PathBuf]) -> Vec<Local> {
    let mut local = Vec::with_capacity(paths.len());
    for path in paths {
        local.push(match tokio::fs::metadata(path).await {
            Err(_) => Local::Missing,
            Ok(m) if m.is_file() => Local::File { size: m.len() },
            Ok(m) if m.is_dir() => Local::Dir,
            Ok(_) => Local::Other,
        });
    }
    local
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

    /// Writes the next `len` bytes of the stream to `file`.
    async fn write_to(
        &mut self,
        file: &mut File,
        mut len: u64,
        control: &mut ReceiverControl,
        on_arrived: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), TransferError> {
        while len > 0 {
            self.fill(control, on_arrived).await?;
            let n = len.min((self.block.len() - self.taken) as u64) as usize;
            file.write_all(&self.block[self.taken..self.taken + n])
                .await?;
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
