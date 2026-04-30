interface Sample {
  time: number;
  bytes: number;
}

export class TransferStats {
  private samples: Sample[] = [];
  private windowMs: number;

  constructor(windowMs = 3000) {
    this.windowMs = windowMs;
  }

  record(totalBytes: number): void {
    const now = performance.now();
    this.samples.push({ time: now, bytes: totalBytes });
    // Trim old samples outside window
    const cutoff = now - this.windowMs;
    while (this.samples.length > 1 && this.samples[0].time < cutoff) {
      this.samples.shift();
    }
  }

  get speed(): number {
    if (this.samples.length < 2) return 0;
    const first = this.samples[0];
    const last = this.samples[this.samples.length - 1];
    const dt = (last.time - first.time) / 1000;
    if (dt <= 0) return 0;
    return (last.bytes - first.bytes) / dt;
  }

  eta(remainingBytes: number): number {
    const s = this.speed;
    if (s <= 0) return Infinity;
    return remainingBytes / s;
  }

  reset(): void {
    this.samples = [];
  }
}
