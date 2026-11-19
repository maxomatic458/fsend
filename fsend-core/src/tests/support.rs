use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use fsend_proto::peer::limits::RECV_WINDOW;
use fsend_proto::peer::{VERSION, hello};

use super::*;

/// Deterministic incompressible bytes.
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

/// Deterministic lorem ipsum in random order, which zstd shrinks a few times.
pub fn text(size: usize) -> Vec<u8> {
    let words: Vec<_> = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim ad minim veniam quis nostrud exercitation ullamco laboris nisi aliquip ex ea commodo consequat duis aute irure in reprehenderit voluptate velit esse cillum fugiat nulla pariatur excepteur sint occaecat cupidatat non proident sunt culpa qui officia deserunt mollit anim id est laborum"
        .split(' ')
        .collect();
    let mut state = 0x2545_F491u32;
    let mut out = Vec::with_capacity(size + 16);
    while out.len() < size {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.extend_from_slice(words[state as usize % words.len()].as_bytes());
        out.push(b' ');
    }
    out.truncate(size);
    out
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

pub fn no_event(_: Event) {}
pub fn no_offer(_: &[TopEntry]) -> Option<PathBuf> {
    panic!("must not get as far as the offer")
}
pub fn no_conflict(_: &[Entry], _: &[Conflict]) -> ConflictPolicy {
    panic!("must not get as far as conflicts")
}

/// A `hello` for a peer that speaks exactly `version`.
pub fn hello_for(version: u32, recv_window: Option<u64>) -> Hello {
    Hello {
        version,
        min_version: version,
        features: vec![],
        recv_window,
    }
}

/// A frame of `json`, valid or not.
pub fn json_frame(json: &str) -> Vec<u8> {
    let mut frame = (json.len() as u32 + 1).to_be_bytes().to_vec();
    frame.push(0);
    frame.extend_from_slice(json.as_bytes());
    frame
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
    /// Replace conflicting files instead of skipping them.
    pub replace: bool,
    pub no_compression: bool,
    pub decline: bool,
    /// Abort the sender once it has handed this many bytes to the transport.
    pub abort_sender_after: Option<u64>,
    /// Abort the receiver once it has written this many bytes.
    pub abort_receiver_after: Option<u64>,
    /// Cancel the sender once it has handed this many bytes to the transport.
    pub cancel_sender_after: Option<u64>,
    /// Cancel the receiver once it has written this many bytes.
    pub cancel_receiver_after: Option<u64>,
}

/// What one side reported through its events.
#[derive(Debug, Default)]
pub struct Tally {
    /// `(name, bytes present, size)` per top-level entry once accepted.
    pub initial: Option<Vec<(String, u64, u64)>>,
    /// Bytes present per top-level entry by the end.
    pub present: Vec<u64>,
    /// Bytes moved this session.
    pub moved: u64,
    /// The largest progress step.
    pub largest_step: u64,
    pub offered: bool,
}

impl Tally {
    fn record(&mut self, event: Event) {
        match event {
            Event::Offered => self.offered = true,
            Event::Started { entries, present } => {
                self.initial = Some(
                    entries
                        .iter()
                        .zip(present)
                        .map(|(entry, present)| (entry.name.clone(), *present, entry.size))
                        .collect(),
                );
                self.present = present.to_vec();
            }
            Event::Progress { entry, bytes } => {
                self.present[entry] += bytes;
                self.moved += bytes;
                self.largest_step = self.largest_step.max(bytes);
            }
        }
    }
}

/// Everything observable about one send/receive run.
pub struct Outcome {
    pub send: Result<(), TransferError>,
    pub recv: Result<(), TransferError>,
    pub sender: Tally,
    pub receiver: Tally,
    pub offered: Option<Vec<TopEntry>>,
    /// What the receiver was asked about, if anything.
    pub conflicts: Option<Vec<Conflict>>,
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
    let outgoing = build_offer(&files).unwrap();
    run_offer(peers, &outgoing, out, opts).await
}

pub async fn run_offer(peers: Peers, outgoing: &Outgoing, out: &Path, opts: RunOptions) -> Outcome {
    let Peers {
        mut sender,
        mut receiver,
        abort_sender,
        abort_receiver,
    } = peers;

    let mut sending = Tally::default();
    let mut receiving = Tally::default();
    let mut offered = None;
    let mut conflicts = None;
    let out_dir = out.to_path_buf();

    let (mut sender_aborted, mut receiver_aborted) = (false, false);
    let (sender_cancel, receiver_cancel) = (CancellationToken::new(), CancellationToken::new());
    let reached = |limit: Option<u64>, moved: u64| limit.is_some_and(|n| moved >= n);

    let mut on_send = |event: Event| {
        sending.record(event);
        if !sender_aborted && reached(opts.abort_sender_after, sending.moved) {
            sender_aborted = true;
            abort_sender();
        }
        if reached(opts.cancel_sender_after, sending.moved) {
            sender_cancel.cancel();
        }
    };
    let on_offer = |entries: &[TopEntry]| {
        offered = Some(entries.to_vec());
        (!opts.decline).then(|| out_dir.clone())
    };
    let on_conflict = |_: &[Entry], found: &[Conflict]| {
        conflicts = Some(found.to_vec());
        match opts.replace {
            true => ConflictPolicy::Replace,
            false => ConflictPolicy::Skip,
        }
    };
    let mut on_receive = |event: Event| {
        receiving.record(event);
        if !receiver_aborted
            && opts
                .abort_receiver_after
                .is_some_and(|n| receiving.moved >= n)
        {
            receiver_aborted = true;
            abort_receiver();
        }
        if reached(opts.cancel_receiver_after, receiving.moved) {
            receiver_cancel.cancel();
        }
    };

    let mut decide = Answer {
        accept: on_offer,
        resolve: on_conflict,
    };
    let send = send_files(
        sender.as_mut(),
        outgoing,
        !opts.no_compression,
        &mut on_send,
        sender_cancel.clone(),
    );
    let recv = receive_files(
        receiver.as_mut(),
        &mut decide,
        &mut on_receive,
        receiver_cancel.clone(),
    );
    let (send, recv) =
        tokio::time::timeout(Duration::from_secs(120), async { tokio::join!(send, recv) })
            .await
            .expect("the transfer hung");

    Outcome {
        send,
        recv,
        sender: sending,
        receiver: receiving,
        offered,
        conflicts,
    }
}

/// What the peer says on `control` until it hangs up or says its last word.
async fn listen<Out: Message + Debug, In: PeerMessage>(
    control: &mut Control<Out, In>,
    mut got: Vec<In>,
) -> Vec<In> {
    while let Ok(Ok(message)) = tokio::time::timeout(Duration::from_secs(10), control.recv()).await
    {
        let last = message.is_final();
        got.push(message);
        if last {
            break;
        }
    }
    got
}

/// Plays a receiver that answers the sender's hello with `raw` bytes; returns what the sender said.
async fn fake_receiver(receiver: &mut Box<dyn Transport>, raw: &[u8]) -> Vec<SenderMessage> {
    let mut control = ReceiverControl::new(receiver.control().await.unwrap());
    let hello = control.recv().await.unwrap();
    control.send_frame(raw).await.unwrap();
    let got = listen(&mut control, vec![hello]).await;
    drop(control);
    receiver.close(false).await;
    got
}

/// Plays a sender that opens with `raw` bytes on the control stream; returns what the receiver said.
async fn fake_sender(sender: &mut Box<dyn Transport>, raw: &[u8]) -> Vec<ReceiverMessage> {
    let mut control = SenderControl::new(sender.control().await.unwrap());
    control.send_frame(raw).await.unwrap();
    let got = listen(&mut control, vec![]).await;
    drop(control);
    sender.close(false).await;
    got
}

/// Frames of `messages`, back to back.
fn frames(messages: &[impl Message]) -> Vec<u8> {
    messages
        .iter()
        .flat_map(|message| frame::encode(message).unwrap())
        .collect()
}

/// The tests every transport must pass, for a module with a `pair` [`PairFn`].
macro_rules! transport_suite {
    () => {
        $crate::transfer::test_support::transport_suite!(
            sends_a_single_file,
            sends_an_empty_file,
            sends_a_file_larger_than_the_buffers,
            sends_a_directory_tree,
            sends_several_entries_at_once,
            sends_compressible_data_either_way,
            progress_moves_in_small_steps,
            sends_the_current_directory,
            receiver_can_decline,
            resumes_a_partial_file,
            resumes_a_partial_directory,
            resume_skips_complete_files,
            conflicts_are_skipped,
            conflicts_are_replaced,
            a_change_near_the_end_is_a_conflict,
            entries_of_another_kind_are_skipped,
            interrupted_by_the_receiver_then_resumed,
            interrupted_by_the_sender_then_resumed,
            cancelled_by_the_receiver_then_resumed,
            cancelled_by_the_sender_then_resumed,
            cancelled_while_deciding,
            a_file_that_vanishes_fails_both_sides,
            receiver_rejects_another_protocol_version,
            sender_stops_when_the_receiver_wants_another_version,
            a_receiver_without_a_usable_window_is_rejected,
            unexpected_messages_are_errors,
            unknown_and_wrong_direction_messages_are_errors,
            malformed_frames_are_errors,
            an_invalid_offer_is_rejected,
            an_invalid_accept_is_rejected,
            an_invalid_check_is_rejected,
            a_second_check_is_rejected,
            hashes_must_match_the_check,
            an_ack_beyond_the_data_is_rejected,
            a_cancel_from_the_peer_ends_the_transfer,
            messages_survive_the_wire,
            a_closed_peer_fails_the_control_stream,
        );
    };
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                $crate::transfer::test_support::suite::$name(pair).await
            }
        )*
    };
}
pub(crate) use transport_suite;

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

    fn progress(name: &str, present: u64, size: u64) -> Vec<(String, u64, u64)> {
        vec![(name.to_string(), present, size)]
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
        assert_eq!(o.sender.initial.unwrap(), progress("photo.jpg", 0, 100_000));
        assert_eq!(
            o.receiver.initial.unwrap(),
            progress("photo.jpg", 0, 100_000)
        );
        assert_eq!(
            o.offered.unwrap(),
            vec![TopEntry {
                name: "photo.jpg".into(),
                dir: false,
                size: 100_000
            }]
        );
        assert_eq!(o.sender.moved, 100_000);
        assert_eq!(o.receiver.moved, 100_000);
        assert!(
            o.sender.offered,
            "the sender must report waiting for the accept"
        );
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
        assert_eq!(o.sender.moved, 0);
        assert_eq!(o.receiver.moved, 0);
    }

    pub async fn sends_a_file_larger_than_the_buffers(pair: PairFn) {
        let f = fixture();
        // Not a multiple of any buffer or chunk size, and past several acks.
        let data = bytes(20 * 1024 * 1024 + 17, 5);
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
        assert_eq!(o.sender.moved, data.len() as u64);
        assert_eq!(o.receiver.moved, data.len() as u64);
    }

    pub async fn sends_a_directory_tree(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);

        let o = run(pair().await, vec![album], &f.out, RunOptions::default()).await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender.initial.unwrap(), progress("album", 0, 80_003));
        assert_eq!(o.sender.moved, 80_003);
        assert_eq!(o.receiver.moved, 80_003);
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
            o.sender.initial.unwrap(),
            vec![
                ("first.txt".to_string(), 0, 500),
                ("album".to_string(), 0, 80_003),
                ("last.txt".to_string(), 0, 0),
            ]
        );
        // Progress lands on the entry it belongs to.
        assert_eq!(o.sender.present, [500, 80_003, 0]);
        assert_eq!(o.receiver.present, [500, 80_003, 0]);
        assert_eq!(o.sender.moved, 80_503);
    }

    pub async fn sends_the_current_directory(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);

        // `fsend send .` hands over a path without a file name of its own.
        let o = run(
            pair().await,
            vec![album.join("nested/..")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender.initial.unwrap(), progress("album", 0, 80_003));
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
            matches!(o.send, Err(TransferError::PeerDeclined(None))),
            "{:?}",
            o.send
        );
        assert!(o.offered.is_some(), "the receiver saw the offer");
        assert!(o.sender.initial.is_none() && o.receiver.initial.is_none());
        assert_eq!(o.sender.moved, 0);
        assert!(!f.out.join("secret.txt").exists());
    }

    pub async fn sends_compressible_data_either_way(pair: PairFn) {
        for no_compression in [false, true] {
            let f = fixture();
            // Blocks that span files, and one file of several blocks.
            let text = b"all work and no play makes jack a dull boy\n".repeat(60_000);
            write(&f.src.join("d/big.txt"), &text);
            for i in 0..50 {
                write(&f.src.join(format!("d/small-{i}.txt")), &text[..i * 997]);
            }
            write(&f.src.join("d/noise.bin"), &bytes(300_000, 7));

            let opts = RunOptions {
                no_compression,
                ..Default::default()
            };
            let o = run(pair().await, vec![f.src.join("d")], &f.out, opts).await;

            o.unwrap();
            assert_eq!(snapshot(&f.out), snapshot(&f.src));
            let total = o.sender.initial.unwrap()[0].2;
            assert_eq!(o.sender.moved, total);
            assert_eq!(o.receiver.moved, total);
        }
    }

    pub async fn progress_moves_in_small_steps(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("big.bin"), &bytes(3 * MAX_BLOCK_BYTES, 8));

        let o = run(
            pair().await,
            vec![f.src.join("big.bin")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        // One write or read of the data stream at most, not a block.
        for side in [&o.sender, &o.receiver] {
            assert!(
                side.largest_step <= BUF_SIZE as u64,
                "{}",
                side.largest_step
            );
            assert_eq!(side.moved, 3 * MAX_BLOCK_BYTES as u64);
        }
    }

    /// Sending compressible data must not be slower than sending incompressible data.
    pub async fn compression_is_no_bottleneck(pair: PairFn) {
        const SIZE: usize = 256 << 20;
        let mut secs = Vec::new();
        for (name, data) in [("noise.bin", bytes(SIZE, 9)), ("text.txt", text(SIZE))] {
            let f = fixture();
            write(&f.src.join(name), &data);
            drop(data);
            let start = std::time::Instant::now();
            let o = run(
                pair().await,
                vec![f.src.join(name)],
                &f.out,
                RunOptions::default(),
            )
            .await;
            o.unwrap();
            let took = start.elapsed().as_secs_f64();
            println!("{name}: {:.0} MiB/s", SIZE as f64 / (1 << 20) as f64 / took);
            secs.push(took);
        }
        assert!(
            secs[1] <= secs[0] * 1.1,
            "text took {:.2} s, noise {:.2} s",
            secs[1],
            secs[0]
        );
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
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("big.bin")).unwrap(), data);
        assert_eq!(
            o.sender.initial.unwrap(),
            progress("big.bin", 80_000, 200_000)
        );
        assert_eq!(
            o.receiver.initial.unwrap(),
            progress("big.bin", 80_000, 200_000)
        );
        assert_eq!(o.sender.moved, 120_000);
        assert_eq!(o.receiver.moved, 120_000);
    }

    pub async fn resumes_a_partial_directory(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);
        // a.txt half there, b.txt complete, c.txt and empty.bin missing.
        write(&f.out.join("album/a.txt"), &bytes(10_000, 1)[..4_000]);
        write(&f.out.join("album/nested/b.txt"), &bytes(70_000, 2));

        let o = run(pair().await, vec![album], &f.out, RunOptions::default()).await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender.initial.unwrap(), progress("album", 74_000, 80_003));
        assert_eq!(o.sender.moved, 6_003);
        assert_eq!(o.receiver.moved, 6_003);
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
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("done.bin")).unwrap(), data);
        assert_eq!(
            o.sender.initial.unwrap(),
            progress("done.bin", 50_000, 50_000)
        );
        assert_eq!(o.sender.moved, 0);
        assert_eq!(o.receiver.moved, 0);
    }

    /// A file that differs from the start, and one larger than the offer.
    fn conflicting(f: &Fixture) -> (Vec<PathBuf>, Vec<u8>, Vec<u8>) {
        write(&f.src.join("a.bin"), &bytes(200_000, 3));
        write(&f.src.join("b.bin"), &bytes(200, 1));
        let (a, b) = (bytes(80_000, 9), bytes(300, 9));
        write(&f.out.join("a.bin"), &a);
        write(&f.out.join("b.bin"), &b);
        (vec![f.src.join("a.bin"), f.src.join("b.bin")], a, b)
    }

    fn replaceable(ids: &[u32]) -> Vec<Conflict> {
        ids.iter()
            .map(|&id| Conflict {
                id,
                replaceable: true,
            })
            .collect()
    }

    pub async fn conflicts_are_skipped(pair: PairFn) {
        let f = fixture();
        let (files, a, b) = conflicting(&f);

        let o = run(pair().await, files, &f.out, RunOptions::default()).await;

        o.unwrap();
        assert_eq!(o.conflicts, Some(replaceable(&[0, 1])));
        assert_eq!(std::fs::read(f.out.join("a.bin")).unwrap(), a);
        assert_eq!(std::fs::read(f.out.join("b.bin")).unwrap(), b);
        assert_eq!(o.sender.moved, 0);
    }

    pub async fn conflicts_are_replaced(pair: PairFn) {
        let f = fixture();
        let (files, _, _) = conflicting(&f);
        let replace = RunOptions {
            replace: true,
            ..Default::default()
        };

        let o = run(pair().await, files, &f.out, replace).await;

        o.unwrap();
        assert_eq!(o.conflicts, Some(replaceable(&[0, 1])));
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender.initial.unwrap()[0], ("a.bin".into(), 0, 200_000));
        assert_eq!(o.sender.moved, 200_200);
    }

    pub async fn a_change_near_the_end_is_a_conflict(pair: PairFn) {
        let f = fixture();
        let data = bytes(INTERRUPT_SIZE, 5);
        write(&f.src.join("f.bin"), &data);
        // Past the first 2 MiB, so only the end is sampled.
        let mut local = data[..INTERRUPT_SIZE - 100_000].to_vec();
        *local.last_mut().unwrap() ^= 1;
        write(&f.out.join("f.bin"), &local);

        let o = run(
            pair().await,
            vec![f.src.join("f.bin")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(o.conflicts, Some(replaceable(&[0])));
        assert_eq!(std::fs::read(f.out.join("f.bin")).unwrap(), local);
    }

    pub async fn entries_of_another_kind_are_skipped(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);
        write(&f.src.join("f.bin"), &bytes(10, 1));
        write(&f.src.join("g.bin"), &bytes(10, 2));
        write(&f.out.join("album"), b"a file");
        std::fs::create_dir_all(f.out.join("f.bin")).unwrap();
        let replace = RunOptions {
            replace: true,
            ..Default::default()
        };

        let files = vec![album, f.src.join("f.bin"), f.src.join("g.bin")];
        let o = run(pair().await, files, &f.out, replace).await;

        o.unwrap();
        let entries = o.conflicts.unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|c| !c.replaceable), "{entries:?}");
        assert_eq!(std::fs::read(f.out.join("album")).unwrap(), b"a file");
        assert!(f.out.join("f.bin").is_dir());
        assert_eq!(std::fs::read(f.out.join("g.bin")).unwrap(), bytes(10, 2));
        assert_eq!(o.sender.moved, 10);
    }

    /// Progress counts blocks as they arrive, so it runs ahead of what is
    /// written by the rest of the block being written and the next one.
    fn arrived_ahead_of_disk(moved: u64, written: u64) {
        assert!(
            (written..written + 2 * MAX_BLOCK_BYTES as u64).contains(&moved),
            "{moved} bytes counted, {written} written"
        );
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
        arrived_ahead_of_disk(o.receiver.moved, partial);
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
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(
            o.sender.initial.unwrap(),
            progress("movie.bin", partial, data.len() as u64)
        );
        assert_eq!(o.sender.moved, data.len() as u64 - partial);
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
        arrived_ahead_of_disk(o.receiver.moved, partial);
        assert_eq!(
            std::fs::read(f.out.join("movie.bin")).unwrap_or_default(),
            &data[..partial as usize]
        );

        let o = run(
            pair().await,
            vec![f.src.join("movie.bin")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(
            o.sender.initial.unwrap(),
            progress("movie.bin", partial, data.len() as u64)
        );
        assert_eq!(o.sender.moved, data.len() as u64 - partial);
        assert_eq!(std::fs::read(f.out.join("movie.bin")).unwrap(), data);
    }

    /// Stops a transfer of a large file with `opts`, checks what is left is a
    /// prefix and that a fresh session finishes it; returns the stopped run.
    async fn stopped_then_resumed(pair: PairFn, opts: RunOptions) -> Outcome {
        let f = fixture();
        let data = bytes(INTERRUPT_SIZE, 6);
        write(&f.src.join("movie.bin"), &data);

        let stopped = run(pair().await, vec![f.src.join("movie.bin")], &f.out, opts).await;

        let partial = std::fs::metadata(f.out.join("movie.bin"))
            .map(|m| m.len())
            .unwrap_or(0);
        assert!(
            partial < data.len() as u64,
            "partial file of {partial} bytes"
        );
        assert_eq!(
            std::fs::read(f.out.join("movie.bin")).unwrap_or_default(),
            &data[..partial as usize]
        );

        let o = run(
            pair().await,
            vec![f.src.join("movie.bin")],
            &f.out,
            RunOptions::default(),
        )
        .await;

        o.unwrap();
        assert_eq!(o.sender.moved, data.len() as u64 - partial);
        assert_eq!(std::fs::read(f.out.join("movie.bin")).unwrap(), data);
        stopped
    }

    pub async fn cancelled_by_the_receiver_then_resumed(pair: PairFn) {
        let opts = RunOptions {
            cancel_receiver_after: Some(INTERRUPT_AFTER),
            ..Default::default()
        };
        let o = stopped_then_resumed(pair, opts).await;
        assert!(
            matches!(o.recv, Err(TransferError::Cancelled)),
            "{:?}",
            o.recv
        );
        assert!(
            matches!(o.send, Err(TransferError::PeerCancelled(None))),
            "{:?}",
            o.send
        );
    }

    pub async fn cancelled_by_the_sender_then_resumed(pair: PairFn) {
        let opts = RunOptions {
            cancel_sender_after: Some(INTERRUPT_AFTER),
            ..Default::default()
        };
        let o = stopped_then_resumed(pair, opts).await;
        assert!(
            matches!(o.send, Err(TransferError::Cancelled)),
            "{:?}",
            o.send
        );
        assert!(
            matches!(o.recv, Err(TransferError::PeerCancelled(None))),
            "{:?}",
            o.recv
        );
    }

    /// A receiver that cancels while its user is still looking at the offer.
    struct CancelWhileDeciding(CancellationToken);

    #[async_trait]
    impl Decide for CancelWhileDeciding {
        async fn accept(&mut self, _: &[TopEntry]) -> Option<PathBuf> {
            self.0.cancel();
            std::future::pending().await
        }

        async fn resolve(&mut self, _: &[Entry], _: &[Conflict]) -> ConflictPolicy {
            unreachable!("the offer is never accepted")
        }
    }

    pub async fn cancelled_while_deciding(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("a.txt"), b"hello");
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let outgoing = build_offer(&[f.src.join("a.txt")]).unwrap();
        let cancel = CancellationToken::new();
        let mut decide = CancelWhileDeciding(cancel.clone());
        let (mut on_send, mut on_receive) = (no_event, no_event);

        let send = send_files(
            sender.as_mut(),
            &outgoing,
            true,
            &mut on_send,
            CancellationToken::new(),
        );
        let recv = receive_files(receiver.as_mut(), &mut decide, &mut on_receive, cancel);
        let (send, recv) =
            tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(send, recv) })
                .await
                .expect("the transfer hung");

        assert!(matches!(recv, Err(TransferError::Cancelled)), "{recv:?}");
        assert!(
            matches!(send, Err(TransferError::PeerCancelled(None))),
            "{send:?}"
        );
        assert!(!f.out.join("a.txt").exists());
    }

    pub async fn a_file_that_vanishes_fails_both_sides(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("gone.txt"), &bytes(100, 1));
        let outgoing = build_offer(&[f.src.join("gone.txt")]).unwrap();
        std::fs::remove_file(f.src.join("gone.txt")).unwrap();

        let o = run_offer(pair().await, &outgoing, &f.out, RunOptions::default()).await;

        assert!(matches!(o.send, Err(TransferError::Io(_))), "{:?}", o.send);
        assert!(
            matches!(
                &o.recv,
                Err(TransferError::Peer {
                    code: ErrorCode::Io,
                    ..
                })
            ),
            "the receiver learns why: {:?}",
            o.recv
        );
    }

    pub async fn receiver_rejects_another_protocol_version(pair: PairFn) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;

        let raw = frames(&[SenderMessage::Hello(hello_for(9, None))]);
        let peer = fake_sender(&mut sender, &raw);
        let mut decide = Answer {
            accept: no_offer,
            resolve: no_conflict,
        };
        let mut event = no_event;
        let receive = receive_files(
            receiver.as_mut(),
            &mut decide,
            &mut event,
            CancellationToken::new(),
        );

        let (got, result) = tokio::join!(peer, receive);
        assert_eq!(got[0], ReceiverMessage::Hello(hello(Some(RECV_WINDOW))));
        assert!(is_error(&got[1], ErrorCode::Version), "{got:?}");
        assert!(
            matches!(
                &result,
                Err(TransferError::Version {
                    version: 9,
                    min_version: 9
                })
            ),
            "{result:?}"
        );
    }

    /// A sender of `a.txt` against a receiver that answers with `raw`.
    async fn hostile_receiver(
        pair: PairFn,
        raw: &[u8],
    ) -> (Vec<SenderMessage>, Result<(), TransferError>) {
        let f = fixture();
        write(&f.src.join("a.txt"), b"hello");
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let outgoing = build_offer(&[f.src.join("a.txt")]).unwrap();
        let mut event = no_event;
        let send = send_files(
            sender.as_mut(),
            &outgoing,
            true,
            &mut event,
            CancellationToken::new(),
        );
        tokio::join!(fake_receiver(&mut receiver, raw), send)
    }

    /// A receiver against a sender that opens with `raw` on the control stream.
    async fn hostile_sender(
        pair: PairFn,
        raw: &[u8],
    ) -> (Vec<ReceiverMessage>, Result<(), TransferError>) {
        hostile_sender_into(pair, raw, no_offer).await
    }

    async fn hostile_sender_into(
        pair: PairFn,
        raw: &[u8],
        accept: impl FnMut(&[TopEntry]) -> Option<PathBuf> + Send,
    ) -> (Vec<ReceiverMessage>, Result<(), TransferError>) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let mut decide = Answer {
            accept,
            resolve: no_conflict,
        };
        let mut event = no_event;
        let receive = receive_files(
            receiver.as_mut(),
            &mut decide,
            &mut event,
            CancellationToken::new(),
        );
        tokio::join!(fake_sender(&mut sender, raw), receive)
    }

    /// Whether `message` is an `error` with the `expected` code.
    fn is_error(message: &(impl PeerMessage + Clone), expected: ErrorCode) -> bool {
        matches!(message.clone().unexpected(), TransferError::Peer { code, .. } if code == expected)
    }

    fn ok_hello() -> Hello {
        hello_for(VERSION, Some(RECV_WINDOW))
    }

    /// The receiver's frames for a `hello` and then `messages`.
    fn receiver_frames(messages: &[ReceiverMessage]) -> Vec<u8> {
        [
            frames(&[ReceiverMessage::Hello(ok_hello())]),
            frames(messages),
        ]
        .concat()
    }

    pub async fn sender_stops_when_the_receiver_wants_another_version(pair: PairFn) {
        let raw = frames(&[ReceiverMessage::Hello(hello_for(9, Some(RECV_WINDOW)))]);
        let (got, result) = hostile_receiver(pair, &raw).await;
        assert_eq!(got[0], SenderMessage::Hello(hello(None)));
        assert!(is_error(&got[1], ErrorCode::Version), "{got:?}");
        assert!(
            matches!(&result, Err(TransferError::Version { version: 9, .. })),
            "{result:?}"
        );
    }

    pub async fn a_receiver_without_a_usable_window_is_rejected(pair: PairFn) {
        let raw = frames(&[ReceiverMessage::Hello(hello_for(VERSION, Some(1024)))]);
        let (got, result) = hostile_receiver(pair, &raw).await;
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
    }

    pub async fn unexpected_messages_are_errors(pair: PairFn) {
        // A sender that skips hello.
        let (got, result) = hostile_sender(pair, &frames(&[an_offer()])).await;
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );

        // A receiver that answers hello with done.
        let (got, result) = hostile_receiver(pair, &frames(&[ReceiverMessage::Done])).await;
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
    }

    fn an_offer() -> SenderMessage {
        SenderMessage::Offer {
            entries: vec![Entry::File {
                path: vec!["a".into()],
                size: 1,
            }],
        }
    }

    /// The sender gets `from_receiver` after the receiver's hello, the receiver
    /// `from_sender` after the sender's; both must fail with a protocol error.
    async fn both_fail_with_a_protocol_error(
        pair: PairFn,
        from_receiver: &[u8],
        from_sender: &[u8],
    ) {
        let raw = [receiver_frames(&[]), from_receiver.to_vec()].concat();
        let (got, result) = hostile_receiver(pair, &raw).await;
        // After its hello and offer.
        assert!(is_error(&got[2], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );

        let raw = [
            frames(&[SenderMessage::Hello(ok_hello())]),
            from_sender.to_vec(),
        ]
        .concat();
        let (got, result) = hostile_sender(pair, &raw).await;
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
    }

    pub async fn unknown_and_wrong_direction_messages_are_errors(pair: PairFn) {
        // Each side sends the other's message.
        let accept = ReceiverMessage::Accept { files: vec![] };
        both_fail_with_a_protocol_error(pair, &frames(&[an_offer()]), &frames(&[accept])).await;
        both_fail_with_a_protocol_error(
            pair,
            &frames(&[SenderMessage::Hashes { hashes: vec![] }]),
            &frames(&[ReceiverMessage::Check { files: vec![] }]),
        )
        .await;
        let unknown = json_frame(r#"{"type":"resume","bytes":1}"#);
        both_fail_with_a_protocol_error(pair, &unknown, &unknown).await;
    }

    pub async fn malformed_frames_are_errors(pair: PairFn) {
        let (got, result) = hostile_sender(pair, &[0, 0, 0, 0]).await;
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );

        // Messages without their fields.
        both_fail_with_a_protocol_error(
            pair,
            &json_frame(r#"{"type":"accept"}"#),
            &json_frame(r#"{"type":"offer"}"#),
        )
        .await;
    }

    pub async fn an_invalid_offer_is_rejected(pair: PairFn) {
        let offer = SenderMessage::Offer {
            entries: vec![Entry::File {
                path: vec!["..".into()],
                size: 1,
            }],
        };
        let raw = frames(&[SenderMessage::Hello(ok_hello()), offer]);
        let (got, result) = hostile_sender(pair, &raw).await;
        assert!(is_error(&got[1], ErrorCode::InvalidOffer), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::InvalidOffer(_))),
            "{result:?}"
        );
    }

    pub async fn an_invalid_accept_is_rejected(pair: PairFn) {
        let accept = ReceiverMessage::Accept {
            files: vec![AcceptedFile { id: 7, offset: 0 }],
        };
        let (got, result) = hostile_receiver(pair, &receiver_frames(&[accept])).await;
        assert!(matches!(&got[1], SenderMessage::Offer { .. }), "{got:?}");
        assert!(is_error(&got[2], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
    }

    pub async fn an_invalid_check_is_rejected(pair: PairFn) {
        let check = ReceiverMessage::Check {
            files: vec![CheckedFile { id: 0, len: 6 }],
        };
        let (got, result) = hostile_receiver(pair, &receiver_frames(&[check])).await;
        assert!(is_error(&got[2], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
    }

    pub async fn a_second_check_is_rejected(pair: PairFn) {
        let check = ReceiverMessage::Check {
            files: vec![CheckedFile { id: 0, len: 5 }],
        };
        let (got, result) = hostile_receiver(pair, &receiver_frames(&[check.clone(), check])).await;
        assert_eq!(
            got[2],
            SenderMessage::Hashes {
                hashes: vec![Fingerprint::of(b"hello")]
            }
        );
        assert!(is_error(&got[3], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
    }

    pub async fn hashes_must_match_the_check(pair: PairFn) {
        let f = fixture();
        write(&f.out.join("a"), b"x");
        let offer = SenderMessage::Offer {
            entries: vec![Entry::File {
                path: vec!["a".into()],
                size: 5,
            }],
        };
        let hashes = SenderMessage::Hashes { hashes: vec![] };
        let raw = frames(&[SenderMessage::Hello(ok_hello()), offer, hashes]);
        let accept = |_: &[TopEntry]| Some(f.out.clone());
        let (got, result) = hostile_sender_into(pair, &raw, accept).await;
        assert_eq!(
            got[1],
            ReceiverMessage::Check {
                files: vec![CheckedFile { id: 0, len: 1 }]
            }
        );
        assert!(is_error(&got[2], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
        assert_eq!(std::fs::read(f.out.join("a")).unwrap(), b"x");
    }

    pub async fn an_ack_beyond_the_data_is_rejected(pair: PairFn) {
        let accept = ReceiverMessage::Accept {
            files: vec![AcceptedFile { id: 0, offset: 0 }],
        };
        let ack = ReceiverMessage::Ack { bytes: 1000 };
        let (got, result) = hostile_receiver(pair, &receiver_frames(&[accept, ack])).await;
        assert!(is_error(&got[2], ErrorCode::Protocol), "{got:?}");
        assert!(
            matches!(result, Err(TransferError::Protocol(_))),
            "{result:?}"
        );
    }

    pub async fn a_cancel_from_the_peer_ends_the_transfer(pair: PairFn) {
        let cancel = ReceiverMessage::Cancel(Cancel {
            reason: Some("changed my mind".into()),
        });
        let (got, result) = hostile_receiver(pair, &receiver_frames(&[cancel])).await;
        // Closing may cut off the offer; nothing else follows a cancel.
        assert!(got.len() <= 2, "{got:?}");
        assert!(
            matches!(&result, Err(TransferError::PeerCancelled(Some(r))) if r == "changed my mind"),
            "{result:?}"
        );
    }

    pub async fn messages_survive_the_wire(pair: PairFn) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let big = SenderMessage::Offer {
            entries: (0..5000)
                .map(|i| Entry::File {
                    path: vec![format!("file-{i}.bin")],
                    size: i,
                })
                .collect(),
        };

        // The receiver's stream only exists once the sender has written.
        let mut a = SenderControl::new(sender.control().await.unwrap());
        let (sent, mut b) = tokio::join!(a.send(&big), async {
            ReceiverControl::new(receiver.control().await.unwrap())
        });
        sent.unwrap();
        assert_eq!(b.recv().await.unwrap(), big);

        // The other direction on the same stream.
        b.send(&ReceiverMessage::Done).await.unwrap();
        assert_eq!(a.recv().await.unwrap(), ReceiverMessage::Done);

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
            Ok(stream) => ReceiverControl::new(stream).recv().await,
        };
        assert!(result.is_err(), "{result:?}");
        receiver.close(false).await;
    }
}
