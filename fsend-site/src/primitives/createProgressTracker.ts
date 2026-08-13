import { createStore, produce } from "solid-js/store";
import { TransferStats } from "../lib/stats";
import type { TransferEntry } from "../lib/transfer/events";

export interface ProgressEntry {
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
  let currentEntryIdx = 0;
  let statsInterval: ReturnType<typeof setInterval> | undefined;

  function initialize(items: TransferEntry[]) {
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

    currentEntryIdx = 0;
    // Find the first non-complete entry
    while (
      currentEntryIdx < entries.length &&
      entries[currentEntryIdx].transferredBytes >=
        entries[currentEntryIdx].sizeBytes
    ) {
      currentEntryIdx++;
    }

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

  function recordBytes(bytes: number) {
    setProgress(
      produce((p) => {
        p.totalTransferredBytes += bytes;

        // Distribute bytes to current entry
        if (currentEntryIdx < p.entries.length) {
          p.entries[currentEntryIdx].transferredBytes += bytes;

          // Advance to next entry if current is complete
          while (
            currentEntryIdx < p.entries.length &&
            p.entries[currentEntryIdx].transferredBytes >=
              p.entries[currentEntryIdx].sizeBytes
          ) {
            currentEntryIdx++;
          }
        }
      }),
    );
  }

  /** Stops the clock, freezing the numbers the summary is computed from. */
  function complete() {
    if (statsInterval) clearInterval(statsInterval);
    statsInterval = undefined;
    setProgress("endTimeMs", performance.now());
  }

  function cleanup() {
    if (statsInterval) clearInterval(statsInterval);
  }

  return { progress, initialize, recordBytes, complete, cleanup };
}
