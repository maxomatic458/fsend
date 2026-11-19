use super::test_support::*;
use super::*;

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
fn offers_a_tree_parents_first() {
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

    let single = build_offer(&[root.join("a.txt")]).unwrap();
    assert_eq!(single.offer.flatten(), [file(&["a.txt"], 10_000)]);
}

#[test]
fn dot_is_announced_under_its_real_name() {
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
        Err(TransferError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput
    ));
}

#[test]
fn an_unsendable_selection_fails_before_pairing() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        build_offer(&[tmp.path().join("nope")]),
        Err(TransferError::PathsNotFound(p)) if p == [tmp.path().join("nope")]
    ));

    // Fine on this system, but not a portable name.
    write(&tmp.path().join("a\\b"), b"x");
    assert!(matches!(
        build_offer(&[tmp.path().join("a\\b")]),
        Err(TransferError::Unsendable(OfferError::Entry { id: 0, .. }))
    ));

    // The same name twice.
    write(&tmp.path().join("x/same"), b"1");
    write(&tmp.path().join("y/same"), b"2");
    assert!(matches!(
        build_offer(&[tmp.path().join("x/same"), tmp.path().join("y/same")]),
        Err(TransferError::Unsendable(OfferError::Entry {
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

    // Truncate to a shorter size.
    let mut f = open_for_receive(&path, 0).await.unwrap();
    f.write_all(&bytes(30, 1)).await.unwrap();
    finish_received_file(&mut f, 30).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes(30, 1));

    // Resume keeps the prefix and appends.
    let mut f = open_for_receive(&path, 30).await.unwrap();
    f.write_all(&bytes(5, 2)).await.unwrap();
    finish_received_file(&mut f, 35).await.unwrap();
    let mut expected = bytes(30, 1);
    expected.extend(bytes(5, 2));
    assert_eq!(std::fs::read(&path).unwrap(), expected);

    // Creates missing files.
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
        SenderMessage::Hello(peer::hello(None)),
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
            matches!(b.recv().await, Err(TransferError::Protocol(_))),
            "{frame:?}"
        );
    }
    let (c, d) = tokio::io::duplex(64);
    drop(c);
    assert!(matches!(
        ReceiverControl::new(Box::new(d)).recv().await,
        Err(TransferError::Disconnected(_))
    ));
}

#[test]
fn errors_tell_the_peer_what_it_needs_to_know() {
    let code = |error: TransferError| error.report().map(|report| report.code);
    assert_eq!(
        code(TransferError::Io(io::Error::other("disk full"))),
        Some(ErrorCode::Io)
    );
    assert_eq!(
        code(TransferError::Version {
            version: 9,
            min_version: 9
        }),
        Some(ErrorCode::Version)
    );
    assert_eq!(
        code(TransferError::Protocol("x".into())),
        Some(ErrorCode::Protocol)
    );
    assert_eq!(code(TransferError::Timeout("x")), Some(ErrorCode::Timeout));
    assert_eq!(
        code(TransferError::Internal("x")),
        Some(ErrorCode::Internal)
    );
    // The peer already knows, cannot be told, or is better not told.
    assert_eq!(code(TransferError::ChecksumMismatch("a".into())), None);
    assert_eq!(code(TransferError::Declined), None);
    assert_eq!(code(TransferError::PeerDeclined(None)), None);
    assert_eq!(code(TransferError::PeerCancelled(None)), None);
    assert_eq!(code(TransferError::Disconnected("x".into())), None);
    assert_eq!(
        code(TransferError::Peer {
            code: ErrorCode::Io,
            message: "x".into()
        }),
        None
    );
}

/// A GUI runs each transfer on a task of its own.
#[test]
fn transfers_can_be_spawned() {
    fn spawnable<A, B, F: Future + Send + 'static>(_: fn(A, B) -> F) {}
    async fn send(mut transport: Box<dyn Transport>, outgoing: Outgoing) {
        let _ = send_files(
            transport.as_mut(),
            &outgoing,
            true,
            &mut |_| {},
            CancellationToken::new(),
        )
        .await;
    }
    async fn receive(mut transport: Box<dyn Transport>, mut resolve: Box<dyn Resolve>) {
        let _ = receive_files(
            transport.as_mut(),
            &Offer::default(),
            Path::new("out"),
            resolve.as_mut(),
            &mut |_| {},
            CancellationToken::new(),
        )
        .await;
    }
    spawnable(send);
    spawnable(receive);
}

#[test]
fn user_errors_are_worded_for_humans() {
    assert_eq!(
        TransferError::Declined.user_message().as_deref(),
        Some("transfer declined")
    );
    assert_eq!(
        TransferError::PeerDeclined(None).user_message().as_deref(),
        Some("the peer declined the files")
    );
    assert_eq!(
        TransferError::PathsNotFound(vec!["a".into()])
            .user_message()
            .as_deref(),
        Some("1 path does not exist:\n - a")
    );
    assert_eq!(
        TransferError::PathsNotFound(vec!["a".into(), "b/c".into()])
            .user_message()
            .as_deref(),
        Some("2 paths do not exist:\n - a\n - b/c")
    );
    let lost = TransferError::Disconnected("timed out".into())
        .user_message()
        .unwrap();
    assert!(lost.contains("timed out") && lost.contains("resume"));
    let version = TransferError::Version {
        version: 9,
        min_version: 7,
    }
    .user_message()
    .unwrap();
    assert!(version.contains("7 to 9") && version.contains("Update"));
    let peer_version = TransferError::Peer {
        code: ErrorCode::Version,
        message: "versions 5 to 6 are supported".into(),
    }
    .user_message()
    .unwrap();
    assert!(peer_version.contains("5 to 6") && peer_version.contains("Update"));

    for error in [
        TransferError::PeerCancelled(None),
        TransferError::Timeout("x"),
        TransferError::PeerTimeout,
        TransferError::Unsendable(OfferError::Empty),
        TransferError::ChecksumMismatch("a".into()),
    ] {
        assert!(error.user_message().is_some(), "{error:?}");
    }
    for error in [
        TransferError::Protocol("x".into()),
        TransferError::Internal("x"),
        TransferError::UnexpectedConnectionInfo,
        TransferError::Io(io::Error::other("x")),
    ] {
        assert!(error.user_message().is_none(), "{error:?}");
    }
}
