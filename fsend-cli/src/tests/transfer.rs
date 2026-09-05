use super::test_support::*;
use super::*;

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
fn a_full_resume_round_trip_agrees_with_itself() {
    // Receiver computes the skip list, both sides apply it identically.
    let offered = vec![
        file("big.bin", 1000),
        dir("d", vec![file("a", 10), file("b", 20)]),
        file("new.txt", 5),
    ];
    let local = [
        Some(file("big.bin", 400)),
        Some(dir("d", vec![file("a", 10)])),
        None,
    ];
    let skips: Vec<Option<FilesToSkip>> = offered
        .iter()
        .zip(&local)
        .map(|(o, l)| l.as_ref().and_then(|l| o.get_skippable(l)))
        .collect();
    assert_eq!(
        skips,
        vec![
            Some(skip_file("big.bin", 400)),
            Some(skip_dir("d", vec![skip_file("a", 10)])),
            None
        ]
    );

    let plan = plan_transfer(&offered, &skips);
    assert_eq!(
        plan.progress,
        vec![
            ("big.bin".to_string(), 400, 1000),
            ("d".to_string(), 10, 30),
            ("new.txt".to_string(), 0, 5)
        ]
    );
    assert_eq!(
        plan.to_transfer,
        vec![
            Some(FileSendRecvTree::File {
                name: "big.bin".into(),
                skip: 400,
                size: 1000
            }),
            Some(FileSendRecvTree::Dir {
                name: "d".into(),
                files: vec![FileSendRecvTree::File {
                    name: "b".into(),
                    skip: 0,
                    size: 20
                }]
            }),
            Some(FileSendRecvTree::File {
                name: "new.txt".into(),
                skip: 0,
                size: 5
            }),
        ]
    );
}

#[test]
fn flatten_walks_in_wire_order() {
    let trees = vec![
        FileSendRecvTree::File {
            name: "top.txt".into(),
            skip: 2,
            size: 9,
        },
        FileSendRecvTree::Dir {
            name: "d".into(),
            files: vec![
                FileSendRecvTree::File {
                    name: "a".into(),
                    skip: 0,
                    size: 1,
                },
                FileSendRecvTree::Dir {
                    name: "e".into(),
                    files: vec![],
                },
                FileSendRecvTree::Dir {
                    name: "f".into(),
                    files: vec![FileSendRecvTree::File {
                        name: "b".into(),
                        skip: 1,
                        size: 3,
                    }],
                },
            ],
        },
    ];

    // The receiver puts every top-level entry under one output directory.
    let out = Path::new("out");
    let mut entries = Vec::new();
    for tree in &trees {
        entries.extend(flatten_trees(
            std::slice::from_ref(tree),
            &out.join(tree.name()),
        ));
    }
    assert_eq!(
        entries,
        vec![
            TransferEntry::File {
                path: out.join("top.txt"),
                skip: 2,
                size: 9
            },
            TransferEntry::Dir {
                path: out.join("d")
            },
            TransferEntry::File {
                path: out.join("d/a"),
                skip: 0,
                size: 1
            },
            TransferEntry::Dir {
                path: out.join("d/e")
            },
            TransferEntry::Dir {
                path: out.join("d/f")
            },
            TransferEntry::File {
                path: out.join("d/f/b"),
                skip: 1,
                size: 3
            },
        ]
    );

    // The sender uses the argument path itself as the root.
    let dot = flatten_trees(&trees[1..], Path::new("."));
    assert_eq!(
        dot[1],
        TransferEntry::File {
            path: PathBuf::from("./a"),
            skip: 0,
            size: 1
        }
    );
    assert!(flatten_trees(&[], Path::new("x")).is_empty());
}

#[test]
fn reads_a_tree_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());

    let tree = get_files_available(&root).unwrap();
    assert_eq!(tree.name(), "album");
    assert_eq!(tree.size(), 10_000 + 70_000 + 3);

    let FilesAvailable::Dir { files, .. } = &tree else {
        panic!("expected a directory");
    };
    let mut names: Vec<_> = files.iter().map(|f| f.name().to_string()).collect();
    names.sort();
    assert_eq!(names, ["a.txt", "empty.bin", "nested"]);

    let nested = files.iter().find(|f| f.name() == "nested").unwrap();
    let FilesAvailable::Dir { files, .. } = nested else {
        panic!("expected a directory");
    };
    assert!(files.iter().any(|f| matches!(
        f,
        FilesAvailable::Dir { name, files } if name == "empty-dir" && files.is_empty()
    )));

    let single = get_files_available(&root.join("a.txt")).unwrap();
    assert_eq!(single, file("a.txt", 10_000));

    assert!(get_files_available(&root.join("nope")).is_err());
}

#[test]
fn dot_is_announced_under_its_real_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    let cwd_name = root.file_name().unwrap().to_str().unwrap().to_owned();

    let tree = get_files_available(&root.join(".")).unwrap();
    assert_eq!(tree.name(), cwd_name);
    let tree = get_files_available(&root.join("nested/..")).unwrap();
    assert_eq!(tree.name(), cwd_name);

    let err = get_files_available(Path::new("/")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn skip_list_reads_the_output_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path();
    write(&out.join("big.bin"), &bytes(400, 1));
    write(&out.join("d/a"), &bytes(10, 2));
    write(&out.join("too-big.bin"), &bytes(50, 3));

    let offered = vec![
        file("big.bin", 1000),
        dir("d", vec![file("a", 10), file("b", 20)]),
        file("new.txt", 5),
        file("too-big.bin", 20),
    ];

    assert_eq!(
        skip_list(&offered, out, false),
        vec![None, None, None, None],
        "overwrite mode never skips"
    );
    assert_eq!(
        skip_list(&offered, out, true),
        vec![
            Some(skip_file("big.bin", 400)),
            Some(skip_dir("d", vec![skip_file("a", 10)])),
            None,
            Some(skip_file("too-big.bin", 0)),
        ]
    );
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
async fn gzip_round_trips() {
    for data in [Vec::new(), b"hello".to_vec(), bytes(100_000, 7)] {
        let compressed = compress_gzip(&data).await.unwrap();
        assert_eq!(decompress_gzip(&compressed).await.unwrap(), data);
    }
    assert!(decompress_gzip(b"definitely not gzip").await.is_err());
}

#[test]
fn rkyv_packets_round_trip() {
    let packets = vec![
        SenderToReceiver::ConnRequest {
            version: PROTO_VERSION.into(),
        },
        SenderToReceiver::FileInfo {
            files: vec![file("a", 1), dir("d", vec![file("b", 2), dir("e", vec![])])],
        },
        SenderToReceiver::FileInfo { files: vec![] },
    ];
    for packet in packets {
        let wire = packet.to_wire().unwrap();
        assert_eq!(SenderToReceiver::from_wire(&wire).unwrap(), packet);
    }

    let packets = vec![
        ReceiverToSender::Ok,
        ReceiverToSender::RejectFiles,
        ReceiverToSender::WrongVersion {
            expected: "9.9.9".into(),
        },
        ReceiverToSender::AcceptFilesSkip {
            files: vec![None, Some(skip_file("a", 5)), Some(skip_dir("d", vec![]))],
        },
    ];
    for packet in packets {
        let wire = packet.to_wire().unwrap();
        assert_eq!(ReceiverToSender::from_wire(&wire).unwrap(), packet);
    }
}

#[test]
fn rkyv_rejects_garbage_and_the_wrong_packet_type() {
    assert!(ReceiverToSender::from_wire(b"").is_err());
    assert!(ReceiverToSender::from_wire(b"nonsense bytes here").is_err());

    // Decoding through a shifted slice must not depend on the caller's
    // buffer alignment.
    let wire = ReceiverToSender::Ok.to_wire().unwrap();
    let mut shifted = vec![0u8; 1];
    shifted.extend_from_slice(&wire);
    assert_eq!(
        ReceiverToSender::from_wire(&shifted[1..]).unwrap(),
        ReceiverToSender::Ok
    );
}

#[test]
fn user_errors_are_worded_for_humans() {
    colored::control::set_override(false);

    assert_eq!(
        TransferError::Declined.user_error().as_deref(),
        Some("Transfer declined.")
    );
    assert_eq!(
        TransferError::PeerDeclined.user_error().as_deref(),
        Some("error: the peer declined the files")
    );
    assert_eq!(
        TransferError::PathsNotFound(vec!["a".into()])
            .user_error()
            .as_deref(),
        Some("error: 1 path does not exist:\n - a")
    );
    assert_eq!(
        TransferError::PathsNotFound(vec!["a".into(), "b/c".into()])
            .user_error()
            .as_deref(),
        Some("error: 2 paths do not exist:\n - a\n - b/c")
    );
    let lost = TransferError::Disconnected("timed out".into())
        .user_error()
        .unwrap();
    assert!(lost.contains("timed out") && lost.contains("resume"));
    let version = TransferError::WrongVersion("0.1.0".into(), "0.0.1".into())
        .user_error()
        .unwrap();
    assert!(version.contains("0.1.0") && version.contains("0.0.1"));

    assert!(TransferError::UnexpectedPacket.user_error().is_none());
    assert!(TransferError::PeerTimeout.user_error().is_some());
    assert!(TransferError::Protocol("x".into()).user_error().is_none());
    assert!(TransferError::Internal("x").user_error().is_none());
    assert!(TransferError::UnexpectedConnectionInfo
        .user_error()
        .is_none());
    assert!(TransferError::FileNotFound("x".into())
        .user_error()
        .is_none());
    assert!(TransferError::Io(std::io::Error::other("x"))
        .user_error()
        .is_none());
}
