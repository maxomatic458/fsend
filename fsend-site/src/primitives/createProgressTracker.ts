import { createStore, produce } from "solid-js/store";
import { TransferStats } from "../lib/stats";
import type { TransferEntry } from "../lib/transfer/events";

interface ProgressEntry {
  name: string;
  sizeBytes: number;
  transferredBytes: number;
  isDir: boolean;
}

export interface ProgressStore {
  entries: ProgressEntry[];
  totalSizeBytes: number;
  totalTransferredBytes: number;
  speedBytesPerSec: number;
  etaSecs: number;
  startTimeMs: number;
  endTimeMs: number;
  skippedBytes: number;
}

export function createProgressTracker() {
  const [progress, setProgress] = createStore<ProgressStore>({
    entries: [],
    totalSizeBytes: 0,
    totalTransferredBytes: 0,
    speedBytesPerSec: 0,
    etaSecs: 0,
    startTimeMs: 0,
    endTimeMs: 0,
    skippedBytes: 0,
  });

  const stats = new TransferStats();
  let statsInterval: ReturnType<typeof setInterval> | undefined;

  /** Stops the sampling interval. Safe to call when none is running. */
  function stopClock() {
    if (statsInterval) clearInterval(statsInterval);
    statsInterval = undefined;
  }

  function initialize(items: TransferEntry[]) {
    // A failed attempt never reaches complete(); stop its clock.
    stopClock();

    const entries = items.map((item) => ({
      name: item.name,
      sizeBytes: item.sizeBytes,
      transferredBytes: item.skipBytes,
      isDir: item.isDir,
    }));
    const totalSizeBytes = items.reduce((sum, item) => sum + item.sizeBytes, 0);
    const skippedBytes = items.reduce((sum, item) => sum + item.skipBytes, 0);

    setProgress({
      entries,
      totalSizeBytes,
      totalTransferredBytes: skippedBytes,
      speedBytesPerSec: 0,
      etaSecs: 0,
      startTimeMs: performance.now(),
      endTimeMs: 0,
      skippedBytes,
    });

    stats.reset();
    stats.record(skippedBytes);

    // Update speed/eta periodically
    statsInterval = setInterval(() => {
      stats.record(progress.totalTransferredBytes);
      const remainingBytes =
        progress.totalSizeBytes - progress.totalTransferredBytes;
      setProgress(
        produce((p) => {
          p.speedBytesPerSec = stats.bytesPerSec;
          p.etaSecs = stats.etaSecs(remainingBytes);
        }),
      );
    }, 500);
  }

  /** `bytes` more of the entry at `index` arrived. */
  function recordBytes(index: number, bytes: number) {
    setProgress(
      produce((p) => {
        const entry = p.entries[index];
        if (!entry) return;
        entry.transferredBytes += bytes;
        p.totalTransferredBytes += bytes;
      }),
    );
  }

  /** Stops the clock, freezing the numbers the summary is computed from. */
  function complete() {
    stopClock();
    setProgress("endTimeMs", performance.now());
  }

  return { progress, initialize, recordBytes, complete, cleanup: stopClock };
}
