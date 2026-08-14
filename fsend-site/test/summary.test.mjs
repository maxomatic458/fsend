import { test, describe } from "node:test";
import assert from "node:assert/strict";

const { summarize, createProgressTracker, createRoot } =
  await import("./.build/app.mjs");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

describe("finished transfer summary", () => {
  test("the mean rate covers the whole transfer", () => {
    const { elapsedSecs, averageBytesPerSec } = summarize({
      startTimeMs: 1000,
      endTimeMs: 5000,
      totalTransferredBytes: 8_000_000,
      skippedBytes: 0,
    });
    assert.equal(elapsedSecs, 4);
    assert.equal(averageBytesPerSec, 2_000_000, "8 MB over 4s is 2 MB/s");
  });

  test("bytes already on disk do not inflate a resumed transfer", () => {
    const { averageBytesPerSec } = summarize({
      startTimeMs: 0,
      endTimeMs: 2000,
      totalTransferredBytes: 3_000_000,
      skippedBytes: 1_000_000,
    });
    assert.equal(
      averageBytesPerSec,
      1_000_000,
      "only the 2 MB that crossed the wire counts",
    );
  });

  test("a transfer too brief to time reports no rate", () => {
    const { elapsedSecs, averageBytesPerSec } = summarize({
      startTimeMs: 1234,
      endTimeMs: 1234,
      totalTransferredBytes: 5000,
      skippedBytes: 0,
    });
    assert.equal(elapsedSecs, 0);
    assert.equal(averageBytesPerSec, 0, "must not divide by zero");
  });

  test("the clock stops when the transfer completes", async () => {
    await createRoot(async (dispose) => {
      const tracker = createProgressTracker();
      tracker.initialize([
        { name: "a.bin", sizeBytes: 2000, skipBytes: 500, isDir: false },
      ]);
      tracker.recordBytes(1500);

      await sleep(60);
      tracker.complete();
      const frozen = summarize(tracker.progress);
      assert.ok(frozen.elapsedSecs > 0, "elapsed must be measured");
      assert.equal(
        frozen.averageBytesPerSec,
        1500 / frozen.elapsedSecs,
        "the mean covers the 1500 bytes received, not the 500 skipped",
      );

      await sleep(60);
      assert.equal(
        summarize(tracker.progress).elapsedSecs,
        frozen.elapsedSecs,
        "the summary must not keep growing after completion",
      );
      tracker.cleanup();
      dispose();
    });
  });

  test("restarting leaves no clock from the previous attempt", async () => {
    await createRoot(async (dispose) => {
      const tracker = createProgressTracker();
      const entries = [
        { name: "a.bin", sizeBytes: 4000, skipBytes: 0, isDir: false },
      ];

      // A cancelled or failed attempt never calls complete(), so its clock is
      // still running when the retry starts.
      tracker.initialize(entries);
      tracker.recordBytes(1000);
      tracker.initialize(entries);

      tracker.complete();
      const frozen = { ...tracker.progress };

      await sleep(700); // longer than the 500ms sampling interval
      assert.equal(
        tracker.progress.speedBytesPerSec,
        frozen.speedBytesPerSec,
        "an orphaned interval from the first attempt must not keep writing",
      );
      assert.equal(tracker.progress.etaSecs, frozen.etaSecs);

      tracker.cleanup();
      dispose();
    });
  });
});
