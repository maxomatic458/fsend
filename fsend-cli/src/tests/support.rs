//! Transport testing suite.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

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
    pub sender: Box<dyn Transfer>,
    pub receiver: Box<dyn Transfer>,
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
        if let Some(limit) = opts.abort_sender_after {
            if sent >= limit && !sender_aborted {
                sender_aborted = true;
                abort_sender();
            }
        }
    };
    let mut on_receiver_initial = |p: &[(String, u64, u64)]| receiver_initial = Some(p.to_vec());
    let mut on_offer = |files: &[FilesAvailable]| {
        offered = Some(files.to_vec());
        (!opts.decline).then(|| out_dir.clone())
    };
    let mut on_read = |n: u64| {
        received += n;
        if let Some(limit) = opts.abort_receiver_after {
            if received >= limit && !receiver_aborted {
                receiver_aborted = true;
                abort_receiver();
            }
        }
    };

    let send = sender.send_files(
        SendArgs { files },
        &mut on_sender_initial,
        &mut on_waiting,
        &mut on_write,
    );
    let recv = receiver.receive_files(
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
        let partial = std::fs::metadata(f.out.join("movie.bin")).unwrap().len();
        assert!(
            partial < data.len() as u64,
            "partial file of {partial} bytes"
        );
        assert_eq!(partial, o.received);
        assert_eq!(
            std::fs::read(f.out.join("movie.bin")).unwrap(),
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
}
