//! Transport testing suite.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use super::*;

/// Deterministic incompressable bytes
pub fn bytes(size: usize, seed: u8) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (seed as u64 + 1);
    (0..size)
        .map(|_| {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
        })
        .collect()
}

pub fn write(path: &Path, data: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, data).unwrap();
}

pub fn sample_tree(root: &Path) -> PathBuf {
    let dir = root.join("album");
    write(&dir.join("a.txt"), &bytes(10_000, 1));
    write(&dir.join("empty.bin"), &[]);
    write(&dir.join("nested/b.txt"), &bytes(70_000, 2));
    write(&dir.join("nested/deeper/c.txt"), &bytes(3, 3));
    std::fs::create_dir_all(dir.join("nested/empty-dir")).unwrap();
    dir
}

/// Every file under `root`, as `relative path -> content`, sorted.
pub fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.push((format!("{rel}/"), Vec::new()));
                walk(&path, base, out);
            } else {
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

pub fn no_progress(_: &[(String, u64, u64)]) {}
pub fn no_wait() {}
pub fn no_data(_: u64) {}
pub fn decline(_: &[FilesAvailable]) -> Option<PathBuf> {
    None
}
pub fn no_offer(_: &[FilesAvailable]) -> Option<PathBuf> {
    panic!("must not get as far as the offer")
}

/// Kills one side's connection from the outside.
pub type AbortFn = Box<dyn Fn() + Send + Sync>;

pub struct Peers {
    pub sender: Box<dyn Transport>,
    pub receiver: Box<dyn Transport>,
    pub abort_sender: AbortFn,
    pub abort_receiver: AbortFn,
}

pub type PairFn = fn() -> Pin<Box<dyn Future<Output = Peers> + Send>>;

#[derive(Default)]
pub struct RunOptions {
    pub resume: bool,
    pub decline: bool,
    /// Abort the sender once it has handed this many bytes to the transport.
    pub abort_sender_after: Option<u64>,
    /// Abort the receiver once it has written this many bytes.
    pub abort_receiver_after: Option<u64>,
}

/// Everything observable about one send/receive run.
pub struct Outcome {
    pub send: Result<(), TransferError>,
    pub recv: Result<(), TransferError>,
    pub sender_initial: Option<Vec<(String, u64, u64)>>,
    pub receiver_initial: Option<Vec<(String, u64, u64)>>,
    pub offered: Option<Vec<FilesAvailable>>,
    pub sent: u64,
    pub received: u64,
    pub waited: bool,
}

impl Outcome {
    pub fn unwrap(&self) {
        if let Err(e) = &self.send {
            panic!("send failed: {e:?}");
        }
        if let Err(e) = &self.recv {
            panic!("receive failed: {e:?}");
        }
    }
}

/// Runs one transfer of `files` into `out` between the two peers.
pub async fn run(peers: Peers, files: Vec<PathBuf>, out: &Path, opts: RunOptions) -> Outcome {
    let Peers {
        mut sender,
        mut receiver,
        abort_sender,
        abort_receiver,
    } = peers;

    let mut sender_initial = None;
    let mut receiver_initial = None;
    let mut offered = None;
    let mut sent = 0u64;
    let mut received = 0u64;
    let mut waited = false;
    let mut sender_aborted = false;
    let mut receiver_aborted = false;
    let out_dir = out.to_path_buf();

    let mut on_sender_initial = |p: &[(String, u64, u64)]| sender_initial = Some(p.to_vec());
    let mut on_waiting = || waited = true;
    let mut on_write = |n: u64| {
        sent += n;
        if let Some(limit) = opts.abort_sender_after
            && sent >= limit
            && !sender_aborted
        {
            sender_aborted = true;
            abort_sender();
        }
    };
    let mut on_receiver_initial = |p: &[(String, u64, u64)]| receiver_initial = Some(p.to_vec());
    let mut on_offer = |files: &[FilesAvailable]| {
        offered = Some(files.to_vec());
        (!opts.decline).then(|| out_dir.clone())
    };
    let mut on_read = |n: u64| {
        received += n;
        if let Some(limit) = opts.abort_receiver_after
            && received >= limit
            && !receiver_aborted
        {
            receiver_aborted = true;
            abort_receiver();
        }
    };

    let send = send_files(
        sender.as_mut(),
        SendArgs { files },
        &mut on_sender_initial,
        &mut on_waiting,
        &mut on_write,
    );
    let recv = receive_files(
        receiver.as_mut(),
        ReceiveArgs {
            resume: opts.resume,
        },
        &mut on_receiver_initial,
        &mut on_offer,
        &mut on_read,
    );
    let (send, recv) = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        tokio::join!(send, recv)
    })
    .await
    .expect("the transfer hung");

    Outcome {
        send,
        recv,
        sender_initial,
        receiver_initial,
        offered,
        sent,
        received,
        waited,
    }
}

pub mod suite {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        src: PathBuf,
        out: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        Fixture {
            _tmp: tmp,
            src,
            out,
        }
    }

    fn progress(name: &str, skip: u64, size: u64) -> Vec<(String, u64, u64)> {
        vec![(name.to_string(), skip, size)]
    }

    pub async fn sends_a_single_file(pair: PairFn) {
        let f = fixture();
        let data = bytes(100_000, 1);
        write(&f.src.join("photo.jpg"), &data);

        let o = run(
            pair().await,
            vec![f.src.join("photo.jpg")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("photo.jpg")).unwrap(), data);
        assert_eq!(o.sender_initial.unwrap(), progress("photo.jpg", 0, 100_000));
        assert_eq!(
            o.receiver_initial.unwrap(),
            progress("photo.jpg", 0, 100_000)
        );
        assert_eq!(
            o.offered.unwrap(),
            vec![FilesAvailable::File {
                name: "photo.jpg".into(),
                size: 100_000
            }]
        );
        assert_eq!(o.sent, 100_000);
        assert_eq!(o.received, 100_000);
        assert!(o.waited, "the sender must report waiting for the accept");
    }

    pub async fn sends_an_empty_file(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("empty"), &[]);

        let o = run(
            pair().await,
            vec![f.src.join("empty")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("empty")).unwrap(), b"");
        assert_eq!(o.sent, 0);
        assert_eq!(o.received, 0);
    }

    pub async fn sends_a_file_larger_than_the_buffers(pair: PairFn) {
        let f = fixture();
        // Not a multiple of any buffer or chunk size in play.
        let data = bytes(1024 * 1024 + 17, 5);
        write(&f.src.join("big.bin"), &data);

        let o = run(
            pair().await,
            vec![f.src.join("big.bin")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("big.bin")).unwrap(), data);
        assert_eq!(o.sent, data.len() as u64);
        assert_eq!(o.received, data.len() as u64);
    }

    pub async fn sends_a_directory_tree(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);

        let o = run(pair().await, vec![album], &f.out, RunOptions::default()).await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender_initial.unwrap(), progress("album", 0, 80_003));
        assert_eq!(o.sent, 80_003);
        assert_eq!(o.received, 80_003);
    }

    pub async fn sends_several_entries_at_once(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);
        write(&f.src.join("first.txt"), &bytes(500, 7));
        write(&f.src.join("last.txt"), &bytes(0, 8));

        let o = run(
            pair().await,
            vec![f.src.join("first.txt"), album, f.src.join("last.txt")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(
            o.sender_initial.unwrap(),
            vec![
                ("first.txt".to_string(), 0, 500),
                ("album".to_string(), 0, 80_003),
                ("last.txt".to_string(), 0, 0),
            ]
        );
        assert_eq!(o.sent, 80_503);
    }

    pub async fn sends_the_current_directory(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);

        // `fsend send .` hands over a path without a file name of its
        // own.
        let o = run(
            pair().await,
            vec![album.join("nested/..")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender_initial.unwrap(), progress("album", 0, 80_003));
    }

    pub async fn receiver_can_decline(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("secret.txt"), &bytes(100, 1));

        let o = run(
            pair().await,
            vec![f.src.join("secret.txt")],
            &f.out,
            RunOptions {
                decline: true,
                ..Default::default()
            },
        )
        .await;

        assert!(
            matches!(o.recv, Err(TransferError::Declined)),
            "{:?}",
            o.recv
        );
        assert!(
            matches!(o.send, Err(TransferError::PeerDeclined)),
            "{:?}",
            o.send
        );
        assert!(o.offered.is_some(), "the receiver saw the offer");
        assert!(o.sender_initial.is_none() && o.receiver_initial.is_none());
        assert_eq!(o.sent, 0);
        assert!(!f.out.join("secret.txt").exists());
    }

    pub async fn resumes_a_partial_file(pair: PairFn) {
        let f = fixture();
        let data = bytes(200_000, 3);
        write(&f.src.join("big.bin"), &data);
        write(&f.out.join("big.bin"), &data[..80_000]);

        let o = run(
            pair().await,
            vec![f.src.join("big.bin")],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("big.bin")).unwrap(), data);
        assert_eq!(
            o.sender_initial.unwrap(),
            progress("big.bin", 80_000, 200_000)
        );
        assert_eq!(
            o.receiver_initial.unwrap(),
            progress("big.bin", 80_000, 200_000)
        );
        assert_eq!(o.sent, 120_000);
        assert_eq!(o.received, 120_000);
    }

    pub async fn resumes_a_partial_directory(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);
        // a.txt half there, b.txt complete, c.txt and empty.bin missing.
        write(&f.out.join("album/a.txt"), &bytes(10_000, 1)[..4_000]);
        write(&f.out.join("album/nested/b.txt"), &bytes(70_000, 2));

        let o = run(
            pair().await,
            vec![album],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender_initial.unwrap(), progress("album", 74_000, 80_003));
        assert_eq!(o.sent, 6_003);
        assert_eq!(o.received, 6_003);
    }

    pub async fn resume_skips_complete_files(pair: PairFn) {
        let f = fixture();
        let data = bytes(50_000, 4);
        write(&f.src.join("done.bin"), &data);
        write(&f.out.join("done.bin"), &data);

        let o = run(
            pair().await,
            vec![f.src.join("done.bin")],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("done.bin")).unwrap(), data);
        assert_eq!(
            o.sender_initial.unwrap(),
            progress("done.bin", 50_000, 50_000)
        );
        assert_eq!(o.sent, 0);
        assert_eq!(o.received, 0);
    }

    pub async fn resume_starts_over_when_the_local_file_is_larger(pair: PairFn) {
        let f = fixture();
        let data = bytes(200, 1);
        write(&f.src.join("f.bin"), &data);
        write(&f.out.join("f.bin"), &bytes(300, 9));

        let o = run(
            pair().await,
            vec![f.src.join("f.bin")],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("f.bin")).unwrap(), data);
        assert_eq!(o.sender_initial.unwrap(), progress("f.bin", 0, 200));
        assert_eq!(o.sent, 200);
    }

    pub async fn overwrite_ignores_partial_files(pair: PairFn) {
        let f = fixture();
        let data = bytes(200_000, 3);
        write(&f.src.join("big.bin"), &data);
        write(&f.out.join("big.bin"), &bytes(80_000, 9));

        let o = run(
            pair().await,
            vec![f.src.join("big.bin")],
            &f.out,
            RunOptions {
                resume: false,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("big.bin")).unwrap(), data);
        assert_eq!(o.sender_initial.unwrap(), progress("big.bin", 0, 200_000));
        assert_eq!(o.sent, 200_000);
    }

    pub async fn overwrite_truncates_a_larger_stale_file(pair: PairFn) {
        let f = fixture();
        let data = bytes(200, 1);
        write(&f.src.join("f.bin"), &data);
        write(&f.out.join("f.bin"), &bytes(300, 9));

        let o = run(
            pair().await,
            vec![f.src.join("f.bin")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("f.bin")).unwrap(), data);
    }

    const INTERRUPT_SIZE: usize = 3 * 1024 * 1024;
    const INTERRUPT_AFTER: u64 = 1024 * 1024;

    pub async fn interrupted_by_the_receiver_then_resumed(pair: PairFn) {
        let f = fixture();
        let data = bytes(INTERRUPT_SIZE, 6);
        write(&f.src.join("movie.bin"), &data);

        let o = run(
            pair().await,
            vec![f.src.join("movie.bin")],
            &f.out,
            RunOptions {
                abort_receiver_after: Some(INTERRUPT_AFTER),
                ..Default::default()
            },
        )
        .await;

        assert!(o.recv.is_err(), "the receiver must notice its own loss");
        assert!(
            o.send.is_err(),
            "the sender must notice the receiver is gone"
        );
        let partial = std::fs::metadata(f.out.join("movie.bin")).unwrap().len();
        assert!(
            (INTERRUPT_AFTER..data.len() as u64).contains(&partial),
            "partial file of {partial} bytes"
        );
        assert_eq!(partial, o.received);
        assert_eq!(
            std::fs::read(f.out.join("movie.bin")).unwrap(),
            &data[..partial as usize],
            "what was written must be a prefix of the file"
        );

        // A fresh session picks up where the last one stopped.
        let o = run(
            pair().await,
            vec![f.src.join("movie.bin")],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(
            o.sender_initial.unwrap(),
            progress("movie.bin", partial, data.len() as u64)
        );
        assert_eq!(o.sent, data.len() as u64 - partial);
        assert_eq!(std::fs::read(f.out.join("movie.bin")).unwrap(), data);
    }

    pub async fn interrupted_by_the_sender_then_resumed(pair: PairFn) {
        let f = fixture();
        let data = bytes(INTERRUPT_SIZE, 6);
        write(&f.src.join("movie.bin"), &data);

        let o = run(
            pair().await,
            vec![f.src.join("movie.bin")],
            &f.out,
            RunOptions {
                abort_sender_after: Some(INTERRUPT_AFTER),
                ..Default::default()
            },
        )
        .await;

        assert!(o.send.is_err(), "the sender must notice its own loss");
        assert!(
            o.recv.is_err(),
            "the receiver must notice the sender is gone"
        );
        // A sender that dies early enough leaves nothing behind at all.
        let partial = std::fs::metadata(f.out.join("movie.bin"))
            .map(|m| m.len())
            .unwrap_or(0);
        assert!(
            partial < data.len() as u64,
            "partial file of {partial} bytes"
        );
        assert_eq!(partial, o.received);
        assert_eq!(
            std::fs::read(f.out.join("movie.bin")).unwrap_or_default(),
            &data[..partial as usize]
        );

        let o = run(
            pair().await,
            vec![f.src.join("movie.bin")],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(
            o.sender_initial.unwrap(),
            progress("movie.bin", partial, data.len() as u64)
        );
        assert_eq!(o.sent, data.len() as u64 - partial);
        assert_eq!(std::fs::read(f.out.join("movie.bin")).unwrap(), data);
    }

    pub async fn resume_restarts_files_that_differ_locally(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);
        write(&f.out.join("album/a.txt"), &bytes(10_000, 1)[..4_000]);
        write(&f.out.join("album/nested/b.txt"), &bytes(30_000, 9));

        let o = run(
            pair().await,
            vec![album],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sent, 6_000 + 70_000 + 3);
    }

    pub async fn resume_resends_a_same_size_file_that_differs(pair: PairFn) {
        let f = fixture();
        let data = bytes(50_000, 4);
        write(&f.src.join("done.bin"), &data);
        write(&f.out.join("done.bin"), &bytes(50_000, 5));

        let o = run(
            pair().await,
            vec![f.src.join("done.bin")],
            &f.out,
            RunOptions {
                resume: true,
                ..Default::default()
            },
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("done.bin")).unwrap(), data);
        assert_eq!(o.sent, 50_000);
    }

    pub async fn sender_fails_when_the_receiver_cannot_store_the_files(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("photo.jpg"), &bytes(1_000, 1));
        std::fs::create_dir_all(f.out.join("photo.jpg")).unwrap();

        let o = run(
            pair().await,
            vec![f.src.join("photo.jpg")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        assert!(o.recv.is_err(), "{:?}", o.recv);
        assert!(o.send.is_err(), "the sender reported success");
    }

    /// Offers `files` from a hand-rolled sender, streaming `data` if accepted.
    /// Returns the receiver's answer, its result and whether it prompted.
    async fn offer_raw(
        pair: PairFn,
        files: Vec<FilesAvailable>,
        data: &[u8],
        out: &Path,
    ) -> (ReceiverToSender, Result<(), TransferError>, bool) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let peer = async {
            let mut control = frame(sender.control().await.unwrap());
            send_packet(
                &mut control,
                &SenderToReceiver::ConnRequest {
                    version: PROTO_VERSION.into(),
                },
            )
            .await
            .unwrap();
            let _: ReceiverToSender = recv_packet(&mut control).await.unwrap();
            send_packet(&mut control, &SenderToReceiver::FileInfo { files })
                .await
                .unwrap();
            let reply: ReceiverToSender = recv_packet(&mut control).await.unwrap();
            if matches!(reply, ReceiverToSender::AcceptFilesSkip { .. }) {
                let mut stream = sender.data().await.unwrap();
                stream.write_all(data).await.unwrap();
                stream.shutdown().await.unwrap();
                let _ = tokio::time::timeout(Duration::from_secs(5), sender.closed()).await;
            }
            drop(control);
            sender.close(false).await;
            reply
        };

        let mut prompted = false;
        let mut offer = |_: &[FilesAvailable]| {
            prompted = true;
            Some(out.to_path_buf())
        };
        let (mut progress, mut read) = (no_progress, no_data);
        let receive = receive_files(
            receiver.as_mut(),
            ReceiveArgs { resume: false },
            &mut progress,
            &mut offer,
            &mut read,
        );
        let (reply, result) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(peer, receive)
        })
        .await
        .expect("the offer hung");
        (reply, result, prompted)
    }

    fn file(name: &str, size: u64) -> FilesAvailable {
        FilesAvailable::File {
            name: name.into(),
            size,
        }
    }

    pub async fn refuses_names_that_escape_the_output_dir(pair: PairFn) {
        let f = fixture();
        let escaped = f.out.parent().unwrap().join("escaped.txt");
        let absolute = f.src.join("absolute.txt");
        let offer = vec![FilesAvailable::Dir {
            name: "photos".into(),
            files: vec![
                file("../../escaped.txt", 4),
                file(absolute.to_str().unwrap(), 4),
            ],
        }];

        let (reply, result, _) = offer_raw(pair, offer, b"evilevil", &f.out).await;

        assert!(!escaped.exists(), "wrote outside the output directory");
        assert!(!absolute.exists(), "wrote to an absolute path");
        assert_eq!(reply, ReceiverToSender::RejectFiles);
        assert!(
            matches!(result, Err(TransferError::InvalidOffer(_))),
            "{result:?}"
        );
    }

    pub async fn refuses_offers_that_would_spoof_the_prompt(pair: PairFn) {
        let offers = [
            vec![file("\x1b[1A\x1b[2Kreport.pdf", 1)],
            vec![file("invoice\u{202E}fdp.exe", 1)],
            vec![file("a", u64::MAX), file("b", 2)],
        ];
        for offer in offers {
            let f = fixture();
            let (reply, result, prompted) = offer_raw(pair, offer, b"x", &f.out).await;
            assert!(!prompted, "the prompt must not show a spoofed offer");
            assert_eq!(reply, ReceiverToSender::RejectFiles);
            assert!(
                matches!(result, Err(TransferError::InvalidOffer(_))),
                "{result:?}"
            );
        }
    }

    pub async fn a_missing_file_fails_the_transfer(pair: PairFn) {
        let f = fixture();

        let o = run(
            pair().await,
            vec![f.src.join("nope.txt")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        assert!(
            matches!(&o.send, Err(TransferError::FileNotFound(p)) if p.ends_with("nope.txt")),
            "{:?}",
            o.send
        );
        assert!(o.recv.is_err(), "the receiver must not wait forever");
        assert!(o.offered.is_none());
    }

    pub async fn receiver_rejects_another_protocol_version(pair: PairFn) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;

        let peer = async {
            let mut control = frame(sender.control().await.unwrap());
            send_packet(
                &mut control,
                &SenderToReceiver::ConnRequest {
                    version: "9.9.9".into(),
                },
            )
            .await
            .unwrap();
            let reply: ReceiverToSender = recv_packet(&mut control).await.unwrap();
            drop(control);
            sender.close(false).await;
            reply
        };
        let (mut progress, mut offer, mut data) = (no_progress, no_offer, no_data);
        let receive = receive_files(
            receiver.as_mut(),
            ReceiveArgs { resume: false },
            &mut progress,
            &mut offer,
            &mut data,
        );

        let (reply, result) = tokio::join!(peer, receive);
        assert_eq!(
            reply,
            ReceiverToSender::WrongVersion {
                expected: PROTO_VERSION.into()
            }
        );
        assert!(
            matches!(&result, Err(TransferError::WrongVersion(expected, got)) if expected == PROTO_VERSION && got == "9.9.9"),
            "{result:?}"
        );
    }

    pub async fn sender_stops_when_the_receiver_wants_another_version(pair: PairFn) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;

        let peer = async {
            let mut control = frame(receiver.control().await.unwrap());
            let request: SenderToReceiver = recv_packet(&mut control).await.unwrap();
            assert_eq!(
                request,
                SenderToReceiver::ConnRequest {
                    version: PROTO_VERSION.into()
                }
            );
            send_packet(
                &mut control,
                &ReceiverToSender::WrongVersion {
                    expected: "9.9.9".into(),
                },
            )
            .await
            .unwrap();
            // Give the packet time to arrive before hanging up.
            let _ = tokio::time::timeout(Duration::from_secs(5), receiver.closed()).await;
            drop(control);
            receiver.close(false).await;
        };
        let (mut progress, mut waiting, mut data) = (no_progress, no_wait, no_data);
        let send = send_files(
            sender.as_mut(),
            SendArgs {
                files: vec!["unused".into()],
            },
            &mut progress,
            &mut waiting,
            &mut data,
        );

        let ((), result) = tokio::join!(peer, send);
        assert!(
            matches!(&result, Err(TransferError::WrongVersion(expected, got)) if expected == "9.9.9" && got == PROTO_VERSION),
            "{result:?}"
        );
    }

    pub async fn unexpected_packets_are_errors(pair: PairFn) {
        // A sender that skips the handshake.
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let peer = async {
            let mut control = frame(sender.control().await.unwrap());
            send_packet(&mut control, &SenderToReceiver::FileInfo { files: vec![] })
                .await
                .unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(5), sender.closed()).await;
            drop(control);
            sender.close(false).await;
        };
        let (mut progress, mut offer, mut data) = (no_progress, decline, no_data);
        let receive = receive_files(
            receiver.as_mut(),
            ReceiveArgs { resume: false },
            &mut progress,
            &mut offer,
            &mut data,
        );
        let ((), result) = tokio::join!(peer, receive);
        assert!(
            matches!(result, Err(TransferError::UnexpectedPacket)),
            "{result:?}"
        );

        // A receiver that answers the handshake with a decline.
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let peer = async {
            let mut control = frame(receiver.control().await.unwrap());
            let _: SenderToReceiver = recv_packet(&mut control).await.unwrap();
            send_packet(&mut control, &ReceiverToSender::RejectFiles)
                .await
                .unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(5), receiver.closed()).await;
            drop(control);
            receiver.close(false).await;
        };
        let (mut progress, mut waiting, mut data) = (no_progress, no_wait, no_data);
        let send = send_files(
            sender.as_mut(),
            SendArgs { files: vec![] },
            &mut progress,
            &mut waiting,
            &mut data,
        );
        let ((), result) = tokio::join!(peer, send);
        assert!(
            matches!(result, Err(TransferError::UnexpectedPacket)),
            "{result:?}"
        );
    }

    pub async fn a_skip_list_of_the_wrong_length_is_rejected(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("a.txt"), b"hello");

        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let peer = async {
            let mut control = frame(receiver.control().await.unwrap());
            let _: SenderToReceiver = recv_packet(&mut control).await.unwrap();
            send_packet(&mut control, &ReceiverToSender::Ok)
                .await
                .unwrap();
            let _: SenderToReceiver = recv_packet(&mut control).await.unwrap();
            send_packet(
                &mut control,
                &ReceiverToSender::AcceptFilesSkip {
                    files: vec![None, None],
                },
            )
            .await
            .unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(5), receiver.closed()).await;
            drop(control);
            receiver.close(false).await;
        };
        let (mut progress, mut waiting, mut data) = (no_progress, no_wait, no_data);
        let send = send_files(
            sender.as_mut(),
            SendArgs {
                files: vec![f.src.join("a.txt")],
            },
            &mut progress,
            &mut waiting,
            &mut data,
        );
        let ((), result) = tokio::join!(peer, send);
        assert!(
            matches!(result, Err(TransferError::UnexpectedPacket)),
            "{result:?}"
        );
    }

    pub async fn packets_survive_the_wire(pair: PairFn) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let big = SenderToReceiver::FileInfo {
            files: (0..5000)
                .map(|i| FilesAvailable::File {
                    name: format!("file-{i}.bin"),
                    size: i,
                })
                .collect(),
        };

        // The receiver's stream only exists once the sender has written.
        let mut a = frame(sender.control().await.unwrap());
        let (sent, (mut b, received)) = tokio::join!(send_packet(&mut a, &big), async {
            let mut b = frame(receiver.control().await.unwrap());
            let received = recv_packet::<SenderToReceiver>(&mut b).await;
            (b, received)
        });
        sent.unwrap();
        assert_eq!(received.unwrap(), big);

        // The other direction on the same stream.
        send_packet(&mut b, &ReceiverToSender::Ok).await.unwrap();
        let got: ReceiverToSender = recv_packet(&mut a).await.unwrap();
        assert_eq!(got, ReceiverToSender::Ok);

        // The wrong packet type does not decode.
        send_packet(&mut b, &ReceiverToSender::RejectFiles)
            .await
            .unwrap();
        let got = recv_packet::<SenderToReceiver>(&mut a).await;
        assert!(matches!(got, Err(TransferError::Json(_))), "{got:?}");

        drop((a, b));
        sender.close(true).await;
        receiver.close(true).await;
    }

    pub async fn a_closed_peer_fails_the_control_stream(pair: PairFn) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        sender.close(false).await;

        let result = match receiver.control().await {
            Err(e) => Err(e),
            Ok(stream) => recv_packet::<SenderToReceiver>(&mut frame(stream)).await,
        };
        assert!(result.is_err(), "{result:?}");
        receiver.close(false).await;
    }
}
