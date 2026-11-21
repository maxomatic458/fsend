import { test, describe, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";

import {
    installBrowserEnv,
    resetBrowserEnv,
    MemoryDirectoryHandle,
    getDownloads,
    makeEntries,
    expectedSnapshot,
    makeRecorder,
    bytes,
    waitFor,
    jsonFrame,
    sleep,
    unzip,
} from "./harness/index.mjs";
import { ice, sctpLimits } from "./harness/webrtc.mjs";
import { reopening, storageLimit } from "./harness/fs-access.mjs";

const {
    BrowserStorage,
    runSend,
    runReceive,
    createFolderSink,
    createZipSink,
    encodeFrame,
    Role,
    MAX_DATA_MESSAGE_BYTES,
} = await import("./.build/app.mjs");

let peerConnections;

beforeEach(() => {
    peerConnections = installBrowserEnv({ latencyMs: 0 });
});
afterEach(() => resetBrowserEnv());

/** Assert two snapshots have identical paths and bytes. */
function assertFiles(actual, expected) {
    assert.deepEqual(Object.keys(actual).sort(), Object.keys(expected).sort(), "file paths differ");
    for (const path of Object.keys(expected)) {
        assert.deepEqual(
            Array.from(actual[path]),
            Array.from(expected[path]),
            `contents differ for ${path}`,
        );
    }
}

/**
 * Starts a transfer of the entries `spec` describes; a test steps in before
 * awaiting `senderDone` and `receiverDone`.
 */
async function start(
    spec,
    {
        mode = "file",
        policy = "skip",
        sink = createFolderSink(new MemoryDirectoryHandle()),
        options,
        sendSignal = new AbortController().signal,
        recvSignal = new AbortController().signal,
        recv = makeRecorder(),
        sendOptions,
    } = {},
) {
    const send = makeRecorder();
    const senderDone = runSend(makeEntries(spec, mode), send.emit, sendSignal, sendOptions);
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(code, sink, policy, recv.emit, recvSignal, options);
    return { send, recv, senderDone, receiverDone };
}

async function transfer(spec, { dir, ...opts } = {}) {
    const dirHandle = dir ?? new MemoryDirectoryHandle();
    const { send, recv, senderDone, receiverDone } = await start(spec, {
        sink: createFolderSink(dirHandle),
        ...opts,
    });
    await Promise.all([senderDone, receiverDone]);
    return { dirHandle, send: send.record, recv: recv.record };
}

/** Many small files around one larger than a block, in nested folders. */
function manyFiles() {
    const small = (seed) =>
        Object.fromEntries(
            Array.from({ length: 150 }, (_, i) => [`f${i}.bin`, bytes(i * 37, seed + i)]),
        );
    return {
        a: small(1000),
        "big.bin": bytes(1_500_000, 7),
        b: { c: small(2000), "empty.txt": new Uint8Array(0) },
    };
}

describe("many small files", () => {
    test("arrive intact in a folder", async () => {
        const spec = manyFiles();
        const { dirHandle, recv } = await transfer(spec);
        assert.equal(recv.error, null);
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });

    test("arrive intact in a zip", async () => {
        const spec = manyFiles();
        const { recv } = await transfer(spec, { sink: createZipSink() });
        assert.equal(recv.error, null);
        assertFiles(await unzip(getDownloads()[0].blob), expectedSnapshot(spec));
    });

    test("arrive intact when writes finish slowly and out of order", async () => {
        const spec = manyFiles();
        const dirHandle = new MemoryDirectoryHandle();
        const folder = createFolderSink(dirHandle);
        let n = 0;
        const sink = {
            ...folder,
            writeFile: async (...args) => {
                await sleep((n++ % 4) * 3);
                return folder.writeFile(...args);
            },
        };
        const { recv } = await transfer(spec, { dir: dirHandle, sink });
        assert.equal(recv.error, null);
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });
});

describe("complete transfers (File System Access API)", () => {
    test("a single file arrives byte-for-byte", async () => {
        const spec = { "hello.txt": bytes(1000, 7) };
        const { dirHandle, send, recv } = await transfer(spec);

        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        assert.ok(send.complete, "sender should complete");
        assert.ok(recv.complete, "receiver should complete");
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });

    test("several files and a nested folder keep their structure", async () => {
        const spec = {
            "top.bin": bytes(5000, 11),
            "notes.txt": bytes(120, 12),
            project: {
                "readme.md": bytes(800, 13),
                src: { "main.rs": bytes(4096, 14), "lib.rs": bytes(2048, 15) },
            },
        };
        const { dirHandle, send, recv } = await transfer(spec);

        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });

    test("files chosen through picker handles transfer identically", async () => {
        const spec = {
            "picked.bin": bytes(3000, 21),
            folder: { "x.dat": bytes(900, 22) },
        };
        const { dirHandle } = await transfer(spec, { mode: "handle" });
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });

    test("a file larger than the send buffer still completes", async () => {
        // Comfortably past MAX_BUFFERED_BYTES so the backpressure path is exercised.
        const spec = { "big.bin": bytes(3_000_000, 31) };
        const { dirHandle, send, recv } = await transfer(spec);

        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        assert.equal(recv.progress, 3_000_000, "receiver progress should match size");
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });

    test("an empty file is created", async () => {
        const spec = { "empty.txt": bytes(0) };
        const { dirHandle } = await transfer(spec);
        assert.deepEqual(Object.keys(dirHandle.snapshot()), ["empty.txt"]);
    });

    test("an empty folder is created", async () => {
        const spec = { project: { "a.txt": bytes(10, 1), empty: {} } };
        const { dirHandle, recv } = await transfer(spec, { mode: "handle" });
        assert.equal(recv.error, null);
        const project = await dirHandle.getDirectoryHandle("project");
        assert.equal((await project.getDirectoryHandle("empty")).kind, "directory");
    });

    test("progress lands on the entry it belongs to", async () => {
        const spec = {
            "a.bin": bytes(40_000, 16),
            dir: { "b.bin": bytes(30_000, 17), "c.bin": bytes(20_000, 18) },
        };
        const { send, recv } = await transfer(spec);
        assert.deepEqual(send.present, [40_000, 50_000]);
        assert.deepEqual(recv.present, [40_000, 50_000]);
    });

    test("a selection that breaks the offer rules is refused before pairing", async () => {
        const send = makeRecorder();
        await runSend(
            makeEntries({ "a.txt": bytes(1) }).concat(makeEntries({ "a.txt": bytes(2) })),
            send.emit,
            new AbortController().signal,
        );
        assert.match(send.record.error ?? "", /taken in its directory/);
        assert.equal(send.record.code, null, "no code is handed out");
    });
});

describe("the offer", () => {
    test("arrives before the peers connect, sorted by name at every level", async () => {
        const spec = { "b.txt": bytes(3, 1), a: { "z.bin": bytes(2, 2), "m.bin": bytes(1, 3) } };
        let connections;
        const recv = makeRecorder({
            onOffered: (e) => {
                connections = peerConnections.length;
                e.accept();
            },
        });
        const { senderDone, receiverDone } = await start(spec, { recv });
        await Promise.all([senderDone, receiverDone]);

        assert.ok(connections <= 1, "at most the sender has prepared its connection");
        assert.deepEqual(recv.record.offered, {
            entries: [
                {
                    kind: "dir",
                    name: "a",
                    entries: [
                        { kind: "file", name: "m.bin", size: 1 },
                        { kind: "file", name: "z.bin", size: 2 },
                    ],
                },
                { kind: "file", name: "b.txt", size: 3 },
            ],
        });
        assert.equal(recv.record.complete, true);
    });

    test("declining it connects nothing", async () => {
        const recv = makeRecorder({ onOffered: (e) => e.reject() });
        const { send, senderDone, receiverDone } = await start({ "a.bin": bytes(10, 4) }, { recv });
        await Promise.all([senderDone, receiverDone]);

        assert.ok(peerConnections.length <= 1, "only the sender prepared a connection");
        assert.match(send.record.error ?? "", /rejected/);
    });

    test("one larger than a relay frame arrives whole", async () => {
        // Names that hardly compress, so the sealed offer spans many frames.
        const names = Array.from({ length: 3000 }, (_, i) =>
            Array.from(bytes(12, i + 1), (b) => b.toString(16).padStart(2, "0")).join(""),
        );
        const spec = { d: Object.fromEntries(names.map((name, i) => [name, bytes(1, i)])) };
        const { dirHandle, recv } = await transfer(spec);
        assert.equal(recv.complete, true);
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });
});

describe("complete transfers (fallback, no File System Access API)", () => {
    async function fallbackTransfer(spec) {
        const { send, recv, senderDone, receiverDone } = await start(spec, {
            sink: createZipSink(),
        });
        await Promise.all([senderDone, receiverDone]);
        return { send: send.record, recv: recv.record };
    }

    test("a single file is offered as a direct download", async () => {
        const spec = { "report.pdf": bytes(2048, 41) };
        const { send, recv } = await fallbackTransfer(spec);

        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        const downloads = getDownloads();
        assert.equal(downloads.length, 1);
        assert.equal(downloads[0].name, "report.pdf");
        const got = new Uint8Array(await downloads[0].blob.arrayBuffer());
        assert.deepEqual(Array.from(got), Array.from(spec["report.pdf"]));
    });

    test("a folder is delivered as a zip", async () => {
        const spec = {
            bundle: { "a.txt": bytes(300, 51), "b.txt": bytes(400, 52) },
        };
        const { send, recv } = await fallbackTransfer(spec);

        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        const downloads = getDownloads();
        assert.equal(downloads.length, 1);
        assert.equal(downloads[0].name, "bundle.zip");
        assertFiles(await unzip(downloads[0].blob), expectedSnapshot(spec));
    });

    test("multiple loose files are zipped together", async () => {
        const spec = { "one.bin": bytes(100, 61), "two.bin": bytes(100, 62) };
        const { recv } = await fallbackTransfer(spec);
        assert.equal(recv.error, null);
        assert.equal(getDownloads()[0].name, "fsend-files.zip");
        assertFiles(await unzip(getDownloads()[0].blob), expectedSnapshot(spec));
    });
});

describe("browser storage (OPFS)", () => {
    /**
     * fsend's storage below a fresh origin, with `quota` bytes; `persisted`
     * is the quota once the user lets fsend keep its data, if they do, or
     * "unanswered" when they never decide.
     */
    const openStorage = (origin = new MemoryDirectoryHandle(), quota = 1e9, persisted) =>
        BrowserStorage.open(origin, {
            estimate: async () => ({ quota, usage: 0 }),
            persist: async () => {
                if (persisted === "unanswered") return new Promise(() => {});
                if (persisted === undefined) return false;
                quota = persisted;
                return true;
            },
        });

    /** One transfer of `spec` into `storage`, as the receive page runs it. */
    async function staged(storage, spec, { cutAfter, options } = {}) {
        const { send, recv, senderDone, receiverDone } = await start(spec, {
            sink: createFolderSink(storage.incoming, storage),
            policy: "replace",
            options,
        });
        if (cutAfter !== undefined) {
            await waitFor(() => recv.record.progress > cutAfter, {
                label: "partial download",
            });
            peerConnections.at(-2).simulateAbruptDisconnect();
            void senderDone.catch(() => {});
            await receiverDone;
        } else {
            await Promise.all([senderDone, receiverDone]);
        }
        return { send: send.record, recv: recv.record };
    }

    test("several files are zipped from storage without a copy", async () => {
        const storage = await openStorage();
        const spec = {
            "top.bin": bytes(5000, 181),
            folder: { "x.dat": bytes(900, 182), "empty.txt": new Uint8Array(0) },
        };
        const { recv } = await staged(storage, spec);

        assert.equal(recv.error, null);
        const [download] = getDownloads();
        assert.equal(download.name, "fsend-files.zip");
        assertFiles(await unzip(download.blob), expectedSnapshot(spec));
        // The zip is made of the stored files, which stay until the next transfer.
        assertFiles(storage.incoming.snapshot(), expectedSnapshot(spec));
        assert.equal(await storage.usage(), 5900);
        assert.ok(!recv.states.includes("askingStorage"), "it fits as it is");
        assert.ok(!recv.states.includes("packing"), "nothing to pack");
    });

    test("a resumed transfer of several files zips correctly", async () => {
        const storage = await openStorage();
        const spec = {
            "a.bin": bytes(600_000, 191),
            "b.bin": bytes(1_500_000, 192),
            "c.bin": bytes(400_000, 193),
        };
        const first = await staged(storage, spec, { cutAfter: 1_300_000 });
        assert.ok(first.recv.error, "the first attempt fails");

        // Complete and resumed files were not checksummed as they arrived.
        const { recv } = await staged(storage, spec);
        assert.equal(recv.error, null);
        assert.ok(recv.states.includes("packing"));
        assertFiles(await unzip(getDownloads().at(-1).blob), expectedSnapshot(spec));
    });

    test("a single file is saved as it is", async () => {
        const storage = await openStorage();
        const spec = { "movie.bin": bytes(3000, 183) };
        await staged(storage, spec);

        const [download] = getDownloads();
        assert.equal(download.name, "movie.bin");
        assertFiles(
            { "movie.bin": new Uint8Array(await download.blob.arrayBuffer()) },
            expectedSnapshot(spec),
        );
    });

    test("a partial file is reopened before the sender starts sending", async () => {
        // Reopening copies the file, which can outlast the sender's wait for acks.
        const storage = await openStorage();
        const spec = { "movie.bin": bytes(2_000_000, 183) };
        const first = await staged(storage, spec, { cutAfter: 1_300_000 });
        assert.ok(first.recv.error, "the first attempt fails");

        reopening.ms = 300;
        try {
            const { send, recv } = await staged(storage, spec);
            assert.equal(recv.error, null);
            assert.ok(reopening.doneAt > 0, "the partial file was reopened");
            assert.ok(
                reopening.doneAt <= send.at.transferring,
                "reopened before the sender was accepted",
            );
        } finally {
            reopening.ms = 0;
            reopening.doneAt = 0;
        }
    });

    test("an interrupted transfer resumes from storage", async () => {
        const storage = await openStorage();
        const spec = { "movie.bin": bytes(2_000_000, 184) };
        const first = await staged(storage, spec, { cutAfter: 1_300_000 });
        assert.ok(first.recv.error, "the first attempt fails");
        const partial = storage.incoming.snapshot()["movie.bin"].length;
        assert.ok(partial > 0 && partial < 2_000_000, `${partial} bytes kept`);

        const { send, recv } = await staged(storage, spec);
        assert.equal(recv.error, null);
        assert.equal(send.progress, 2_000_000 - partial);
        const download = getDownloads().at(-1);
        assertFiles(
            { "movie.bin": new Uint8Array(await download.blob.arrayBuffer()) },
            expectedSnapshot(spec),
        );
    });

    test("stale entries are dropped or replaced without asking", async () => {
        const storage = await openStorage();
        const put = async (name, data) => {
            const file = await storage.incoming.getFileHandle(name, { create: true });
            file.bytes = data;
        };
        await put("other.bin", bytes(10, 1));
        await put("folder", bytes(10, 2));
        await put("a.bin", bytes(10, 3));
        const spec = {
            folder: { "x.dat": bytes(900, 185) },
            "a.bin": bytes(20, 186),
        };

        const { recv } = await staged(storage, spec);
        assert.equal(recv.error, null);
        assertFiles(await unzip(getDownloads()[0].blob), expectedSnapshot(spec));
        assert.deepEqual(recv.conflicts, { replaced: 1, skipped: 0 });
    });

    test("a transfer that does not fit is refused", async () => {
        const storage = await openStorage(undefined, 1000);
        const { send, recv } = await staged(storage, {
            "big.bin": bytes(5000, 187),
        });
        assert.ok(recv.states.includes("askingStorage"), "more room was asked for");
        assert.match(recv.error ?? "", /Not enough browser storage/);
        assert.match(send.error ?? "", /The other side failed/);
    });

    for (const [why, persisted, skipAsking] of [
        ["the user denies more room", undefined, undefined],
        ["the user downloads it as a zip instead", "unanswered", Promise.resolve()],
    ]) {
        test(`a transfer without room is held in memory when ${why}`, async () => {
            const storage = await openStorage(undefined, 1000, persisted);
            const spec = { "big.bin": bytes(5000, 190) };
            const { recv } = await staged(storage, spec, {
                options: { fallback: createZipSink, skipAsking },
            });
            assert.equal(recv.error, null);
            assert.ok(recv.states.includes("askingStorage"));
            assert.ok(recv.states.includes("heldInMemory"));
            const [download] = getDownloads();
            assert.equal(download.name, "big.bin");
            assert.deepEqual(new Uint8Array(await download.blob.arrayBuffer()), spec["big.bin"]);
            assert.deepEqual(storage.incoming.snapshot(), {}, "nothing was stored");
        });
    }

    test("a transfer that fits once the user allows more room is received", async () => {
        // Firefox allows half the disk instead of 10 GiB once a site may keep its data.
        const storage = await openStorage(undefined, 1000, 1e9);
        const spec = { "big.bin": bytes(5000, 189) };
        const { recv } = await staged(storage, spec);
        assert.equal(recv.error, null);
        assert.ok(recv.states.includes("askingStorage"));
        assertFiles(storage.incoming.snapshot(), expectedSnapshot(spec));
    });

    test("storage that holds less than it reports fails clearly", async () => {
        // Chromium in a private window reports 10 GiB, then stops at about 4 GiB.
        const storage = await openStorage();
        storageLimit.left = 3000;
        try {
            const { send, recv } = await staged(storage, {
                "big.bin": bytes(5000, 188),
            });
            assert.match(
                recv.error ?? "",
                /Browser storage ran out.*Clear the browser storage, or choose Download as Zip/,
            );
            assert.match(send.error ?? "", /The other side failed/);
        } finally {
            storageLimit.left = Infinity;
        }
    });

    test("clearing removes everything kept", async () => {
        const storage = await openStorage();
        await staged(storage, { "a.bin": bytes(100, 188) }, {});
        assert.ok((await storage.usage()) > 0);
        await storage.clear();
        assert.equal(await storage.usage(), 0);
    });

    test("a browser that refuses storage opens none", async () => {
        const refusing = {
            getDirectoryHandle: async () => {
                throw new DOMException("denied", "SecurityError");
            },
        };
        assert.equal(await BrowserStorage.open(refusing), null);
    });
});

describe("interrupted transfers", () => {
    /** One attempt in which the peers get on as `outcome` says; `then` runs once both are trying. */
    async function attempt(outcome, then = async () => {}) {
        ice.outcome = outcome;
        try {
            const sendAbort = new AbortController();
            const recvAbort = new AbortController();
            const { send, recv, senderDone, receiverDone } = await start(
                { "a.bin": bytes(100, 70) },
                { sendSignal: sendAbort.signal, recvSignal: recvAbort.signal },
            );
            await waitFor(() => peerConnections.length >= 2, {
                label: "both connections",
            });
            await then(sendAbort, recvAbort);
            await Promise.all([senderDone, receiverDone]);
            return { send: send.record, recv: recv.record };
        } finally {
            ice.outcome = "connect";
        }
    }

    test("peers that never connect both fail instead of waiting", async () => {
        const { send, recv } = await attempt("fail");
        assert.match(recv.error ?? "", /WebRTC connection failed/);
        assert.match(send.error ?? "", /WebRTC connection failed/);
    });

    test("cancelling while the peers connect ends the attempt at once", async () => {
        let cancelledAt = 0;
        const { send, recv } = await attempt("stall", async (...aborts) => {
            cancelledAt = Date.now();
            for (const abort of aborts) abort.abort();
        });
        // Not after the connect timeout.
        assert.ok(Date.now() - cancelledAt < 1000, "both sides end at once");
        assert.equal(recv.error, null, "a cancel is not an error");
        assert.equal(send.error, null, "a cancel is not an error");
    });

    test("sender stops when the receiver's tab disappears", async () => {
        const spec = { "big.bin": bytes(4_000_000, 71) };
        const { send, senderDone, receiverDone } = await start(spec);

        // Wait until data is genuinely flowing, then yank the receiver.
        await waitFor(() => send.record.progress > 100_000, {
            label: "transfer under way",
        });
        peerConnections[1].simulateAbruptDisconnect();

        // The receiver is gone; its promise never settles, exactly as it would
        // not in a closed tab. Only the surviving sender is awaited.
        void receiverDone;
        await senderDone;

        assert.equal(send.record.error, "Peer disconnected", "sender must notice the peer is gone");
        assert.equal(send.record.complete, false, "sender must not report success");
        assert.ok(
            send.record.progress < 4_000_000,
            "sender should have stopped early, not streamed the whole file",
        );
    });

    test("sender stops even when the send buffer never fills", async () => {
        // With bufferedAmount at zero, only polling the inbox between blocks can notice.
        resetBrowserEnv();
        peerConnections = installBrowserEnv({ instantDrain: true });

        const spec = { "big.bin": bytes(4_000_000, 141) };
        const { send, senderDone, receiverDone } = await start(spec);

        // Yank the peer after a fixed number of chunks rather than after a delay:
        // with no backpressure the sender outruns any timer.
        const channel = await waitFor(() => peerConnections[0]?._channels.get("data"), {
            label: "sender data channel",
        });
        const realSend = channel.send.bind(channel);
        let chunks = 0;
        channel.send = (data) => {
            realSend(data);
            if (++chunks === 10) peerConnections[1].simulateAbruptDisconnect();
        };

        void receiverDone;
        await senderDone;

        assert.equal(channel.bufferedAmount, 0, "precondition: buffer never fills");
        assert.equal(send.record.error, "Peer disconnected");
        assert.equal(send.record.complete, false);
        assert.ok(send.record.progress < 4_000_000, "sender should have stopped early");
    });

    test("receiver stops when the sender's tab disappears", async () => {
        const spec = { "big.bin": bytes(4_000_000, 81) };
        const { recv, senderDone, receiverDone } = await start(spec);

        await waitFor(() => recv.record.progress > 100_000, {
            label: "transfer under way",
        });
        peerConnections[0].simulateAbruptDisconnect();

        void senderDone.catch(() => {});
        await receiverDone;

        assert.equal(recv.record.error, "Peer disconnected");
        assert.equal(recv.record.complete, false);
    });

    test("rejecting the offer stops both sides cleanly", async () => {
        const spec = { "unwanted.bin": bytes(500, 91) };
        const { send, recv, senderDone, receiverDone } = await start(spec, {
            recv: makeRecorder({ onOffered: (e) => e.reject() }),
        });
        await Promise.all([senderDone, receiverDone]);

        assert.match(
            send.record.error ?? "",
            /reject/i,
            "the sender should learn it was declined, not just that the peer vanished",
        );
        assert.equal(send.record.complete, false);
        assert.match(recv.record.error ?? "", /reject/i);
    });

    test("cancelling a receive tells the sender", async () => {
        const spec = { "big.bin": bytes(3_000_000, 102) };
        const controller = new AbortController();
        const { send, recv, senderDone, receiverDone } = await start(spec, {
            recvSignal: controller.signal,
        });
        await waitFor(() => recv.record.progress > 100_000, {
            label: "transfer under way",
        });
        controller.abort();
        await Promise.all([senderDone, receiverDone]);

        assert.equal(recv.record.error, null, "a deliberate abort is no error");
        assert.match(send.record.error ?? "", /cancelled/);
        assert.equal(send.record.complete, false);
    });

    test("aborting the sender reports no error to the user", async () => {
        const spec = { "big.bin": bytes(2_000_000, 101) };
        const send = makeRecorder();
        const controller = new AbortController();

        const senderDone = runSend(makeEntries(spec), send.emit, controller.signal);
        await waitFor(() => send.record.code, { label: "share code" });
        controller.abort();
        // Aborting closes the relay connection, which ends the wait for a peer.
        await senderDone;

        assert.equal(send.record.error, null, "a deliberate abort is not an error worth showing");
        assert.equal(send.record.complete, false);
    });
});

describe("compression", () => {
    /** Runs a transfer of `spec`, counting the bytes on the sender's data channel. */
    async function counted(spec, tamper) {
        const dir = new MemoryDirectoryHandle();
        const { send, recv, senderDone, receiverDone } = await start(spec, {
            sink: createFolderSink(dir),
        });
        const channel = await waitFor(() => peerConnections[0]?._channels.get("data"), {
            label: "sender data channel",
        });
        const realSend = channel.send.bind(channel);
        let wire = 0;
        let largest = 0;
        channel.send = (data) => {
            wire += data.byteLength;
            largest = Math.max(largest, data.byteLength);
            realSend(tamper ? tamper(data) : data);
        };
        await Promise.all([senderDone, receiverDone]);
        return { dir, wire, largest, send: send.record, recv: recv.record };
    }

    test("compressible files cross in far fewer bytes", async () => {
        const line = new TextEncoder().encode("all work and no play\n".repeat(150_000));
        const spec = {
            "log.txt": line,
            notes: Object.fromEntries(
                Array.from({ length: 30 }, (_, i) => [`n${i}.txt`, line.slice(0, i * 500)]),
            ),
            "noise.bin": bytes(200_000, 191),
        };
        const { dir, wire, send, recv } = await counted(spec);
        assert.equal(recv.error, null);
        assertFiles(dir.snapshot(), expectedSnapshot(spec));
        const total = Object.values(expectedSnapshot(spec)).reduce((n, b) => n + b.length, 0);
        assert.equal(send.progress, total, "progress counts file bytes");
        assert.ok(wire < 200_000 + total / 10, `${wire} bytes on the wire for ${total}`);
    });

    test("progress moves a data channel message at a time, not a block", async () => {
        const spec = { "big.bin": bytes(3 * 1024 * 1024, 193) };
        const { send, recv } = await counted(spec);
        assert.equal(recv.error, null);
        for (const side of [send, recv]) {
            assert.equal(side.progress, 3 * 1024 * 1024);
            assert.ok(side.largestStep <= MAX_DATA_MESSAGE_BYTES, `steps of ${side.largestStep}`);
        }
    });

    test("data messages are as large as both ends take", async () => {
        const spec = { "big.bin": bytes(2 * 1024 * 1024, 194) };
        const { dir, largest } = await counted(spec);
        assertFiles(dir.snapshot(), expectedSnapshot(spec));
        assert.equal(largest, MAX_DATA_MESSAGE_BYTES);
    });

    test("a peer that takes smaller messages gets them", async () => {
        sctpLimits.maxMessageSize = 16 * 1024;
        try {
            const spec = { "big.bin": bytes(2 * 1024 * 1024, 195) };
            const { dir, largest } = await counted(spec);
            assertFiles(dir.snapshot(), expectedSnapshot(spec));
            assert.equal(largest, 16 * 1024);
        } finally {
            sctpLimits.maxMessageSize = 256 * 1024;
        }
    });

    test("a malformed block fails the receiver", async () => {
        let first = true;
        const { send, recv } = await counted({ "a.txt": bytes(5000, 192) }, (data) => {
            if (!first) return data;
            first = false;
            const copy = new Uint8Array(data);
            copy[4] = 0x7f;
            return copy;
        });
        assert.match(recv.error ?? "", /^Protocol error/);
        assert.match(send.error ?? "", /^The other side failed/);
    });
});

describe("protocol errors", () => {
    for (const [name, frame] of [
        [
            "a message only the sender sends",
            () => encodeFrame({ type: "hashes", hashes: [] }, Role.Sender),
        ],
        ["an unknown message", () => jsonFrame('{"type":"resume"}')],
        ["a malformed message", () => jsonFrame('{"type":"accept"}')],
        [
            "a check of an unknown file",
            () => encodeFrame({ type: "check", files: [{ id: 7, len: 1 }] }, Role.Receiver),
        ],
    ]) {
        test(`${name} from the receiver fails the sender`, async () => {
            const sink = createFolderSink(new MemoryDirectoryHandle());
            // Connected by then, and ahead of the receiver's accept.
            const prepare = sink.prepare;
            sink.prepare = (tops) => {
                peerConnections[1]._channels.get("control").send(frame());
                return prepare(tops);
            };
            const recv = makeRecorder();
            const { send, senderDone, receiverDone } = await start(
                { "a.bin": bytes(500, 5) },
                { recv, sink },
            );
            await Promise.all([senderDone, receiverDone]);

            assert.match(send.record.error ?? "", /^Protocol error/);
            assert.equal(send.record.complete, false);
            // The receiver learns why.
            assert.match(recv.record.error ?? "", /^The other side failed/);
        });
    }
});

describe("checksums", () => {
    const sha256 = (data) => createHash("sha256").update(data).digest("hex");
    const withChecksums = { sendOptions: { checksums: "sha256" } };
    const MIB = 1024 * 1024;

    test("come with every file and are checked as it arrives", async () => {
        const spec = {
            "a.bin": bytes(300_000, 1),
            d: { "b.bin": bytes(10, 2), "empty.bin": new Uint8Array(0) },
        };
        const { dirHandle, send, recv } = await transfer(spec, withChecksums);

        assert.equal(recv.error, null);
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
        const [a, d] = recv.offered.entries;
        assert.equal(recv.offered.checksums, "sha256");
        assert.equal(a.checksum, sha256(spec["a.bin"]));
        assert.deepEqual(
            d.entries.map((file) => file.checksum),
            [sha256(spec.d["b.bin"]), sha256(spec.d["empty.bin"])],
        );
        assert.ok(send.states.includes("hashing"));
        assert.ok(!recv.states.includes("verifying"), "nothing was here to check");
    });

    test("a file changed on the way fails the receiver, which the sender only sees leave", async () => {
        const spec = { "a.bin": bytes(200_000, 3) };
        const dir = new MemoryDirectoryHandle();
        const { send, recv, senderDone, receiverDone } = await start(spec, {
            sink: createFolderSink(dir),
            ...withChecksums,
        });
        const channel = await waitFor(() => peerConnections[0]?._channels.get("data"), {
            label: "sender data channel",
        });
        // The block goes raw, so its last byte here is a file byte.
        const realSend = channel.send.bind(channel);
        let tampered = false;
        channel.send = (data) => {
            if (!tampered) {
                data = new Uint8Array(data);
                data[data.length - 1] ^= 1;
                tampered = true;
            }
            realSend(data);
        };
        await Promise.all([senderDone, receiverDone]);

        assert.equal(
            recv.record.error,
            "a.bin does not match its checksum. Delete it to receive it again.",
        );
        assert.ok(send.record.error, "the sender fails too");
        assert.doesNotMatch(send.record.error, /checksum/, "but is not told why");
        assert.equal(dir.snapshot()["a.bin"].length, 200_000, "kept for the user to look at");
    });

    test("what is already here is checked before resuming", async () => {
        const spec = { "done.bin": bytes(5000, 4), "part.bin": bytes(300_000, 5) };
        const dir = new MemoryDirectoryHandle();
        (await dir.getFileHandle("done.bin", { create: true })).bytes = spec["done.bin"];
        (await dir.getFileHandle("part.bin", { create: true })).bytes = spec["part.bin"].slice(
            0,
            100_000,
        );

        const { send, recv } = await transfer(spec, { dir, ...withChecksums });

        assert.equal(recv.error, null);
        assertFiles(dir.snapshot(), expectedSnapshot(spec));
        assert.ok(recv.states.includes("verifying"));
        assert.equal(send.progress, 200_000, "only the missing part crosses the wire");
    });

    test("a complete file that differs fails before anything is accepted", async () => {
        // Past 2 MiB, the fingerprint samples only the ends.
        const offered = bytes(3 * MIB, 6);
        const local = offered.slice();
        local[1.5 * MIB] ^= 1;
        const dir = new MemoryDirectoryHandle();
        (await dir.getFileHandle("f.bin", { create: true })).bytes = local;

        const { send, recv } = await transfer({ "f.bin": offered }, { dir, ...withChecksums });

        assert.match(recv.error ?? "", /^f\.bin does not match its checksum/);
        assert.ok(send.error, "the sender sees the receiver leave");
        assert.ok(!send.states.includes("transferring"), "nothing was accepted");
        assert.deepEqual(dir.snapshot()["f.bin"], local);
    });
});

describe("resuming an interrupted download", () => {
    test("a cut-off transfer resumes and ends byte-perfect", async () => {
        const payload = bytes(2_000_000, 111);
        const spec = { "movie.bin": payload };
        const dirHandle = new MemoryDirectoryHandle();

        // First attempt: pull the sender away partway through.
        {
            const { recv, senderDone, receiverDone } = await start(spec, {
                sink: createFolderSink(dirHandle),
            });

            // Past the first block, so some of it is on disk.
            await waitFor(() => recv.record.progress > 1_300_000, {
                label: "partial download",
            });
            peerConnections[0].simulateAbruptDisconnect();
            void senderDone.catch(() => {});
            await receiverDone;
        }

        const partial = dirHandle.snapshot()["movie.bin"];
        assert.ok(partial.length > 0, "partial data should be on disk");
        assert.ok(partial.length < payload.length, "should not be complete yet");
        assert.deepEqual(
            Array.from(partial),
            Array.from(payload.subarray(0, partial.length)),
            "the partial prefix must match the source",
        );

        // Second attempt: only the remainder should be sent.
        const second = await transfer(spec, { dir: dirHandle });
        assert.equal(second.send.error, null);
        assert.equal(second.recv.error, null);
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
        assert.ok(
            second.send.progress < payload.length,
            `resume should skip the ${partial.length} bytes already on disk, ` +
                `but sent ${second.send.progress} of ${payload.length}`,
        );
    });

    test("complete files are skipped on resume", async () => {
        const spec = { "done.bin": bytes(5000, 161), "new.bin": bytes(300, 162) };
        const dirHandle = new MemoryDirectoryHandle();
        const done = await dirHandle.getFileHandle("done.bin", { create: true });
        done.bytes = spec["done.bin"];

        const { send, recv } = await transfer(spec, { dir: dirHandle });
        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
        assert.equal(send.progress, 300, "only the new file crosses the wire");
        assert.deepEqual(
            recv.entries.map((entry) => entry.skipBytes),
            [5000, 0],
        );
    });

    /** A folder with `a.bin` differing from the start and `b.bin` larger than offered. */
    const conflicting = async () => {
        const dir = new MemoryDirectoryHandle();
        const a = await dir.getFileHandle("a.bin", { create: true });
        a.bytes = bytes(2000, 9);
        const b = await dir.getFileHandle("b.bin", { create: true });
        b.bytes = bytes(400, 9);
        return dir;
    };
    const offered = { "a.bin": bytes(5000, 171), "b.bin": bytes(300, 172) };

    test("files that differ are skipped by default", async () => {
        const dir = await conflicting();
        const before = dir.snapshot();
        const { send, recv } = await transfer(offered, { dir });
        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        assertFiles(dir.snapshot(), before);
        assert.equal(send.progress, 0);
        assert.deepEqual(recv.conflicts, { replaced: 0, skipped: 2 });
    });

    test("files that differ are replaced when chosen", async () => {
        const dir = await conflicting();
        const { send, recv } = await transfer(offered, { dir, policy: "replace" });
        assert.equal(recv.error, null);
        assertFiles(dir.snapshot(), expectedSnapshot(offered));
        assert.equal(send.progress, 5300);
        assert.deepEqual(recv.conflicts, { replaced: 2, skipped: 0 });
    });

    test("entries of another kind in the way are always skipped", async () => {
        const dir = new MemoryDirectoryHandle();
        const file = await dir.getFileHandle("folder", { create: true });
        file.bytes = bytes(10, 1);
        await dir.getDirectoryHandle("f.bin", { create: true });
        const spec = {
            folder: { "x.dat": bytes(900, 22) },
            "f.bin": bytes(10, 2),
            "g.bin": bytes(10, 3),
        };
        const { send, recv } = await transfer(spec, { dir, policy: "replace" });
        assert.equal(recv.error, null);
        assert.deepEqual(Object.keys(dir.snapshot()).sort(), ["folder", "g.bin"]);
        assert.equal(send.progress, 10);
        assert.deepEqual(recv.conflicts, { replaced: 0, skipped: 2 });
    });

    test("resuming without existing data transfers everything", async () => {
        const spec = { "fresh.bin": bytes(50_000, 121) };
        const { dirHandle, send } = await transfer(spec);
        assert.equal(send.error, null);
        assert.equal(send.progress, 50_000, "every byte is sent");
        assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    });
});

describe("relay behaviour", () => {
    const receive = async (code) => {
        const recv = makeRecorder();
        await runReceive(
            code,
            createFolderSink(new MemoryDirectoryHandle()),
            "skip",
            recv.emit,
            new AbortController().signal,
        );
        assert.equal(recv.record.complete, false);
        return recv.record.error ?? "";
    };

    test("an unknown share code surfaces an error", async () => {
        assert.match(await receive("ZZZZ000000"), /no transfer with this code/i);
    });

    test("a malformed code is rejected before contacting the relay", async () => {
        assert.match(await receive("K7Q2"), /invalid code/i);
    });

    test("a wrong password fails on both sides", async () => {
        const send = makeRecorder();
        const senderDone = runSend(
            makeEntries({ "x.bin": bytes(100, 141) }),
            send.emit,
            new AbortController().signal,
        );
        const code = await waitFor(() => send.record.code, { label: "share code" });
        const last = code.endsWith("0") ? "1" : "0";
        const error = await receive(`${code.slice(0, -1)}${last}`);
        await senderDone;

        assert.match(error, /did not match/i);
        assert.match(send.record.error ?? "", /did not match/i);
    });

    test("only the bare code is accepted", async () => {
        const code = "K7Q29XMV4P";
        for (const input of [`https://fsend.sh/receive#${code}`, ` ${code}`]) {
            assert.match(await receive(input), /invalid code/i, input);
        }
    });

    test("a lowercase code is accepted", async () => {
        const send = makeRecorder();
        const recv = makeRecorder();
        const senderDone = runSend(
            makeEntries({ "x.bin": bytes(100, 151) }),
            send.emit,
            new AbortController().signal,
        );
        const code = await waitFor(() => send.record.code, { label: "share code" });
        assert.equal(code, code.toUpperCase(), "codes are shown upper-case");
        await runReceive(
            code.toLowerCase(),
            createFolderSink(new MemoryDirectoryHandle()),
            "skip",
            recv.emit,
            new AbortController().signal,
        );
        await senderDone;

        assert.equal(recv.record.error, null);
        assert.ok(recv.record.complete);
    });

    test("the connection is reported as direct", async () => {
        const { recv, send } = await transfer({ "x.bin": bytes(100, 131) });
        assert.equal(send.error, null);
        assert.equal(recv.error, null);
        await waitFor(() => recv.connectionType, { label: "connection type" });
        assert.equal(recv.connectionType, "direct");
    });
});
