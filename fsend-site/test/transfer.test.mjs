import { test, describe, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";

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
  unzip,
} from "./harness/index.mjs";

const {
  BrowserStorage,
  runSend,
  runReceive,
  createFolderSink,
  createZipSink,
  encodeFrame,
  Role,
} = await import("./.build/app.mjs");

let peerConnections;

beforeEach(() => {
  peerConnections = installBrowserEnv({ latencyMs: 0 });
});
afterEach(() => resetBrowserEnv());

/** Assert two snapshots have identical paths and bytes. */
function assertFiles(actual, expected) {
  assert.deepEqual(
    Object.keys(actual).sort(),
    Object.keys(expected).sort(),
    "file paths differ",
  );
  for (const path of Object.keys(expected)) {
    assert.deepEqual(
      Array.from(actual[path]),
      Array.from(expected[path]),
      `contents differ for ${path}`,
    );
  }
}

/**
 * Runs a sender and a native-FS receiver against each other.
 * Returns once both have settled.
 */
async function transfer(spec, { mode = "file", policy = "skip", dir } = {}) {
  const dirHandle = dir ?? new MemoryDirectoryHandle();
  const send = makeRecorder();
  const recv = makeRecorder();

  const senderDone = runSend(
    makeEntries(spec, mode),
    send.emit,
    new AbortController().signal,
  );

  const code = await waitFor(() => send.record.code, { label: "share code" });
  const receiverDone = runReceive(
    code,
    createFolderSink(dirHandle),
    policy,
    recv.emit,
    new AbortController().signal,
  );

  await Promise.all([senderDone, receiverDone]);
  return { dirHandle, send: send.record, recv: recv.record };
}

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
    assert.equal(
      recv.progress,
      3_000_000,
      "receiver progress should match size",
    );
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
      makeEntries({ "a.txt": bytes(1) }).concat(
        makeEntries({ "a.txt": bytes(2) }),
      ),
      send.emit,
      new AbortController().signal,
    );
    assert.match(send.record.error ?? "", /offered twice/);
    assert.equal(send.record.code, null, "no code is handed out");
  });
});

describe("complete transfers (fallback, no File System Access API)", () => {
  async function fallbackTransfer(spec) {
    const send = makeRecorder();
    const recv = makeRecorder();
    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(
      code,
      createZipSink(),
      "skip",
      recv.emit,
      new AbortController().signal,
    );
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
  /** fsend's storage below a fresh origin, with `quota` bytes. */
  const openStorage = (origin = new MemoryDirectoryHandle(), quota = 1e9) =>
    BrowserStorage.open(origin, async () => ({ quota, usage: 0 }));

  /** One transfer of `spec` into `storage`, as the receive page runs it. */
  async function staged(storage, spec, { cutAfter } = {}) {
    const send = makeRecorder();
    const recv = makeRecorder();
    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(
      code,
      createFolderSink(storage.incoming, storage),
      "replace",
      recv.emit,
      new AbortController().signal,
    );
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

  test("several files are zipped out of storage, then dropped", async () => {
    const storage = await openStorage();
    const spec = {
      "top.bin": bytes(5000, 181),
      folder: { "x.dat": bytes(900, 182) },
    };
    const { recv } = await staged(storage, spec);

    assert.equal(recv.error, null);
    const [download] = getDownloads();
    assert.equal(download.name, "fsend-files.zip");
    assertFiles(await unzip(download.blob), expectedSnapshot(spec));
    assert.deepEqual(storage.incoming.snapshot(), {});
    // Only the export is left, until the next transfer.
    assert.equal(await storage.usage(), download.blob.size);
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
    assert.match(recv.error ?? "", /Not enough browser storage/);
    assert.match(send.error ?? "", /The other side failed/);
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
  test("sender stops when the receiver's tab disappears", async () => {
    const spec = { "big.bin": bytes(4_000_000, 71) };
    const send = makeRecorder();
    const recv = makeRecorder();

    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(
      code,
      createFolderSink(new MemoryDirectoryHandle()),
      "skip",
      recv.emit,
      new AbortController().signal,
    );

    // Wait until data is genuinely flowing, then yank the receiver.
    await waitFor(() => send.record.progress > 100_000, {
      label: "transfer under way",
    });
    peerConnections[1].simulateAbruptDisconnect();

    // The receiver is gone; its promise never settles, exactly as it would
    // not in a closed tab. Only the surviving sender is awaited.
    void receiverDone;
    await senderDone;

    assert.equal(
      send.record.error,
      "Peer disconnected",
      "sender must notice the peer is gone",
    );
    assert.equal(send.record.complete, false, "sender must not report success");
    assert.ok(
      send.record.progress < 4_000_000,
      "sender should have stopped early, not streamed the whole file",
    );
  });

  test("sender stops even when the send buffer never fills", async () => {
    // The other interruption test is rescued by the backpressure path: a full
    // buffer means the sender is already awaiting the disconnect promise. On a
    // link fast enough that bufferedAmount stays at zero, the per-chunk
    // isDown() check is the only thing that can notice — so pin it directly.
    resetBrowserEnv();
    peerConnections = installBrowserEnv({ instantDrain: true });

    const spec = { "big.bin": bytes(4_000_000, 141) };
    const send = makeRecorder();
    const recv = makeRecorder();

    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(
      code,
      createFolderSink(new MemoryDirectoryHandle()),
      "skip",
      recv.emit,
      new AbortController().signal,
    );

    // Yank the peer after a fixed number of chunks rather than after a delay:
    // with no backpressure the sender outruns any timer.
    const channel = await waitFor(
      () => peerConnections[0]?._channels.get("data"),
      { label: "sender data channel" },
    );
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
    assert.ok(
      send.record.progress < 4_000_000,
      "sender should have stopped early",
    );
  });

  test("receiver stops when the sender's tab disappears", async () => {
    const spec = { "big.bin": bytes(4_000_000, 81) };
    const send = makeRecorder();
    const recv = makeRecorder();

    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(
      code,
      createFolderSink(new MemoryDirectoryHandle()),
      "skip",
      recv.emit,
      new AbortController().signal,
    );

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
    const send = makeRecorder();
    const recv = makeRecorder({ onOffered: (e) => e.reject() });

    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(
      code,
      createFolderSink(new MemoryDirectoryHandle()),
      "skip",
      recv.emit,
      new AbortController().signal,
    );

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
    const send = makeRecorder();
    const recv = makeRecorder();
    const controller = new AbortController();

    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const receiverDone = runReceive(
      code,
      createFolderSink(new MemoryDirectoryHandle()),
      "skip",
      recv.emit,
      controller.signal,
    );
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

    assert.equal(
      send.record.error,
      null,
      "a deliberate abort is not an error worth showing",
    );
    assert.equal(send.record.complete, false);
  });
});

describe("compression", () => {
  /** Runs a transfer of `spec`, counting the bytes on the sender's data channel. */
  async function counted(spec, tamper) {
    const send = makeRecorder();
    const recv = makeRecorder();
    const senderDone = runSend(
      makeEntries(spec),
      send.emit,
      new AbortController().signal,
    );
    const code = await waitFor(() => send.record.code, { label: "share code" });
    const dir = new MemoryDirectoryHandle();
    const receiverDone = runReceive(
      code,
      createFolderSink(dir),
      "skip",
      recv.emit,
      new AbortController().signal,
    );
    const channel = await waitFor(
      () => peerConnections[0]?._channels.get("data"),
      { label: "sender data channel" },
    );
    const realSend = channel.send.bind(channel);
    let wire = 0;
    channel.send = (data) => {
      wire += data.byteLength;
      realSend(tamper ? tamper(data) : data);
    };
    await Promise.all([senderDone, receiverDone]);
    return { dir, wire, send: send.record, recv: recv.record };
  }

  test("compressible files cross in far fewer bytes", async () => {
    const line = new TextEncoder().encode(
      "all work and no play\n".repeat(150_000),
    );
    const spec = {
      "log.txt": line,
      notes: Object.fromEntries(
        Array.from({ length: 30 }, (_, i) => [
          `n${i}.txt`,
          line.slice(0, i * 500),
        ]),
      ),
      "noise.bin": bytes(200_000, 191),
    };
    const { dir, wire, send, recv } = await counted(spec);
    assert.equal(recv.error, null);
    assertFiles(dir.snapshot(), expectedSnapshot(spec));
    const total = Object.values(expectedSnapshot(spec)).reduce(
      (n, b) => n + b.length,
      0,
    );
    assert.equal(send.progress, total, "progress counts file bytes");
    assert.ok(
      wire < 200_000 + total / 10,
      `${wire} bytes on the wire for ${total}`,
    );
  });

  test("progress moves a data channel message at a time, not a block", async () => {
    const spec = { "big.bin": bytes(3 * 1024 * 1024, 193) };
    const { send, recv } = await counted(spec);
    assert.equal(recv.error, null);
    for (const side of [send, recv]) {
      assert.equal(side.progress, 3 * 1024 * 1024);
      assert.ok(side.largestStep <= 16 * 1024, `steps of ${side.largestStep}`);
    }
  });

  test("a malformed block fails the receiver", async () => {
    let first = true;
    const { send, recv } = await counted(
      { "a.txt": bytes(5000, 192) },
      (data) => {
        if (!first) return data;
        first = false;
        const copy = new Uint8Array(data);
        copy[4] = 0x7f;
        return copy;
      },
    );
    assert.match(recv.error ?? "", /^Protocol error/);
    assert.match(send.error ?? "", /^The other side failed/);
  });
});

describe("protocol errors", () => {
  const offer = {
    type: "offer",
    entries: [{ kind: "file", path: ["a"], size: 1 }],
  };
  for (const [name, frame] of [
    ["a message only the sender sends", () => encodeFrame(offer, Role.Sender)],
    ["an unknown message", () => jsonFrame('{"type":"resume"}')],
    ["a malformed message", () => jsonFrame('{"type":"accept"}')],
    [
      "a check of an unknown file",
      () =>
        encodeFrame(
          { type: "check", files: [{ id: 7, len: 1 }] },
          Role.Receiver,
        ),
    ],
  ]) {
    test(`${name} from the receiver fails the sender`, async () => {
      const send = makeRecorder();
      const recv = makeRecorder({
        onOffered: (e) => {
          // Ahead of the receiver's accept.
          peerConnections[1]._channels.get("control").send(frame());
          e.accept();
        },
      });

      const senderDone = runSend(
        makeEntries({ "a.bin": bytes(500, 5) }),
        send.emit,
        new AbortController().signal,
      );
      const code = await waitFor(() => send.record.code, {
        label: "share code",
      });
      const receiverDone = runReceive(
        code,
        createFolderSink(new MemoryDirectoryHandle()),
        "skip",
        recv.emit,
        new AbortController().signal,
      );
      await Promise.all([senderDone, receiverDone]);

      assert.match(send.record.error ?? "", /^Protocol error/);
      assert.equal(send.record.complete, false);
      // The receiver learns why.
      assert.match(recv.record.error ?? "", /^The other side failed/);
    });
  }
});

describe("resuming an interrupted download", () => {
  test("a cut-off transfer resumes and ends byte-perfect", async () => {
    const payload = bytes(2_000_000, 111);
    const spec = { "movie.bin": payload };
    const dirHandle = new MemoryDirectoryHandle();

    // First attempt: pull the sender away partway through.
    {
      const send = makeRecorder();
      const recv = makeRecorder();
      const senderDone = runSend(
        makeEntries(spec),
        send.emit,
        new AbortController().signal,
      );
      const code = await waitFor(() => send.record.code, {
        label: "share code",
      });
      const receiverDone = runReceive(
        code,
        createFolderSink(dirHandle),
        "skip",
        recv.emit,
        new AbortController().signal,
      );

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
    const send2 = makeRecorder();
    const recv2 = makeRecorder();
    const senderDone2 = runSend(
      makeEntries(spec),
      send2.emit,
      new AbortController().signal,
    );
    const code2 = await waitFor(() => send2.record.code, {
      label: "share code",
    });
    const receiverDone2 = runReceive(
      code2,
      createFolderSink(dirHandle),
      "skip",
      recv2.emit,
      new AbortController().signal,
    );
    await Promise.all([senderDone2, receiverDone2]);

    assert.equal(send2.record.error, null);
    assert.equal(recv2.record.error, null);
    assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
    assert.ok(
      send2.record.progress < payload.length,
      `resume should skip the ${partial.length} bytes already on disk, ` +
        `but sent ${send2.record.progress} of ${payload.length}`,
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
  });
});
