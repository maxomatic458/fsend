use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{DuplexStream, ReadBuf};
use tokio::sync::watch;
use tokio_util::sync::WaitForCancellationFutureOwned;

use super::test_support::*;
use super::*;

/// One end of an in-memory connection.
struct Memory {
    control: Option<Aborting>,
    data: Option<Aborting>,
    aborted: CancellationToken,
    /// Once closed: whether this side was done.
    done: watch::Sender<Option<bool>>,
    peer: watch::Receiver<Option<bool>>,
}

/// A stream that fails once its connection is aborted.
struct Aborting {
    stream: DuplexStream,
    aborted: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl Aborting {
    fn new(stream: DuplexStream, aborted: &CancellationToken) -> Self {
        Self {
            stream,
            aborted: Box::pin(aborted.clone().cancelled_owned()),
        }
    }

    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        match self.aborted.as_mut().poll(cx) {
            Poll::Ready(()) => Err(io::ErrorKind::ConnectionReset.into()),
            Poll::Pending => Ok(()),
        }
    }
}

impl AsyncRead for Aborting {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Aborting {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[async_trait]
impl Transport for Memory {
    async fn connection_kind(&mut self) -> ConnectionKind {
        ConnectionKind::Unknown
    }

    async fn control(&mut self) -> Result<Box<dyn ByteStream>, Error> {
        let stream = self.control.take().ok_or(Error::Internal("taken"))?;
        Ok(Box::new(stream))
    }

    async fn data(&mut self) -> Result<Box<dyn ByteStream>, Error> {
        let stream = self.data.take().ok_or(Error::Internal("taken"))?;
        Ok(Box::new(stream))
    }

    async fn closed(&mut self) -> Result<(), Error> {
        let lost = || Error::Disconnected("the peer gave up".into());
        tokio::select! {
            _ = self.aborted.cancelled() => Err(lost()),
            done = self.peer.wait_for(Option::is_some) => match done.map(|done| *done) {
                Ok(Some(true)) => Ok(()),
                _ => Err(lost()),
            },
        }
    }

    async fn close(&mut self, ok: bool) {
        (self.control, self.data) = (None, None);
        self.done.send_replace(Some(ok));
    }
}

fn pair() -> Pin<Box<dyn Future<Output = Peers> + Send>> {
    Box::pin(async {
        let aborted = CancellationToken::new();
        let (control, peer_control) = tokio::io::duplex(4 * BUF_SIZE);
        let (data, peer_data) = tokio::io::duplex(4 * BUF_SIZE);
        let (sender_done, sender_closed) = watch::channel(None);
        let (receiver_done, receiver_closed) = watch::channel(None);
        let end = |control, data, done, peer| Memory {
            control: Some(Aborting::new(control, &aborted)),
            data: Some(Aborting::new(data, &aborted)),
            aborted: aborted.clone(),
            done,
            peer,
        };
        let abort = || -> AbortFn {
            let aborted = aborted.clone();
            Box::new(move || aborted.cancel())
        };
        Peers {
            sender: Box::new(end(control, data, sender_done, receiver_closed)),
            receiver: Box::new(end(peer_control, peer_data, receiver_done, sender_closed)),
            abort_sender: abort(),
            abort_receiver: abort(),
        }
    })
}

transport_suite!();

fn file(path: &[&str], size: u64) -> Entry {
    Entry::File {
        path: path.iter().map(|s| s.to_string()).collect(),
        size,
        checksum: None,
    }
}

fn dir(path: &[&str]) -> Entry {
    Entry::Dir {
        path: path.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn offers_a_tree_parents_first_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());

    let outgoing = build_offer(std::slice::from_ref(&root)).unwrap();
    assert_eq!(
        outgoing.offer.flatten(),
        [
            dir(&["album"]),
            file(&["album", "a.txt"], 10_000),
            file(&["album", "empty.bin"], 0),
            dir(&["album", "nested"]),
            file(&["album", "nested", "b.txt"], 70_000),
            dir(&["album", "nested", "deeper"]),
            file(&["album", "nested", "deeper", "c.txt"], 3),
            dir(&["album", "nested", "empty-dir"]),
        ]
    );
    assert_eq!(outgoing.sources[4], root.join("nested/b.txt"));

    let picked = build_offer(&[root.join("nested"), root.join("a.txt")]).unwrap();
    let names: Vec<_> = picked.offer.entries.iter().map(Node::name).collect();
    assert_eq!(names, ["a.txt", "nested"]);
    assert_eq!(
        picked.sources[..2],
        [root.join("a.txt"), root.join("nested")]
    );
}

#[test]
fn dot_is_offered_under_its_real_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    let name = |path: PathBuf| {
        build_offer(&[path]).unwrap().offer.entries[0]
            .name()
            .to_owned()
    };

    assert_eq!(name(root.join(".")), "album");
    assert_eq!(name(root.join("nested/..")), "album");
    assert!(matches!(
        build_offer(&["/".into()]),
        Err(Error::Io(e)) if e.kind() == io::ErrorKind::InvalidInput
    ));
}

#[test]
fn an_unsendable_selection_fails_before_pairing() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        build_offer(&[tmp.path().join("nope")]),
        Err(Error::PathsNotFound(p)) if p == [tmp.path().join("nope")]
    ));

    // Fine on this system, but not a portable name.
    write(&tmp.path().join("a\\b"), b"x");
    assert!(matches!(
        build_offer(&[tmp.path().join("a\\b")]),
        Err(Error::Unsendable(OfferError::Entry { id: 0, .. }))
    ));

    write(&tmp.path().join("x/same"), b"1");
    write(&tmp.path().join("y/same"), b"2");
    assert!(matches!(
        build_offer(&[tmp.path().join("x/same"), tmp.path().join("y/same")]),
        Err(Error::Unsendable(OfferError::Entry {
            id: 1,
            reason: EntryError::Duplicate
        }))
    ));
}

#[test]
fn local_paths_stay_under_the_output_directory() {
    let root = Path::new("out");
    let path = |names: &[&str]| {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        local_path(root, &names)
    };
    assert_eq!(path(&["d", "a.txt"]).unwrap(), root.join("d/a.txt"));
    for bad in [&[".."][..], &["d", "."], &["/etc"], &["a/b"], &[""]] {
        assert!(path(bad).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn received_files_are_truncated_to_the_offered_size() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("f.bin");
    write(&path, &bytes(100, 9));

    let mut f = open_for_receive(&path, 0).await.unwrap();
    f.write_all(&bytes(30, 1)).await.unwrap();
    finish_received_file(&mut f, 30).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes(30, 1));

    // Resuming keeps the prefix.
    let mut f = open_for_receive(&path, 30).await.unwrap();
    f.write_all(&bytes(5, 2)).await.unwrap();
    finish_received_file(&mut f, 35).await.unwrap();
    let mut expected = bytes(30, 1);
    expected.extend(bytes(5, 2));
    assert_eq!(std::fs::read(&path).unwrap(), expected);

    let fresh = tmp.path().join("new.bin");
    let mut f = open_for_receive(&fresh, 0).await.unwrap();
    finish_received_file(&mut f, 0).await.unwrap();
    assert_eq!(std::fs::read(&fresh).unwrap(), b"");
}

#[tokio::test]
async fn control_messages_round_trip_over_a_byte_stream() {
    let (a, b) = tokio::io::duplex(1024);
    let (mut a, mut b) = (
        SenderControl::new(Box::new(a)),
        ReceiverControl::new(Box::new(b)),
    );

    let messages = [
        SenderMessage::Hashes { hashes: vec![] },
        // Compressed, and bigger than any single WebRTC message.
        SenderMessage::Hashes {
            hashes: (0..3000).map(|i| Fingerprint(i * 31)).collect(),
        },
    ];
    let send = async {
        for message in &messages {
            a.send(message).await.unwrap();
        }
    };
    let receive = async { [b.recv().await.unwrap(), b.recv().await.unwrap()] };
    let ((), got) = tokio::join!(send, receive);
    assert_eq!(got, messages);

    b.send(&ReceiverMessage::Ack { bytes: 5 }).await.unwrap();
    b.send(&ReceiverMessage::Done).await.unwrap();
    assert_eq!(a.recv().await.unwrap(), ReceiverMessage::Ack { bytes: 5 });
    assert_eq!(a.recv().await.unwrap(), ReceiverMessage::Done);

    // A message of the wrong side, an unknown one, a malformed one, and a malformed frame.
    let receivers = frame::encode(&ReceiverMessage::Done).unwrap();
    for frame in [
        receivers,
        json_frame(r#"{"type":"resume"}"#),
        json_frame(r#"{"type":"hashes"}"#),
        vec![0, 0, 0, 1, 7],
    ] {
        a.send_frame(&frame).await.unwrap();
        assert!(
            matches!(b.recv().await, Err(Error::Protocol(_))),
            "{frame:?}"
        );
    }
    let (c, d) = tokio::io::duplex(64);
    drop(c);
    assert!(matches!(
        ReceiverControl::new(Box::new(d)).recv().await,
        Err(Error::Disconnected(_))
    ));
}

#[test]
fn errors_tell_the_peer_what_it_needs_to_know() {
    let code = |error: Error| report(&error).map(|report| report.code);
    assert_eq!(
        code(Error::Io(io::Error::other("full"))),
        Some(ErrorCode::Io)
    );
    assert_eq!(code(Error::Protocol("x".into())), Some(ErrorCode::Protocol));
    assert_eq!(code(Error::Timeout("x")), Some(ErrorCode::Timeout));
    assert_eq!(code(Error::Internal("x")), Some(ErrorCode::Internal));
    // The peer already knows, cannot be told, or is better not told.
    assert_eq!(code(Error::ChecksumMismatch("a".into())), None);
    assert_eq!(code(Error::PeerCancelled(None)), None);
    assert_eq!(code(Error::Disconnected("x".into())), None);
    let peer = Error::Peer(ErrorReport {
        code: ErrorCode::Io,
        message: "x".into(),
    });
    assert_eq!(code(peer), None);
}

#[test]
fn a_lost_peer_is_told_apart_from_other_io_errors() {
    for kind in [io::ErrorKind::ConnectionReset, io::ErrorKind::BrokenPipe] {
        let error: Error = io::Error::from(kind).into();
        assert!(matches!(error, Error::Disconnected(_)), "{error:?}");
    }
    let error: Error = io::Error::from(io::ErrorKind::StorageFull).into();
    assert!(matches!(error, Error::Io(_)), "{error:?}");
}
