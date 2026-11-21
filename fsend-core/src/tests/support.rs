use std::collections::BTreeMap;
use std::pin::Pin;

use fsend_proto::peer::resume::Conflict;

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

/// Deterministic text that zstd shrinks a few times.
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

/// 80_003 bytes in `album`.
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

/// A frame of `json`, valid or not.
pub fn json_frame(json: &str) -> Vec<u8> {
    let mut frame = (json.len() as u32 + 1).to_be_bytes().to_vec();
    frame.push(0);
    frame.extend_from_slice(json.as_bytes());
    frame
}

pub const APP: &str = "fsend-core tests";

pub fn ws_url(listener: &tokio::net::TcpListener) -> url::Url {
    url::Url::parse(&format!("ws://{}", listener.local_addr().unwrap())).unwrap()
}

pub async fn start_relay() -> url::Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = ws_url(&listener);
    tokio::spawn(fsend_relay::serve(listener, fsend_relay::Config::default()));
    url
}

/// Kills the connection from the outside.
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
    pub replace: bool,
    pub no_compression: bool,
    pub checksums: Option<Algorithm>,
    /// Abort or cancel a side once this many bytes moved.
    pub abort_sender_after: Option<u64>,
    pub abort_receiver_after: Option<u64>,
    pub cancel_sender_after: Option<u64>,
    pub cancel_receiver_after: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Tally {
    /// Bytes already there once started.
    pub present: Option<u64>,
    /// Bytes moved this session, by file id.
    pub files: BTreeMap<u32, u64>,
    pub moved: u64,
    pub largest_step: u64,
    /// Receiver: the last `(done, total)` of verifying.
    pub verifying: Option<(u64, u64)>,
}

/// Records a side's events, and stops it after a limit.
struct Side<'a> {
    entries: &'a [Entry],
    tally: Tally,
    abort: Option<(u64, &'a AbortFn)>,
    cancel: Option<(u64, CancellationToken)>,
    replace: bool,
    conflicts: Option<Vec<Conflict>>,
}

impl<'a> Side<'a> {
    fn new(entries: &'a [Entry], abort: Option<(u64, &'a AbortFn)>, cancel: Option<u64>) -> Self {
        Self {
            entries,
            tally: Tally::default(),
            abort,
            cancel: cancel.map(|after| (after, CancellationToken::new())),
            replace: false,
            conflicts: None,
        }
    }

    fn token(&self) -> CancellationToken {
        (self.cancel.as_ref())
            .map(|(_, token)| token.clone())
            .unwrap_or_default()
    }

    fn record(&mut self, event: Event) {
        let tally = &mut self.tally;
        match event {
            Event::Verifying { done, total } => tally.verifying = Some((done, total)),
            Event::Started(files) => {
                let total: u64 = self.entries.iter().map(Entry::size).sum();
                tally.present = Some(total - remaining(self.entries, files));
            }
            Event::Progress { id, bytes } => {
                *tally.files.entry(id).or_default() += bytes;
                tally.moved += bytes;
                tally.largest_step = tally.largest_step.max(bytes);
            }
            _ => {}
        }
        if let Some((after, abort)) = self.abort
            && tally.moved >= after
        {
            self.abort = None;
            abort();
        }
        if let Some((after, token)) = &self.cancel
            && tally.moved >= *after
        {
            token.cancel();
        }
    }
}

#[async_trait]
impl ReceiveHandler for Side<'_> {
    async fn accept(&mut self, _: &Offer) -> bool {
        true
    }

    async fn resolve(&mut self, _: &[Entry], conflicts: &[Conflict]) -> ConflictPolicy {
        self.conflicts = Some(conflicts.to_vec());
        match self.replace {
            true => ConflictPolicy::Replace,
            false => ConflictPolicy::Skip,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        self.record(event);
    }
}

pub struct Outcome {
    pub send: Result<(), Error>,
    pub recv: Result<(), Error>,
    pub sender: Tally,
    pub receiver: Tally,
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

pub async fn run(peers: Peers, files: Vec<PathBuf>, out: &Path, opts: RunOptions) -> Outcome {
    let mut outgoing = build_offer(&files).unwrap();
    if let Some(algorithm) = opts.checksums {
        (outgoing.add_checksums(algorithm, &mut |_| {}))
            .await
            .unwrap();
    }
    run_offer(peers, &outgoing, out, opts).await
}

pub async fn run_offer(peers: Peers, outgoing: &Outgoing, out: &Path, opts: RunOptions) -> Outcome {
    let Peers {
        mut sender,
        mut receiver,
        abort_sender,
        abort_receiver,
    } = peers;
    let entries = outgoing.offer.flatten();
    let limit = |after: Option<u64>, abort| after.map(|after| (after, abort));
    let mut sending = Side::new(
        &entries,
        limit(opts.abort_sender_after, &abort_sender),
        opts.cancel_sender_after,
    );
    let mut receiving = Side::new(
        &entries,
        limit(opts.abort_receiver_after, &abort_receiver),
        opts.cancel_receiver_after,
    );
    receiving.replace = opts.replace;
    let (sender_cancel, receiver_cancel) = (sending.token(), receiving.token());
    let mut on_send = |event: Event<'_>| sending.record(event);

    let send = send_files(
        sender.as_mut(),
        outgoing,
        !opts.no_compression,
        &mut on_send,
        sender_cancel,
    );
    let recv = receive_files(
        receiver.as_mut(),
        &outgoing.offer,
        out,
        &mut receiving,
        receiver_cancel,
    );
    let (send, recv) =
        tokio::time::timeout(Duration::from_secs(120), async { tokio::join!(send, recv) })
            .await
            .expect("the transfer hung");

    Outcome {
        send,
        recv,
        sender: sending.tally,
        receiver: receiving.tally,
        conflicts: receiving.conflicts,
    }
}

/// Until the peer hangs up or says its last word.
async fn listen<Out: Message + Debug, In: PeerMessage>(control: &mut Control<Out, In>) -> Vec<In> {
    let mut got = Vec::new();
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

/// Plays a receiver that opens with `raw`; returns what the sender said.
async fn fake_receiver(receiver: &mut Box<dyn Transport>, raw: &[u8]) -> Vec<SenderMessage> {
    let mut control = ReceiverControl::new(receiver.control().await.unwrap());
    control.send_frame(raw).await.unwrap();
    let got = listen(&mut control).await;
    drop(control);
    receiver.close(false).await;
    got
}

/// Plays a sender that sends `raw`; returns what the receiver said.
async fn fake_sender(sender: &mut Box<dyn Transport>, raw: &[u8]) -> Vec<ReceiverMessage> {
    let mut control = SenderControl::new(sender.control().await.unwrap());
    control.send_frame(raw).await.unwrap();
    let got = listen(&mut control).await;
    drop(control);
    sender.close(false).await;
    got
}

fn frames(messages: &[impl Message]) -> Vec<u8> {
    messages
        .iter()
        .flat_map(|message| frame::encode(message).unwrap())
        .collect()
}

/// Tests for a module's `pair`: those named, or all of them.
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
            cancelled_while_resolving_conflicts,
            sends_with_checksums,
            resumes_with_checksums,
            a_file_changed_after_hashing_fails_verification,
            a_stale_part_fails_verification,
            a_complete_file_that_differs_fails_verification,
            a_file_that_vanishes_fails_both_sides,
            a_receiver_without_a_usable_window_is_rejected,
            the_sender_keeps_within_the_window,
            unexpected_messages_are_errors,
            unknown_and_wrong_direction_messages_are_errors,
            malformed_frames_are_errors,
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
    use std::time::Instant;

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

    async fn run_one(pair: PairFn, f: &Fixture, name: &str) -> Outcome {
        let files = vec![f.src.join(name)];
        run(pair().await, files, &f.out, RunOptions::default()).await
    }

    pub async fn sends_a_single_file(pair: PairFn) {
        let f = fixture();
        let data = bytes(100_000, 1);
        write(&f.src.join("photo.jpg"), &data);

        let o = run_one(pair, &f, "photo.jpg").await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("photo.jpg")).unwrap(), data);
        for side in [&o.sender, &o.receiver] {
            assert_eq!(side.present, Some(0));
            assert_eq!(side.moved, 100_000);
        }
    }

    pub async fn sends_an_empty_file(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("empty"), &[]);

        let o = run_one(pair, &f, "empty").await;

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

        let o = run_one(pair, &f, "big.bin").await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("big.bin")).unwrap(), data);
        assert_eq!(o.sender.moved, data.len() as u64);
        assert_eq!(o.receiver.moved, data.len() as u64);
    }

    pub async fn sends_a_directory_tree(pair: PairFn) {
        let f = fixture();
        sample_tree(&f.src);

        let o = run_one(pair, &f, "album").await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender.moved, 80_003);
        assert_eq!(o.receiver.moved, 80_003);
    }

    pub async fn sends_several_entries_at_once(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);
        write(&f.src.join("first.txt"), &bytes(500, 7));
        write(&f.src.join("last.txt"), &bytes(0, 8));

        let files = vec![f.src.join("last.txt"), f.src.join("first.txt"), album];
        let o = run(pair().await, files, &f.out, RunOptions::default()).await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        // In name order: album's files, then first.txt.
        let moved = BTreeMap::from([(1, 10_000), (4, 70_000), (6, 3), (8, 500)]);
        assert_eq!(o.sender.files, moved);
        assert_eq!(o.receiver.files, moved);
    }

    pub async fn sends_the_current_directory(pair: PairFn) {
        let f = fixture();
        sample_tree(&f.src);

        // `fsend send .` hands over a path without a file name of its own.
        let o = run_one(pair, &f, "album/nested/..").await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
    }

    pub async fn sends_compressible_data_either_way(pair: PairFn) {
        for no_compression in [false, true] {
            let f = fixture();
            // Blocks that span files, and one file of several blocks.
            let text = b"all work and no play makes jack a dull boy\n".repeat(60_000);
            write(&f.src.join("d/big.txt"), &text);
            let mut total = text.len() + 300_000;
            for i in 0..50 {
                write(&f.src.join(format!("d/small-{i}.txt")), &text[..i * 997]);
                total += i * 997;
            }
            write(&f.src.join("d/noise.bin"), &bytes(300_000, 7));

            let opts = RunOptions {
                no_compression,
                ..Default::default()
            };
            let o = run(pair().await, vec![f.src.join("d")], &f.out, opts).await;

            o.unwrap();
            assert_eq!(snapshot(&f.out), snapshot(&f.src));
            assert_eq!(o.sender.moved, total as u64);
            assert_eq!(o.receiver.moved, total as u64);
        }
    }

    pub async fn progress_moves_in_small_steps(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("big.bin"), &bytes(3 * MAX_BLOCK_BYTES, 8));

        let o = run_one(pair, &f, "big.bin").await;

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

    /// Also checks that compressing does not slow a transfer down.
    pub async fn throughput(pair: PairFn) {
        const MIB: usize = 256;
        const RUNS: usize = 3;
        let mut medians = Vec::new();
        for (name, data) in [
            ("noise.bin", bytes(MIB << 20, 9)),
            ("text.txt", text(MIB << 20)),
        ] {
            let f = fixture();
            write(&f.src.join(name), &data);
            drop(data);
            let mut secs = Vec::new();
            for _ in 0..RUNS {
                let _ = std::fs::remove_dir_all(&f.out);
                let peers = pair().await;
                let start = Instant::now();
                run(peers, vec![f.src.join(name)], &f.out, RunOptions::default())
                    .await
                    .unwrap();
                secs.push(start.elapsed().as_secs_f64());
            }
            secs.sort_by(f64::total_cmp);
            let median = secs[RUNS / 2];
            println!("{name}: {:.0} MiB/s", MIB as f64 / median);
            medians.push(median);
        }
        assert!(
            medians[1] <= medians[0] * 1.1,
            "text took {:.2} s, noise {:.2} s",
            medians[1],
            medians[0]
        );
    }

    pub async fn resumes_a_partial_file(pair: PairFn) {
        let f = fixture();
        let data = bytes(200_000, 3);
        write(&f.src.join("big.bin"), &data);
        write(&f.out.join("big.bin"), &data[..80_000]);

        let o = run_one(pair, &f, "big.bin").await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("big.bin")).unwrap(), data);
        for side in [&o.sender, &o.receiver] {
            assert_eq!(side.present, Some(80_000));
            assert_eq!(side.moved, 120_000);
        }
    }

    pub async fn resumes_a_partial_directory(pair: PairFn) {
        let f = fixture();
        sample_tree(&f.src);
        // a.txt half there, b.txt complete, c.txt and empty.bin missing.
        write(&f.out.join("album/a.txt"), &bytes(10_000, 1)[..4_000]);
        write(&f.out.join("album/nested/b.txt"), &bytes(70_000, 2));

        let o = run_one(pair, &f, "album").await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.sender.present, Some(74_000));
        assert_eq!(o.sender.moved, 6_003);
        assert_eq!(o.receiver.moved, 6_003);
    }

    pub async fn resume_skips_complete_files(pair: PairFn) {
        let f = fixture();
        let data = bytes(50_000, 4);
        write(&f.src.join("done.bin"), &data);
        write(&f.out.join("done.bin"), &data);

        let o = run_one(pair, &f, "done.bin").await;

        o.unwrap();
        assert_eq!(std::fs::read(f.out.join("done.bin")).unwrap(), data);
        assert_eq!(o.sender.present, Some(50_000));
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
        assert_eq!(o.sender.present, Some(0));
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

        let o = run_one(pair, &f, "f.bin").await;

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

    /// Progress counts blocks as they arrive, up to two ahead of the disk.
    fn arrived_ahead_of_disk(moved: u64, written: u64) {
        assert!(
            (written..written + 2 * MAX_BLOCK_BYTES as u64).contains(&moved),
            "{moved} bytes counted, {written} written"
        );
    }

    const INTERRUPT_SIZE: usize = 3 * 1024 * 1024;
    const INTERRUPT_AFTER: u64 = 1024 * 1024;

    /// Checks that a stopped run leaves a prefix that a fresh session finishes.
    async fn stopped_then_resumed(pair: PairFn, opts: RunOptions) -> (Outcome, u64) {
        let f = fixture();
        let data = bytes(INTERRUPT_SIZE, 6);
        write(&f.src.join("movie.bin"), &data);

        let stopped = run(pair().await, vec![f.src.join("movie.bin")], &f.out, opts).await;

        assert!(stopped.send.is_err() && stopped.recv.is_err());
        // A sender that stops early enough leaves nothing behind.
        let partial = std::fs::metadata(f.out.join("movie.bin")).map_or(0, |m| m.len());
        assert!(
            partial < data.len() as u64,
            "partial file of {partial} bytes"
        );
        arrived_ahead_of_disk(stopped.receiver.moved, partial);
        assert_eq!(
            std::fs::read(f.out.join("movie.bin")).unwrap_or_default(),
            &data[..partial as usize]
        );

        let o = run_one(pair, &f, "movie.bin").await;

        o.unwrap();
        assert_eq!(o.sender.present, Some(partial));
        assert_eq!(o.sender.moved, data.len() as u64 - partial);
        assert_eq!(std::fs::read(f.out.join("movie.bin")).unwrap(), data);
        (stopped, partial)
    }

    pub async fn interrupted_by_the_receiver_then_resumed(pair: PairFn) {
        let opts = RunOptions {
            abort_receiver_after: Some(INTERRUPT_AFTER),
            ..Default::default()
        };
        let (_, partial) = stopped_then_resumed(pair, opts).await;
        // What arrived before the abort was written.
        assert!(partial >= INTERRUPT_AFTER, "{partial}");
    }

    pub async fn interrupted_by_the_sender_then_resumed(pair: PairFn) {
        let opts = RunOptions {
            abort_sender_after: Some(INTERRUPT_AFTER),
            ..Default::default()
        };
        stopped_then_resumed(pair, opts).await;
    }

    pub async fn cancelled_by_the_receiver_then_resumed(pair: PairFn) {
        let opts = RunOptions {
            cancel_receiver_after: Some(INTERRUPT_AFTER),
            ..Default::default()
        };
        let (o, _) = stopped_then_resumed(pair, opts).await;
        assert!(matches!(o.recv, Err(Error::Cancelled)), "{:?}", o.recv);
        assert!(
            matches!(o.send, Err(Error::PeerCancelled(None))),
            "{:?}",
            o.send
        );
    }

    pub async fn cancelled_by_the_sender_then_resumed(pair: PairFn) {
        let opts = RunOptions {
            cancel_sender_after: Some(INTERRUPT_AFTER),
            ..Default::default()
        };
        let (o, _) = stopped_then_resumed(pair, opts).await;
        assert!(matches!(o.send, Err(Error::Cancelled)), "{:?}", o.send);
        assert!(
            matches!(o.recv, Err(Error::PeerCancelled(None))),
            "{:?}",
            o.recv
        );
    }

    /// Cancels while its user is still looking at the conflicts.
    struct CancelWhileResolving(CancellationToken);

    #[async_trait]
    impl ReceiveHandler for CancelWhileResolving {
        async fn accept(&mut self, _: &Offer) -> bool {
            true
        }

        async fn resolve(&mut self, _: &[Entry], _: &[Conflict]) -> ConflictPolicy {
            self.0.cancel();
            std::future::pending().await
        }

        fn event(&mut self, _: Event<'_>) {}
    }

    pub async fn cancelled_while_resolving_conflicts(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("a.txt"), b"hello");
        write(&f.out.join("a.txt"), b"other");
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let outgoing = build_offer(&[f.src.join("a.txt")]).unwrap();
        let cancel = CancellationToken::new();
        let mut handler = CancelWhileResolving(cancel.clone());
        let mut on_send = |_: Event<'_>| {};

        let send = send_files(
            sender.as_mut(),
            &outgoing,
            true,
            &mut on_send,
            CancellationToken::new(),
        );
        let recv = receive_files(
            receiver.as_mut(),
            &outgoing.offer,
            &f.out,
            &mut handler,
            cancel,
        );
        let (send, recv) =
            tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(send, recv) })
                .await
                .expect("the transfer hung");

        assert!(matches!(recv, Err(Error::Cancelled)), "{recv:?}");
        assert!(matches!(send, Err(Error::PeerCancelled(None))), "{send:?}");
        assert_eq!(std::fs::read(f.out.join("a.txt")).unwrap(), b"other");
    }

    fn checksums() -> RunOptions {
        RunOptions {
            checksums: Some(Algorithm::Sha256),
            ..Default::default()
        }
    }

    /// The sender only sees the receiver hang up.
    fn mismatch_at(o: &Outcome, path: &Path) {
        assert!(
            matches!(&o.recv, Err(Error::ChecksumMismatch(at)) if at == path),
            "{:?}",
            o.recv
        );
        assert!(
            matches!(o.send, Err(Error::Disconnected(_))),
            "the sender is not told why: {:?}",
            o.send
        );
    }

    pub async fn sends_with_checksums(pair: PairFn) {
        let f = fixture();
        let album = sample_tree(&f.src);

        let o = run(pair().await, vec![album], &f.out, checksums()).await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        assert_eq!(o.receiver.verifying, None, "nothing was here to check");
    }

    pub async fn resumes_with_checksums(pair: PairFn) {
        let f = fixture();
        let (data, done) = (bytes(200_000, 3), bytes(50_000, 4));
        write(&f.src.join("big.bin"), &data);
        write(&f.out.join("big.bin"), &data[..80_000]);
        write(&f.src.join("done.bin"), &done);
        write(&f.out.join("done.bin"), &done);

        let files = vec![f.src.join("big.bin"), f.src.join("done.bin")];
        let o = run(pair().await, files, &f.out, checksums()).await;

        o.unwrap();
        assert_eq!(snapshot(&f.out), snapshot(&f.src));
        // The part of big.bin and all of done.bin are read before anything arrives.
        assert_eq!(o.receiver.verifying, Some((130_000, 130_000)));
        assert_eq!(o.sender.moved, 120_000);
    }

    pub async fn a_file_changed_after_hashing_fails_verification(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("a.bin"), &bytes(100_000, 1));
        let mut outgoing = build_offer(&[f.src.join("a.bin")]).unwrap();
        (outgoing.add_checksums(Algorithm::Sha256, &mut |_| {}))
            .await
            .unwrap();
        let changed = bytes(100_000, 2);
        write(&f.src.join("a.bin"), &changed);

        let o = run_offer(pair().await, &outgoing, &f.out, RunOptions::default()).await;

        mismatch_at(&o, &f.out.join("a.bin"));
        // Kept for the user to look at.
        assert_eq!(std::fs::read(f.out.join("a.bin")).unwrap(), changed);
    }

    /// Differs where the fingerprint does not look.
    fn differing_in_the_middle(data: &[u8], len: usize) -> Vec<u8> {
        let mut local = data[..len].to_vec();
        local[len / 2] ^= 1;
        local
    }

    pub async fn a_stale_part_fails_verification(pair: PairFn) {
        let f = fixture();
        let data = bytes(INTERRUPT_SIZE, 5);
        write(&f.src.join("f.bin"), &data);
        let local = differing_in_the_middle(&data, INTERRUPT_SIZE - 100_000);
        write(&f.out.join("f.bin"), &local);

        let o = run(pair().await, vec![f.src.join("f.bin")], &f.out, checksums()).await;

        mismatch_at(&o, &f.out.join("f.bin"));
        assert_eq!(o.sender.moved, 100_000, "the part resumed as usual");
        assert_eq!(
            std::fs::metadata(f.out.join("f.bin")).unwrap().len(),
            data.len() as u64
        );
    }

    pub async fn a_complete_file_that_differs_fails_verification(pair: PairFn) {
        let f = fixture();
        let data = bytes(INTERRUPT_SIZE, 5);
        write(&f.src.join("f.bin"), &data);
        let local = differing_in_the_middle(&data, INTERRUPT_SIZE);
        write(&f.out.join("f.bin"), &local);

        let o = run(pair().await, vec![f.src.join("f.bin")], &f.out, checksums()).await;

        mismatch_at(&o, &f.out.join("f.bin"));
        assert!(o.sender.present.is_none(), "it fails before the accept");
        assert_eq!(std::fs::read(f.out.join("f.bin")).unwrap(), local);
    }

    pub async fn a_file_that_vanishes_fails_both_sides(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("gone.txt"), &bytes(100, 1));
        let outgoing = build_offer(&[f.src.join("gone.txt")]).unwrap();
        std::fs::remove_file(f.src.join("gone.txt")).unwrap();

        let o = run_offer(pair().await, &outgoing, &f.out, RunOptions::default()).await;

        assert!(matches!(o.send, Err(Error::Io(_))), "{:?}", o.send);
        assert!(
            matches!(
                &o.recv,
                Err(Error::Peer(ErrorReport {
                    code: ErrorCode::Io,
                    ..
                }))
            ),
            "the receiver learns why: {:?}",
            o.recv
        );
    }

    /// A sender of `a.txt` against a receiver that opens with `raw`.
    async fn hostile_receiver(pair: PairFn, raw: &[u8]) -> (Vec<SenderMessage>, Result<(), Error>) {
        let f = fixture();
        write(&f.src.join("a.txt"), b"hello");
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let outgoing = build_offer(&[f.src.join("a.txt")]).unwrap();
        let mut on_send = |_: Event<'_>| {};
        let send = send_files(
            sender.as_mut(),
            &outgoing,
            true,
            &mut on_send,
            CancellationToken::new(),
        );
        tokio::join!(fake_receiver(&mut receiver, raw), send)
    }

    /// A receiver of a `size` byte file against a sender that sends `raw`.
    async fn hostile_sender_into(
        pair: PairFn,
        raw: &[u8],
        size: u64,
        out: &Path,
    ) -> (Vec<ReceiverMessage>, Result<(), Error>) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let offer = Offer {
            checksums: None,
            entries: vec![Node::File {
                name: "a".into(),
                size,
                checksum: None,
            }],
        };
        let entries = offer.flatten();
        let mut handler = Side::new(&entries, None, None);
        let receive = receive_files(
            receiver.as_mut(),
            &offer,
            out,
            &mut handler,
            CancellationToken::new(),
        );
        tokio::join!(fake_sender(&mut sender, raw), receive)
    }

    async fn hostile_sender(pair: PairFn, raw: &[u8]) -> (Vec<ReceiverMessage>, Result<(), Error>) {
        let f = fixture();
        hostile_sender_into(pair, raw, 1, &f.out).await
    }

    fn is_error(message: &(impl PeerMessage + Clone), expected: ErrorCode) -> bool {
        matches!(message.clone().unexpected(), Error::Peer(report) if report.code == expected)
    }

    fn is_protocol_error(result: &Result<(), Error>) -> bool {
        matches!(result, Err(Error::Protocol(_)))
    }

    fn accept(files: Vec<AcceptedFile>) -> ReceiverMessage {
        ReceiverMessage::Accept {
            files,
            window: WINDOW,
        }
    }

    /// Both sides must fail with a protocol error.
    async fn both_fail_with_a_protocol_error(
        pair: PairFn,
        from_receiver: &[u8],
        from_sender: &[u8],
    ) {
        let (got, result) = hostile_receiver(pair, from_receiver).await;
        assert!(is_error(&got[0], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");

        let (got, result) = hostile_sender(pair, from_sender).await;
        assert!(matches!(got[0], ReceiverMessage::Accept { .. }), "{got:?}");
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
    }

    pub async fn a_receiver_without_a_usable_window_is_rejected(pair: PairFn) {
        let accept = ReceiverMessage::Accept {
            files: vec![AcceptedFile { id: 0, offset: 0 }],
            window: MIN_WINDOW - 1,
        };
        let (got, result) = hostile_receiver(pair, &frames(&[accept])).await;
        assert!(is_error(&got[0], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
    }

    pub async fn the_sender_keeps_within_the_window(pair: PairFn) {
        let f = fixture();
        write(&f.src.join("big.bin"), &bytes(4 * MIN_WINDOW as usize, 1));
        let outgoing = build_offer(&[f.src.join("big.bin")]).unwrap();
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let mut on_send = |_: Event<'_>| {};
        let send = send_files(
            sender.as_mut(),
            &outgoing,
            true,
            &mut on_send,
            CancellationToken::new(),
        );

        let receive = async {
            let mut control = ReceiverControl::new(receiver.control().await.unwrap());
            let accept = ReceiverMessage::Accept {
                files: vec![AcceptedFile { id: 0, offset: 0 }],
                window: MIN_WINDOW,
            };
            control.send(&accept).await.unwrap();
            let mut data = receiver.data().await.unwrap();
            let (mut decoder, mut buf, mut got) = (BlockDecoder::default(), vec![0; BUF_SIZE], 0);
            for acked in [0, MIN_WINDOW] {
                if acked > 0 {
                    control
                        .send(&ReceiverMessage::Ack { bytes: acked })
                        .await
                        .unwrap();
                }
                // A full window arrives, and nothing past it.
                while got < acked + MIN_WINDOW {
                    let n = data.read(&mut buf).await.unwrap();
                    decoder.push(&buf[..n]);
                    while let Some(block) = decoder.next_block().unwrap() {
                        got += block.len() as u64;
                    }
                }
                let more = tokio::time::timeout(Duration::from_millis(300), data.read(&mut buf));
                assert!(more.await.is_err(), "sent past the window");
                assert_eq!(got, acked + MIN_WINDOW);
            }
            let cancel = ReceiverMessage::Cancel(Cancel { reason: None });
            control.send(&cancel).await.unwrap();
            drop((control, data));
            receiver.close(false).await;
        };

        let (result, ()) = tokio::join!(send, receive);
        assert!(
            matches!(result, Err(Error::PeerCancelled(None))),
            "{result:?}"
        );
    }

    pub async fn unexpected_messages_are_errors(pair: PairFn) {
        // Hashes nobody asked for.
        let hashes = SenderMessage::Hashes { hashes: vec![] };
        let (got, result) = hostile_sender(pair, &frames(&[hashes])).await;
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");

        // Done before anything happened.
        let (got, result) = hostile_receiver(pair, &frames(&[ReceiverMessage::Done])).await;
        assert!(is_error(&got[0], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
    }

    pub async fn unknown_and_wrong_direction_messages_are_errors(pair: PairFn) {
        // The offer travels in pairing, so neither side may send it.
        let offer = json_frame(r#"{"type":"offer","entries":[]}"#);
        both_fail_with_a_protocol_error(pair, &offer, &frames(&[accept(vec![])])).await;
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
        both_fail_with_a_protocol_error(pair, &[0, 0, 0, 0], &[0, 0, 0, 0]).await;
        // Messages without their fields.
        both_fail_with_a_protocol_error(
            pair,
            &json_frame(r#"{"type":"accept","files":[]}"#),
            &json_frame(r#"{"type":"hashes"}"#),
        )
        .await;
    }

    pub async fn an_invalid_accept_is_rejected(pair: PairFn) {
        let accept = accept(vec![AcceptedFile { id: 7, offset: 0 }]);
        let (got, result) = hostile_receiver(pair, &frames(&[accept])).await;
        assert!(is_error(&got[0], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
    }

    pub async fn an_invalid_check_is_rejected(pair: PairFn) {
        let check = ReceiverMessage::Check {
            files: vec![CheckedFile { id: 0, len: 6 }],
        };
        let (got, result) = hostile_receiver(pair, &frames(&[check])).await;
        assert!(is_error(&got[0], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
    }

    pub async fn a_second_check_is_rejected(pair: PairFn) {
        let check = ReceiverMessage::Check {
            files: vec![CheckedFile { id: 0, len: 5 }],
        };
        let (got, result) = hostile_receiver(pair, &frames(&[check.clone(), check])).await;
        let hashes = vec![Fingerprint::of(b"hello")];
        assert_eq!(got[0], SenderMessage::Hashes { hashes });
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
    }

    pub async fn hashes_must_match_the_check(pair: PairFn) {
        let f = fixture();
        write(&f.out.join("a"), b"x");
        let hashes = SenderMessage::Hashes { hashes: vec![] };
        let (got, result) = hostile_sender_into(pair, &frames(&[hashes]), 5, &f.out).await;
        let files = vec![CheckedFile { id: 0, len: 1 }];
        assert_eq!(got[0], ReceiverMessage::Check { files });
        assert!(is_error(&got[1], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
        assert_eq!(std::fs::read(f.out.join("a")).unwrap(), b"x");
    }

    pub async fn an_ack_beyond_the_data_is_rejected(pair: PairFn) {
        let accept = accept(vec![AcceptedFile { id: 0, offset: 0 }]);
        let ack = ReceiverMessage::Ack { bytes: 1000 };
        let (got, result) = hostile_receiver(pair, &frames(&[accept, ack])).await;
        assert!(is_error(&got[0], ErrorCode::Protocol), "{got:?}");
        assert!(is_protocol_error(&result), "{result:?}");
    }

    pub async fn a_cancel_from_the_peer_ends_the_transfer(pair: PairFn) {
        let cancel = ReceiverMessage::Cancel(Cancel {
            reason: Some("changed my mind".into()),
        });
        let (got, result) = hostile_receiver(pair, &frames(&[cancel])).await;
        assert!(got.is_empty(), "{got:?}");
        assert!(
            matches!(&result, Err(Error::PeerCancelled(Some(r))) if r == "changed my mind"),
            "{result:?}"
        );
    }

    pub async fn messages_survive_the_wire(pair: PairFn) {
        let Peers {
            mut sender,
            mut receiver,
            ..
        } = pair().await;
        let check = ReceiverMessage::Check {
            files: (0..50_000).map(|id| CheckedFile { id, len: 1 }).collect(),
        };
        let hashes = SenderMessage::Hashes {
            hashes: (0..50_000).map(Fingerprint).collect(),
        };

        // The sender's stream only exists once the receiver has written.
        let mut b = ReceiverControl::new(receiver.control().await.unwrap());
        let (sent, mut a) = tokio::join!(b.send(&check), async {
            SenderControl::new(sender.control().await.unwrap())
        });
        sent.unwrap();
        assert_eq!(a.recv().await.unwrap(), check);
        a.send(&hashes).await.unwrap();
        assert_eq!(b.recv().await.unwrap(), hashes);

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
        receiver.close(false).await;

        let result = match sender.control().await {
            Err(e) => Err(e),
            Ok(stream) => SenderControl::new(stream).recv().await,
        };
        assert!(result.is_err(), "{result:?}");
        sender.close(false).await;
    }
}
