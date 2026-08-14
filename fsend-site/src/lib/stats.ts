export interface FinishedTransfer {
  startTimeMs: number;
  // 0 when transfer is in progress
  endTimeMs: number;
  totalTransferredBytes: number;
  skippedBytes: number;
}

export interface TransferSummary {
  elapsedSecs: number;
  averageBytesPerSec: number;
}

// headline numbers for a completed transfer
export function summarize(transfer: FinishedTransfer): TransferSummary {
  const elapsedSecs = Math.max(
    0,
    (transfer.endTimeMs - transfer.startTimeMs) / 1000,
  );
  const movedBytes = transfer.totalTransferredBytes - transfer.skippedBytes;
  return {
    elapsedSecs,
    averageBytesPerSec: elapsedSecs > 0 ? movedBytes / elapsedSecs : 0,
  };
}

interface Sample {
  timeMs: number;
  totalBytes: number;
}

export class TransferStats {
  private samples: Sample[] = [];
  private windowMs: number;

  constructor(windowMs = 3000) {
    this.windowMs = windowMs;
  }

  record(totalBytes: number): void {
    const nowMs = performance.now();
    this.samples.push({ timeMs: nowMs, totalBytes });
    // Trim old samples outside window
    const cutoffMs = nowMs - this.windowMs;
    while (this.samples.length > 1 && this.samples[0].timeMs < cutoffMs) {
      this.samples.shift();
    }
  }

  get bytesPerSec(): number {
    if (this.samples.length < 2) return 0;
    const first = this.samples[0];
    const last = this.samples[this.samples.length - 1];
    const spanSecs = (last.timeMs - first.timeMs) / 1000;
    if (spanSecs <= 0) return 0;
    return (last.totalBytes - first.totalBytes) / spanSecs;
  }

  etaSecs(remainingBytes: number): number {
    const rate = this.bytesPerSec;
    if (rate <= 0) return Infinity;
    return remainingBytes / rate;
  }

  reset(): void {
    this.samples = [];
  }
}
