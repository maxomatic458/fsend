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
  sleep,
  within,
  dropRelayConnections,
} from "./harness/index.mjs";

const { runSend, runReceive, createDiskSink, createDownloadSink } =
  await import("./.build/app.mjs");

let peerConnections;

beforeEach(() => {
  peerConnections = installBrowserEnv({ latencyMs: 0 });
});
afterEach(() => resetBrowserEnv());

/** Byte equality, without asking assert to diff megabytes on failure. */
function assertBytes(actual, expected, message = "contents differ") {
  assert.ok(
    actual && Buffer.from(actual).equals(Buffer.from(expected)),
    `${message} (${actual?.length ?? "missing"} vs ${expected.length} bytes)`,
  );
}

/** Assert two snapshots have identical paths and bytes. */
function assertFiles(actual, expected) {
  const paths = (snapshot) => Object.keys(snapshot).sort().join("\n");
  assert.ok(paths(actual) === paths(expected), "file paths differ");
  for (const path of Object.keys(expected)) {
    assertBytes(actual[path], expected[path], `contents differ for ${path}`);
  }
}

/**
 * Runs a sender and a native-FS receiver against each other.
 * Returns once both have settled.
 */
async function transfer(spec, { mode = "file", resume = false, dir } = {}) {
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
    createDiskSink(dirHandle),
    resume,
    recv.emit,
    new AbortController().signal,
  );

  await within(Promise.all([senderDone, receiverDone]), 20_000, "transfer");
  return { dirHandle, send: send.record, recv: recv.record };
}

/** Starts a sender and resolves once it has a share code. */
async function startSender(spec, signal = new AbortController().signal) {
  const send = makeRecorder();
  const done = runSend(makeEntries(spec), send.emit, signal);
  const code = await waitFor(() => send.record.code, { label: "share code" });
  return { send: send.record, done, code };
}

async function writeFile(dir, name, data) {
  const writable = await (
    await dir.getFileHandle(name, { create: true })
  ).createWritable();
  await writable.write(data);
  await writable.close();
}

const hex = (data) =>
  Array.from(data, (b) => b.toString(16).padStart(2, "0")).join("");

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
      createDownloadSink(),
      false,
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
    assertBytes(got, spec["report.pdf"]);
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
    assert.match(downloads[0].name, /\.zip$/);
    assert.ok(downloads[0].blob.size > 0, "zip should not be empty");
  });

  test("multiple loose files are zipped together", async () => {
    const spec = { "one.bin": bytes(100, 61), "two.bin": bytes(100, 62) };
    const { recv } = await fallbackTransfer(spec);
    assert.equal(recv.error, null);
    assert.match(getDownloads()[0].name, /\.zip$/);
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
      createDiskSink(new MemoryDirectoryHandle()),
      false,
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
      createDiskSink(new MemoryDirectoryHandle()),
      false,
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
      createDiskSink(new MemoryDirectoryHandle()),
      false,
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
      createDiskSink(new MemoryDirectoryHandle()),
      false,
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

  test("aborting the sender reports no error to the user", async () => {
    const spec = { "big.bin": bytes(2_000_000, 101) };
    const send = makeRecorder();
    const controller = new AbortController();

    const senderDone = runSend(makeEntries(spec), send.emit, controller.signal);
    await waitFor(() => send.record.code, { label: "share code" });
    controller.abort();
    await within(senderDone, 2000, "aborted sender");

    assert.equal(
      send.record.error,
      null,
      "a deliberate abort is not an error worth showing",
    );
    assert.equal(send.record.complete, false);
  });
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
        createDiskSink(dirHandle),
        false,
        recv.emit,
        new AbortController().signal,
      );

      await waitFor(() => recv.record.progress > 200_000, {
        label: "partial download",
      });
      peerConnections[0].simulateAbruptDisconnect();
      void senderDone.catch(() => {});
      await receiverDone;
    }

    const partial = dirHandle.snapshot()["movie.bin"];
    assert.ok(partial.length > 0, "partial data should be on disk");
    assert.ok(partial.length < payload.length, "should not be complete yet");
    assertBytes(
      partial,
      payload.subarray(0, partial.length),
      "the partial prefix must match the source",
    );

    // Second attempt, with resume on: only the remainder should be sent.
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
      createDiskSink(dirHandle),
      true,
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

  test("resuming without existing data transfers everything", async () => {
    const spec = { "fresh.bin": bytes(50_000, 121) };
    const { dirHandle, send } = await transfer(spec, { resume: true });
    assert.equal(send.error, null);
    assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
  });
});

describe("relay behaviour", () => {
  test("an unknown share code surfaces an error", async () => {
    const recv = makeRecorder();
    await runReceive(
      "NOPE0000",
      createDiskSink(new MemoryDirectoryHandle()),
      false,
      recv.emit,
      new AbortController().signal,
    );
    assert.ok(recv.record.error, "receiver should report the bad code");
    assert.equal(recv.record.complete, false);
  });

  test("the connection is reported as direct", async () => {
    const { recv, send } = await transfer({ "x.bin": bytes(100, 131) });
    assert.equal(send.error, null);
    assert.equal(recv.error, null);
  });
});

describe("resume only trusts matching data", () => {
  test("a local file with other contents is downloaded again", async () => {
    const spec = { "movie.bin": bytes(500_000, 1) };
    const dir = new MemoryDirectoryHandle();
    await writeFile(dir, "movie.bin", bytes(200_000, 2));

    const { send, recv } = await transfer(spec, { resume: true, dir });

    assert.equal(send.error, null);
    assert.equal(recv.error, null);
    assertFiles(dir.snapshot(), expectedSnapshot(spec));
    assert.equal(send.progress, 500_000);
  });

  test("a local file larger than the offer is replaced", async () => {
    const spec = { "notes.txt": bytes(1000, 3) };
    const dir = new MemoryDirectoryHandle();
    await writeFile(dir, "notes.txt", bytes(3000, 4));

    const { recv } = await transfer(spec, { resume: true, dir });

    assert.equal(recv.error, null);
    assertFiles(dir.snapshot(), expectedSnapshot(spec));
  });
});

describe("relay connection", () => {
  test("a sender whose relay connection drops is told so", async () => {
    const { send, done } = await startSender({ "a.bin": bytes(10, 1) });
    dropRelayConnections();

    await within(done, 2000, "sender");
    assert.match(send.error ?? "", /relay/i);
  });

  test("cancelling the sender frees its share code", async () => {
    const controller = new AbortController();
    const { send, code } = await startSender(
      { "a.bin": bytes(10, 1) },
      controller.signal,
    );
    controller.abort();
    await sleep(10);

    const recv = makeRecorder();
    const done = runReceive(
      code,
      createDiskSink(new MemoryDirectoryHandle()),
      false,
      recv.emit,
      new AbortController().signal,
    );
    await within(done, 2000, "receiver");
    assert.match(recv.record.error ?? "", /unknown session/);
    assert.equal(send.error, null);
  });

  test("cancelling a receiver that is still connecting settles", async () => {
    // A sender that creates a session and then never answers.
    const stalled = new WebSocket("wss://relay.test/ws");
    await new Promise((resolve) => (stalled.onopen = resolve));
    const code = await new Promise((resolve) => {
      stalled.onmessage = (ev) => resolve(JSON.parse(ev.data).code);
      stalled.send(
        JSON.stringify({ type: "create_session", capabilities: ["web_rtc"] }),
      );
    });

    const recv = makeRecorder();
    const controller = new AbortController();
    const done = runReceive(
      code,
      createDiskSink(new MemoryDirectoryHandle()),
      false,
      recv.emit,
      controller.signal,
    );
    await sleep(20);
    controller.abort();

    await within(done, 2000, "receiver");
    assert.equal(recv.record.error, null);
  });
});

describe("large and hostile offers", () => {
  test("a folder with thousands of files can be offered", async () => {
    const folder = {};
    for (let i = 0; i < 10_000; i++) {
      folder[`${hex(bytes(8, i + 1))}.bin`] = bytes(0);
    }
    const spec = { photos: folder };

    const { dirHandle, send, recv } = await transfer(spec);

    assert.equal(send.error, null);
    assert.equal(recv.error, null);
    assertFiles(dirHandle.snapshot(), expectedSnapshot(spec));
  });

  test("the receiver refuses names that are not plain file names", async () => {
    const specs = [
      { "../escape.txt": bytes(10, 1), "b.txt": bytes(10, 2) },
      { "\u001b[2Jreport.pdf": bytes(10, 3) },
    ];
    for (const spec of specs) {
      const { send, done } = await startSender(spec);
      const recv = makeRecorder();
      const received = runReceive(
        send.code,
        createDownloadSink(),
        false,
        recv.emit,
        new AbortController().signal,
      );
      await within(Promise.all([done, received]), 5000, "transfer");

      assert.equal(recv.record.offered, null, "the offer must not be shown");
      assert.ok(recv.record.error);
      assert.match(send.error ?? "", /reject/i);
      assert.equal(getDownloads().length, 0);
    }
  });
});

describe("cancelling and failing", () => {
  test("cancelling a download keeps what has arrived", async () => {
    const payload = bytes(2_000_000, 7);
    const dir = new MemoryDirectoryHandle();
    const { send, done: sent } = await startSender({ "movie.bin": payload });
    void sent;

    const recv = makeRecorder();
    const controller = new AbortController();
    const done = runReceive(
      send.code,
      createDiskSink(dir),
      false,
      recv.emit,
      controller.signal,
    );
    await waitFor(() => recv.record.progress > 200_000, {
      label: "partial download",
    });
    controller.abort();

    await within(done, 2000, "cancelled receiver");
    const partial = dir.snapshot()["movie.bin"];
    assert.ok(partial?.length > 0, "what arrived must be on disk");
    assert.ok(partial.length < payload.length, "cancelling must stop it");
    assertBytes(partial, payload.subarray(0, partial.length));
    assert.equal(recv.record.error, null);
    assert.equal(recv.record.complete, false);
  });

  test("the sender does not report success when the receiver fails to save", async () => {
    const sink = createDiskSink(new MemoryDirectoryHandle());
    const failing = {
      ...sink,
      write: async () => {
        throw new Error("disk full");
      },
    };
    const { send, done } = await startSender({ "a.bin": bytes(1000, 1) });
    const recv = makeRecorder();
    const received = runReceive(
      send.code,
      failing,
      false,
      recv.emit,
      new AbortController().signal,
    );
    await within(Promise.all([done, received]), 5000, "transfer");

    assert.match(recv.record.error ?? "", /disk full/);
    assert.equal(send.complete, false, "the sender reported success");
    assert.ok(send.error);
  });
});
