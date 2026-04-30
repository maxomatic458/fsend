import { createStore, produce } from 'solid-js/store';
import { TransferStats } from '../lib/stats';

export interface ProgressEntry {
  name: string;
  size: number;
  transferred: number;
  isDir: boolean;
}

export interface ProgressStore {
  entries: ProgressEntry[];
  totalSize: number;
  totalTransferred: number;
  speed: number;
  eta: number;
  startTime: number;
}

export function createProgressTracker() {
  const [progress, setProgress] = createStore<ProgressStore>({
    entries: [],
    totalSize: 0,
    totalTransferred: 0,
    speed: 0,
    eta: 0,
    startTime: 0,
  });

  const stats = new TransferStats();
  let currentEntryIdx = 0;
  let statsInterval: ReturnType<typeof setInterval> | undefined;

  function initialize(items: Array<{ name: string; size: number; skip: number; isDir: boolean }>) {
    const entries = items.map((item) => ({
      name: item.name,
      size: item.size,
      transferred: item.skip,
      isDir: item.isDir,
    }));
    const totalSize = items.reduce((sum, item) => sum + item.size, 0);
    const totalSkipped = items.reduce((sum, item) => sum + item.skip, 0);

    setProgress({
      entries,
      totalSize,
      totalTransferred: totalSkipped,
      speed: 0,
      eta: 0,
      startTime: performance.now(),
    });

    currentEntryIdx = 0;
    // Find the first non-complete entry
    while (currentEntryIdx < entries.length && entries[currentEntryIdx].transferred >= entries[currentEntryIdx].size) {
      currentEntryIdx++;
    }

    stats.reset();
    stats.record(totalSkipped);

    // Update speed/eta periodically
    statsInterval = setInterval(() => {
      stats.record(progress.totalTransferred);
      const remaining = progress.totalSize - progress.totalTransferred;
      setProgress(
        produce((p) => {
          p.speed = stats.speed;
          p.eta = stats.eta(remaining);
        }),
      );
    }, 500);
  }

  function recordBytes(n: number) {
    setProgress(
      produce((p) => {
        p.totalTransferred += n;

        // Distribute bytes to current entry
        if (currentEntryIdx < p.entries.length) {
          p.entries[currentEntryIdx].transferred += n;

          // Advance to next entry if current is complete
          while (
            currentEntryIdx < p.entries.length &&
            p.entries[currentEntryIdx].transferred >= p.entries[currentEntryIdx].size
          ) {
            currentEntryIdx++;
          }
        }
      }),
    );
  }

  function cleanup() {
    if (statsInterval) clearInterval(statsInterval);
  }

  return { progress, initialize, recordBytes, cleanup };
}
