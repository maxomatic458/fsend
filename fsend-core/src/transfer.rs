use std::fmt::Debug;
use std::io;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fsend_proto::code::Code;
use fsend_proto::peer::block::{self, BlockDecoder};
use fsend_proto::peer::checksum::{Algorithm, Hasher};
use fsend_proto::peer::frame::{self, FrameDecoder};
use fsend_proto::peer::limits::{
    ACK_INTERVAL, ACK_STEP, GOODBYE_TIMEOUT, IDLE_TIMEOUT, MAX_BLOCK_BYTES, MAX_DEPTH, MIN_WINDOW,
    WINDOW,
};
use fsend_proto::peer::offer::{self, EntryError, OfferError};
use fsend_proto::peer::progress::Progress;
use fsend_proto::peer::resume::{self, ConflictPolicy, Fingerprint, Local};
use fsend_proto::peer::{
    AcceptedFile, Cancel, CheckedFile, Entry, ErrorCode, ErrorReport, Message, Node, Offer,
    ReceiverMessage, SenderMessage,
};
use futures_util::StreamExt;
use futures_util::stream::FuturesOrdered;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::{Error, ReceiveHandler, checksum};

const BUF_SIZE: usize = 64 * 1024;
/// Data blocks compressed at once.
const COMPRESS_WORKERS: usize = 4;

const SENDER_IDLE: &str = "the sender stopped sending";
const RECEIVER_IDLE: &str = "the receiver stopped responding";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// Sender: the code for the receiver.
    Code(&'a Code),
    /// Sender: the receiver joined and has the offer.
    Paired,
    Connected(ConnectionKind),
    /// Receiver: checking the bytes already here against the checksums.
    Verifying {
        done: u64,
        total: u64,
    },
    /// The files to move, each from its offset; the rest are there or skipped.
    Started(&'a [AcceptedFile]),
    /// `bytes` more of file `id` moved.
    Progress {
        id: u32,
        bytes: u64,
    },
}

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

pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized> ByteStream for T {}

#[async_trait]
pub trait Transport: Send {
    fn connection_kind(&self) -> ConnectionKind;
    /// The receiver writes to it first.
    async fn control(&mut self) -> Result<Box<dyn ByteStream>, Error>;
    /// The sender writes to it first.
    async fn data(&mut self) -> Result<Box<dyn ByteStream>, Error>;
    /// Resolves once the peer has hung up; `Ok` if it was done.
    async fn closed(&mut self) -> Result<(), Error>;
    async fn close(&mut self, ok: bool);
}

/// An offer and where its files come from.
#[derive(Debug, Default)]
pub struct Outgoing {
    pub(crate) offer: Offer,
    /// By entry id.
    pub(crate) sources: Vec<PathBuf>,
    /// For pairing.
    pub(crate) encoded: Vec<u8>,
}

/// Walks `paths` into a valid offer, sorted by name at every level.
pub fn build_offer(paths: &[PathBuf]) -> Result<Outgoing, Error> {
    let missing: Vec<_> = paths.iter().filter(|p| !p.exists()).cloned().collect();
    if !missing.is_empty() {
        return Err(Error::PathsNotFound(missing));
    }
    let mut named = (paths.iter())
        .map(|path| Ok((entry_name(path)?, path.clone())))
        .collect::<io::Result<Vec<_>>>()?;
    named.sort_by(|a, b| a.0.cmp(&b.0));
    let mut outgoing = Outgoing::default();
    for (name, path) in named {
        let node = outgoing.add(&path, name, 1)?;
        outgoing.offer.entries.push(node);
    }
    outgoing.encode()?;
    Ok(outgoing)
}

impl Outgoing {
    pub fn offer(&self) -> &Offer {
        &self.offer
    }

    pub fn size(&self) -> u64 {
        self.offer.entries.iter().map(Node::size).sum()
    }

    pub(crate) fn encode(&mut self) -> Result<(), Error> {
        self.encoded = offer::encode(&self.offer).map_err(Error::Unsendable)?;
        Ok(())
    }

    fn add(&mut self, source: &Path, name: String, depth: usize) -> Result<Node, Error> {
        // Symlinked directories can loop.
        if depth > MAX_DEPTH {
            return Err(Error::Unsendable(OfferError::Entry {
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
        let mut children = std::fs::read_dir(source)?
            .map(|child| {
                let child = child?;
                Ok((
                    child.file_name().to_string_lossy().into_owned(),
                    child.path(),
                ))
            })
            .collect::<io::Result<Vec<_>>>()?;
        children.sort_by(|a, b| a.0.cmp(&b.0));
        let entries = (children.into_iter())
            .map(|(name, path)| self.add(&path, name, depth + 1))
            .collect::<Result<_, _>>()?;
        Ok(Node::Dir { name, entries })
    }
}

/// `.` goes by its real name.
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

/// Each name must be a plain file name here.
fn local_path(root: &Path, path: &[String]) -> io::Result<PathBuf> {
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

async fn open_for_receive(path: &Path, offset: u64) -> io::Result<File> {
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

async fn finish_received_file(file: &mut File, size: u64) -> io::Result<()> {
    file.set_len(size).await?;
    file.shutdown().await
}

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

fn protocol(error: impl ToString) -> Error {
    Error::Protocol(error.to_string())
}

/// What the peer is told about `error`, if anything.
fn report(error: &Error) -> Option<ErrorReport> {
    let (code, message) = match error {
        Error::Io(e) => (ErrorCode::Io, e.to_string()),
        Error::Protocol(message) => (ErrorCode::Protocol, message.clone()),
        Error::Timeout(what) => (ErrorCode::Timeout, what.to_string()),
        Error::Internal(what) => (ErrorCode::Internal, what.to_string()),
        _ => return None,
    };
    Some(ErrorReport { code, message })
}

trait PeerMessage: Message + Debug {
    /// The error when the flow expects another message.
    fn unexpected(self) -> Error;
    /// Whether it is the peer's last word.
    fn is_final(&self) -> bool;
}

impl PeerMessage for SenderMessage {
    fn unexpected(self) -> Error {
        match self {
            Self::Error(report) => Error::Peer(report),
            Self::Cancel(Cancel { reason }) => Error::PeerCancelled(reason),
            other => protocol(format!("unexpected {} message", other.kind())),
        }
    }

    fn is_final(&self) -> bool {
        matches!(self, Self::Cancel(_) | Self::Error(_))
    }
}

impl PeerMessage for ReceiverMessage {
    fn unexpected(self) -> Error {
        match self {
            Self::Error(report) => Error::Peer(report),
            Self::Cancel(Cancel { reason }) => Error::PeerCancelled(reason),
            other => protocol(format!("unexpected {} message", other.kind())),
        }
    }

    fn is_final(&self) -> bool {
        matches!(self, Self::Done | Self::Cancel(_) | Self::Error(_))
    }
}

/// One message per frame: we send `Out`, the peer `In`.
struct Control<Out, In> {
    stream: Box<dyn ByteStream>,
    decoder: FrameDecoder,
    buf: Vec<u8>,
    messages: PhantomData<fn(Out) -> In>,
}

type SenderControl = Control<SenderMessage, ReceiverMessage>;
type ReceiverControl = Control<ReceiverMessage, SenderMessage>;

impl<Out: Message + Debug, In: PeerMessage> Control<Out, In> {
    fn new(stream: Box<dyn ByteStream>) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::default(),
            buf: vec![0; BUF_SIZE],
            messages: PhantomData,
        }
    }

    async fn send(&mut self, message: &Out) -> Result<(), Error> {
        tracing::debug!("sending {message:?}");
        let frame = frame::encode(message)
            .map_err(|_| Error::Internal("a message does not fit a frame"))?;
        self.send_frame(&frame).await
    }

    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), Error> {
        self.stream.write_all(frame).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Cancel safe.
    async fn recv(&mut self) -> Result<In, Error> {
        loop {
            match self.decoder.next_message() {
                Ok(Some(message)) => {
                    tracing::debug!("received {message:?}");
                    return Ok(message);
                }
                Ok(None) => {
                    let n = self.stream.read(&mut self.buf).await?;
                    if n == 0 {
                        return Err(Error::Disconnected("the control stream ended".into()));
                    }
                    self.decoder.push(&self.buf[..n]);
                }
                Err(e) => return Err(protocol(e)),
            }
        }
    }

    async fn recv_within(&mut self, limit: Duration, what: &'static str) -> Result<In, Error> {
        timeout(limit, self.recv())
            .await
            .map_err(|_| Error::Timeout(what))?
    }
}

/// Gives the peer a moment to read our last message.
async fn goodbye<Out: Message + Debug, In: PeerMessage>(
    transport: &mut dyn Transport,
    control: &mut Control<Out, In>,
) {
    let last_word = async {
        loop {
            match control.recv().await {
                Ok(message) if message.is_final() => return,
                Ok(_) => {}
                // Nothing more will arrive; wait for the hang-up.
                Err(Error::Protocol(_)) => std::future::pending().await,
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

/// Tells the peer why `result` failed, if it should know.
async fn explain<Out: Message + Debug, In: PeerMessage>(
    transport: &mut dyn Transport,
    control: &mut Control<Out, In>,
    result: Result<(), Error>,
    error: fn(ErrorReport) -> Out,
    cancel: Out,
) -> Result<(), Error> {
    let last_word = match &result {
        Ok(()) => None,
        Err(Error::Cancelled) => Some(cancel),
        Err(failure) => report(failure).map(error),
    };
    if let Some(message) = last_word
        && control.send(&message).await.is_ok()
    {
        goodbye(transport, control).await;
    }
    result
}

pub(crate) async fn cancellable<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    cancel
        .run_until_cancelled(work)
        .await
        .unwrap_or(Err(Error::Cancelled))
}

fn remaining(entries: &[Entry], files: &[AcceptedFile]) -> u64 {
    files
        .iter()
        .map(|file| entries[file.id as usize].size() - file.offset)
        .sum()
}

pub async fn send_files(
    transport: &mut dyn Transport,
    outgoing: &Outgoing,
    compress: bool,
    on_event: &mut (dyn FnMut(Event<'_>) + Send),
    cancel: CancellationToken,
) -> Result<(), Error> {
    let result = match cancellable(&cancel, transport.control()).await {
        Ok(stream) => {
            let mut control = Control::new(stream);
            let entries = outgoing.offer.flatten();
            let send = send(
                transport,
                &mut control,
                &entries,
                &outgoing.sources,
                compress,
                on_event,
            );
            let result = cancellable(&cancel, send).await;
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
    on_event: &mut (dyn FnMut(Event<'_>) + Send),
) -> Result<(), Error> {
    // No timeout: the receiver's user may be deciding about conflicts.
    let mut answer = control.recv().await?;
    if let ReceiverMessage::Check { files } = answer {
        offer::check_lengths(entries, &files).map_err(protocol)?;
        let sources = (files.iter())
            .map(|file| (sources[file.id as usize].clone(), file.len))
            .collect();
        let hashes = fingerprints(sources).await?;
        control.send(&SenderMessage::Hashes { hashes }).await?;
        answer = control.recv().await?;
    }
    let (files, size) = match answer {
        ReceiverMessage::Accept { files, window } => (files, window),
        other => return Err(other.unexpected()),
    };
    if size < MIN_WINDOW {
        return Err(protocol("the receiver's window is too small"));
    }
    offer::check_accept(entries, &files).map_err(protocol)?;
    on_event(Event::Started(&files));

    let mut window = Window {
        size,
        sent: 0,
        acked: 0,
    };
    let mut reader = BlockReader::new(entries, sources, &files);
    let mut progress = Progress::new(entries, &files);
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
            let (frame, raw) = encoded.map_err(|_| Error::Internal("encoding a block failed"))?;
            // The block's file bytes count in proportion to its bytes written.
            let mut counted = 0;
            let mut on_written = |written: usize| {
                let now = raw * written as u64 / frame.len() as u64;
                for (id, bytes) in progress.advance(now - counted) {
                    on_event(Event::Progress { id, bytes });
                }
                counted = now;
            };
            let write = window.write(data.as_mut(), &frame, control, &mut on_written);
            match timeout(IDLE_TIMEOUT, write).await {
                Ok(Ok(())) => {}
                // The receiver may say why it stopped the data stream.
                Ok(Err(lost @ Error::Disconnected(_))) => {
                    return Err(window.last_word(control, lost).await);
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(Error::Timeout(RECEIVER_IDLE)),
            }
        }
        timeout(IDLE_TIMEOUT, data.shutdown())
            .await
            .map_err(|_| Error::Timeout(RECEIVER_IDLE))??;
    }

    loop {
        match control.recv_within(IDLE_TIMEOUT, RECEIVER_IDLE).await? {
            ReceiverMessage::Done => return Ok(()),
            message => window.ack(message)?,
        }
    }
}

struct BlockReader<'a> {
    entries: &'a [Entry],
    sources: &'a [PathBuf],
    files: std::slice::Iter<'a, AcceptedFile>,
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

struct Window {
    size: u64,
    sent: u64,
    acked: u64,
}

impl Window {
    fn room(&self) -> u64 {
        self.size - (self.sent - self.acked)
    }

    /// Takes acks while writing `frame`.
    async fn write(
        &mut self,
        data: &mut dyn ByteStream,
        frame: &[u8],
        control: &mut SenderControl,
        on_written: &mut (dyn FnMut(usize) + Send),
    ) -> Result<(), Error> {
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

    /// The receiver's reason, or `lost` if it gives none in time.
    async fn last_word(&mut self, control: &mut SenderControl, lost: Error) -> Error {
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

    fn ack(&mut self, message: ReceiverMessage) -> Result<(), Error> {
        match message {
            ReceiverMessage::Ack { bytes } if (self.acked..=self.sent).contains(&bytes) => {
                self.acked = bytes;
                Ok(())
            }
            ReceiverMessage::Ack { .. } => Err(protocol("an ack outside the bytes sent")),
            other => Err(other.unexpected()),
        }
    }
}

pub async fn receive_files(
    transport: &mut dyn Transport,
    offer: &Offer,
    out: &Path,
    handler: &mut dyn ReceiveHandler,
    cancel: CancellationToken,
) -> Result<(), Error> {
    let result = match cancellable(&cancel, transport.control()).await {
        Ok(stream) => {
            let mut control = Control::new(stream);
            let mut open = None;
            let receive = receive(transport, &mut control, offer, out, handler, &mut open);
            let result = cancellable(&cancel, receive).await;
            // A cancelled write may still be in flight.
            if let Some(mut file) = open {
                let _ = file.flush().await;
            }
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
    handler: &mut dyn ReceiveHandler,
    open: &mut Option<File>,
) -> Result<(), Error> {
    let entries = &offer.flatten();
    tokio::fs::create_dir_all(out).await?;
    let paths = (entries.iter())
        .map(|entry| local_path(out, entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    let (paths, local) = local_entries(paths).await?;
    let checked = resume::to_check(entries, &local);
    let verified = if checked.is_empty() {
        Vec::new()
    } else {
        verify(control, &paths, checked).await?
    };
    let plan = resume::plan(entries, &local, &verified);
    let policy = match plan.conflicts.is_empty() {
        true => ConflictPolicy::Skip,
        false => handler.resolve(entries, &plan.conflicts).await,
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
            let hashers = present.check(algorithm, handler).await?;
            Some(hashers.into_iter())
        }
        None => None,
    };
    let accept = ReceiverMessage::Accept {
        files: files.clone(),
        window: WINDOW,
    };
    control.send(&accept).await?;
    handler.event(Event::Started(&files));

    for &id in &plan.dirs {
        tokio::fs::create_dir_all(&paths[id as usize]).await?;
    }
    let mut incoming = if remaining(entries, &files) > 0 {
        let stream = tokio::select! {
            stream = timeout(IDLE_TIMEOUT, transport.data()) => {
                stream.map_err(|_| Error::Timeout(SENDER_IDLE))??
            }
            message = control.recv() => return Err(message?.unexpected()),
        };
        Some(Incoming::new(stream))
    } else {
        None
    };
    let mut progress = Progress::new(entries, &files);
    for &AcceptedFile { id, offset } in &files {
        let id = id as usize;
        let size = entries[id].size();
        let file = open.insert(open_for_receive(&paths[id], offset).await?);
        let mut hasher = hashers.as_mut().and_then(Iterator::next);
        let received = match &mut incoming {
            Some(incoming) => {
                let mut on_arrived = |bytes| {
                    for (id, bytes) in progress.advance(bytes) {
                        handler.event(Event::Progress { id, bytes });
                    }
                };
                let len = size - offset;
                incoming
                    .write_to(file, len, control, &mut on_arrived, hasher.as_mut())
                    .await
            }
            None => Ok(()),
        };
        // Keeps even a partial file, for a later session to resume.
        let flushed = file.flush().await;
        received?;
        flushed?;
        finish_received_file(file, size).await?;
        *open = None;
        if let Some(hasher) = hasher
            && Some(&hasher.finish()) != entries[id].checksum()
        {
            return Err(Error::ChecksumMismatch(paths[id].clone()));
        }
    }
    if incoming.is_some_and(|incoming| !incoming.is_empty()) {
        return Err(protocol("the sender sent more bytes than accepted"));
    }

    control.send(&ReceiverMessage::Done).await?;
    goodbye(transport, control).await;
    Ok(())
}

struct Present<'a> {
    entries: &'a [Entry],
    paths: &'a [PathBuf],
    complete: &'a [u32],
    files: &'a [AcceptedFile],
}

impl Present<'_> {
    /// Checks complete files; returns a hasher per accepted file, fed what is here.
    async fn check(
        &self,
        algorithm: Algorithm,
        handler: &mut dyn ReceiveHandler,
    ) -> Result<Vec<Hasher>, Error> {
        let path = |id: u32| self.paths[id as usize].clone();
        let jobs: Vec<_> = (self.complete.iter())
            .map(|&id| (path(id), self.entries[id as usize].size()))
            .chain(self.files.iter().map(|file| (path(file.id), file.offset)))
            .collect();
        let total = jobs.iter().map(|(_, len)| len).sum();
        let mut on_progress = |done| {
            if total > 0 {
                handler.event(Event::Verifying { done, total });
            }
        };
        let mut hashers = checksum::hash_files(algorithm, jobs, &mut on_progress).await?;
        let resumed = hashers.split_off(self.complete.len());
        for (&id, hasher) in self.complete.iter().zip(hashers) {
            if Some(&hasher.finish()) != self.entries[id as usize].checksum() {
                return Err(Error::ChecksumMismatch(path(id)));
            }
        }
        Ok(resumed)
    }
}

/// What is at each of `paths`, which are handed back.
async fn local_entries(paths: Vec<PathBuf>) -> io::Result<(Vec<PathBuf>, Vec<Local>)> {
    tokio::task::spawn_blocking(move || {
        let local = (paths.iter())
            .map(|path| match std::fs::metadata(path) {
                Err(_) => Local::Missing,
                Ok(m) if m.is_file() => Local::File { size: m.len() },
                Ok(m) if m.is_dir() => Local::Dir,
                Ok(_) => Local::Other,
            })
            .collect();
        (paths, local)
    })
    .await
    .map_err(io::Error::other)
}

/// The ids of the `checked` files whose fingerprints match the sender's.
async fn verify(
    control: &mut ReceiverControl,
    paths: &[PathBuf],
    checked: Vec<CheckedFile>,
) -> Result<Vec<u32>, Error> {
    let check = ReceiverMessage::Check {
        files: checked.clone(),
    };
    control.send(&check).await?;
    let hashes = match control.recv().await? {
        SenderMessage::Hashes { hashes } if hashes.len() == checked.len() => hashes,
        SenderMessage::Hashes { .. } => return Err(protocol("the hashes do not match the check")),
        other => return Err(other.unexpected()),
    };
    let ours = (checked.iter())
        .map(|file| (paths[file.id as usize].clone(), file.len))
        .collect();
    let ours = fingerprints(ours).await?;
    Ok(checked
        .iter()
        .zip(ours.iter().zip(&hashes))
        .filter(|(_, (ours, theirs))| ours == theirs)
        .map(|(file, _)| file.id)
        .collect())
}

struct Incoming {
    stream: Box<dyn ByteStream>,
    buf: Vec<u8>,
    decoder: BlockDecoder,
    block: Vec<u8>,
    taken: usize,
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

    /// `on_arrived` gets file bytes as their blocks arrive.
    async fn fill(
        &mut self,
        control: &mut ReceiverControl,
        on_arrived: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), Error> {
        while self.taken == self.block.len() {
            if let Some(block) = self.decoder.next_block().map_err(protocol)? {
                self.decoded += block.len() as u64;
                self.block = block;
                self.taken = 0;
                self.count_arrived(on_arrived);
                return Ok(());
            }
            let n = tokio::select! {
                read = timeout(IDLE_TIMEOUT, self.stream.read(&mut self.buf)) => {
                    read.map_err(|_| Error::Timeout(SENDER_IDLE))??
                }
                // The sender says nothing while it sends.
                message = control.recv() => return Err(message?.unexpected()),
            };
            if n == 0 {
                // The sender may say why it stopped.
                return Err(match timeout(GOODBYE_TIMEOUT, control.recv()).await {
                    Ok(Ok(message)) => message.unexpected(),
                    _ => Error::Disconnected("the data stream ended early".into()),
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

    async fn write_to(
        &mut self,
        file: &mut File,
        mut len: u64,
        control: &mut ReceiverControl,
        on_arrived: &mut (dyn FnMut(u64) + Send),
        mut hasher: Option<&mut Hasher>,
    ) -> Result<(), Error> {
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
                let ack = ReceiverMessage::Ack {
                    bytes: self.written,
                };
                control.send(&ack).await?;
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
