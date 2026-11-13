use std::fmt::Debug;
use std::io;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use colored::Colorize;
use fsend_proto::peer::frame::{self, FrameDecoder};
use fsend_proto::peer::limits::{
    ACK_INTERVAL, ACK_STEP, GOODBYE_TIMEOUT, HELLO_TIMEOUT, IDLE_TIMEOUT, MAX_DEPTH,
    MIN_RECV_WINDOW, RECV_WINDOW,
};
use fsend_proto::peer::offer::{self, EntryError, OfferError, TopEntry};
use fsend_proto::peer::{
    self, AcceptedFile, Cancel, Entry, ErrorCode, ErrorReport, Hello, Message, ReceiverMessage,
    SenderMessage,
};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

/// Read/write chunk on the data stream.
pub const BUF_SIZE: usize = 64 * 1024;

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

pub struct ReceiveArgs {
    pub resume: bool,
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

/// Opens a file for receiving.
pub async fn open_for_receive(path: &Path, offset: u64) -> io::Result<File> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await?;
    file.seek(io::SeekFrom::Start(offset)).await?;
    Ok(file)
}

/// Finishes a received file.
pub async fn finish_received_file(file: &mut File, size: u64) -> io::Result<()> {
    file.set_len(size).await?;
    file.sync_all().await?;
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

pub async fn send_files(
    transport: &mut dyn Transport,
    outgoing: &Outgoing,
    on_event: EventCb<'_>,
) -> Result<(), TransferError> {
    let result = match transport.control().await {
        Ok(stream) => {
            let mut control = Control::new(stream);
            let result = send(transport, &mut control, outgoing, on_event).await;
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
    let files = match control.recv().await? {
        ReceiverMessage::Accept { files } => files,
        ReceiverMessage::Reject { reason } => return Err(TransferError::PeerDeclined(reason)),
        other => return Err(other.unexpected()),
    };
    let entries = &outgoing.entries;
    offer::check_accept(entries, &files).map_err(|e| TransferError::Protocol(e.to_string()))?;
    let (tops, top_of) = offer::top_entries(entries);
    on_event(Event::Started {
        entries: &tops,
        present: &offer::present(entries, &files),
    });

    if remaining(entries, &files) > 0 {
        let mut data = transport.data().await?;
        let mut buf = vec![0; BUF_SIZE];
        for &AcceptedFile { id, offset } in &files {
            let id = id as usize;
            let mut left = entries[id].size() - offset;
            if left == 0 {
                continue;
            }
            let mut file = File::open(&outgoing.sources[id]).await?;
            file.seek(io::SeekFrom::Start(offset)).await?;
            while left > 0 {
                while window.room() == 0 {
                    window.ack(control.recv_within(IDLE_TIMEOUT, RECEIVER_IDLE).await?)?;
                }
                let n = left.min(window.room()).min(BUF_SIZE as u64);
                let chunk = &mut buf[..n as usize];
                file.read_exact(chunk).await?;
                timeout(IDLE_TIMEOUT, window.write(data.as_mut(), chunk, control))
                    .await
                    .map_err(|_| TransferError::Timeout(RECEIVER_IDLE))??;
                left -= n;
                window.sent += n;
                on_event(Event::Progress {
                    entry: top_of[id],
                    bytes: n,
                });
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

    /// Writes `chunk` to the data stream, taking acks as they arrive.
    async fn write(
        &mut self,
        data: &mut dyn ByteStream,
        mut chunk: &[u8],
        control: &mut SenderControl,
    ) -> Result<(), TransferError> {
        while !chunk.is_empty() {
            tokio::select! {
                written = data.write(chunk) => match written? {
                    0 => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                    n => chunk = &chunk[n..],
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
    args: ReceiveArgs,
    accept: AcceptCb<'_>,
    on_event: EventCb<'_>,
) -> Result<(), TransferError> {
    // Over iroh the stream only shows up once the sender has written to it.
    let result = match timeout(HELLO_TIMEOUT, transport.control()).await {
        Ok(Ok(stream)) => {
            let mut control = Control::new(stream);
            let result = receive(transport, &mut control, args, accept, on_event).await;
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
    args: ReceiveArgs,
    accept: AcceptCb<'_>,
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
    let local = if args.resume {
        local_sizes(&entries, &paths).await
    } else {
        vec![None; entries.len()]
    };
    let files = offer::accept(&entries, &local);
    control
        .send(&ReceiverMessage::Accept {
            files: files.clone(),
        })
        .await?;
    on_event(Event::Started {
        entries: &tops,
        present: &offer::present(&entries, &files),
    });

    for (entry, path) in entries.iter().zip(&paths) {
        if entry.is_dir() {
            tokio::fs::create_dir_all(path).await?;
        }
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
    for &AcceptedFile { id, offset } in &files {
        let id = id as usize;
        let size = entries[id].size();
        let mut file = open_for_receive(&paths[id], offset).await?;
        let received = match &mut incoming {
            Some(incoming) => {
                let mut on_bytes = |bytes| {
                    on_event(Event::Progress {
                        entry: top_of[id],
                        bytes,
                    })
                };
                incoming
                    .write_to(&mut file, size - offset, control, &mut on_bytes)
                    .await
            }
            None => Ok(()),
        };
        // Even a partial file must be on disk before the next session resumes it.
        file.flush().await?;
        received?;
        finish_received_file(&mut file, size).await?;
    }

    control.send(&ReceiverMessage::Done).await?;
    goodbye(transport, control).await;
    Ok(())
}

/// The size of the file already at each entry's path, if there is one.
async fn local_sizes(entries: &[Entry], paths: &[PathBuf]) -> Vec<Option<u64>> {
    let mut sizes = Vec::with_capacity(entries.len());
    for (entry, path) in entries.iter().zip(paths) {
        let metadata = match entry {
            Entry::File { .. } => tokio::fs::metadata(path).await.ok(),
            Entry::Dir { .. } => None,
        };
        sizes.push(metadata.filter(|m| m.is_file()).map(|m| m.len()));
    }
    sizes
}

/// The receiving end of the data stream, acking what it wrote.
struct Incoming {
    stream: Box<dyn ByteStream>,
    buf: Vec<u8>,
    written: u64,
    acked: u64,
    acked_at: Instant,
}

impl Incoming {
    fn new(stream: Box<dyn ByteStream>) -> Self {
        Self {
            stream,
            buf: vec![0; BUF_SIZE],
            written: 0,
            acked: 0,
            acked_at: Instant::now(),
        }
    }

    /// Writes the next `len` bytes of the stream to `file`.
    async fn write_to(
        &mut self,
        file: &mut File,
        mut len: u64,
        control: &mut ReceiverControl,
        on_bytes: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), TransferError> {
        while len > 0 {
            let want = len.min(BUF_SIZE as u64) as usize;
            let n = tokio::select! {
                read = timeout(IDLE_TIMEOUT, self.stream.read(&mut self.buf[..want])) => {
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
            file.write_all(&self.buf[..n]).await?;
            len -= n as u64;
            self.written += n as u64;
            on_bytes(n as u64);
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
