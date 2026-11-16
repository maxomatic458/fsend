import { crc32, type Entry, type TopEntry } from "../proto";

/** A file of a zip, or a folder without `data`. */
export interface ZipEntry {
  /** Its path, parts joined with `/`. */
  name: string;
  data?: Blob;
  crc?: number;
}

/** The name a transfer of several files is saved under. */
export const zipName = (tops: TopEntry[]): string =>
  tops.length === 1 ? `${tops[0].name}.zip` : "fsend-files.zip";

const MAX_U16 = 0xffff;
const MAX_U32 = 0xffffffff;
const UTF8_NAMES = 0x0800;
/** Made by version 4.5 (zip64) on Unix, so the modes below apply. */
const MADE_BY = (3 << 8) | 45;
const FILE_MODE = 0o100644 * 0x10000;
const DIR_MODE = 0o40755 * 0x10000 + 0x10;

/** Little-endian fields, written in order. */
class Fields {
  private bytes: number[] = [];

  u16(value: number): this {
    this.bytes.push(value & 0xff, (value >>> 8) & 0xff);
    return this;
  }

  u32(value: number): this {
    const v = value >>> 0;
    this.bytes.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, v >>> 24);
    return this;
  }

  u64(value: number): this {
    return this.u32(value % 2 ** 32).u32(Math.floor(value / 2 ** 32));
  }

  raw(data: Uint8Array): this {
    for (const byte of data) this.bytes.push(byte);
    return this;
  }

  done(): Uint8Array<ArrayBuffer> {
    return Uint8Array.from(this.bytes);
  }
}

/** A DOS time and date, as zip headers record them. */
function dos(at: Date): [number, number] {
  return [
    (at.getHours() << 11) | (at.getMinutes() << 5) | (at.getSeconds() >> 1),
    ((at.getFullYear() - 1980) << 9) |
      ((at.getMonth() + 1) << 5) |
      at.getDate(),
  ];
}

/**
 * A stored (uncompressed) zip of `entries`, made of their blobs without
 * copying them: only the headers are new bytes. Files need their `crc`.
 */
export function zipBlob(entries: ZipEntry[], modified = new Date()): Blob {
  const [time, date] = dos(modified);
  const encoder = new TextEncoder();
  const parts: BlobPart[] = [];
  const central = new Fields();
  let offset = 0;
  for (const entry of entries) {
    const folder = entry.data === undefined;
    const name = encoder.encode(folder ? `${entry.name}/` : entry.name);
    const size = entry.data?.size ?? 0;
    const crc = entry.crc ?? 0;
    // Zip64 extra fields carry what does not fit in 32 bits.
    const big = size >= MAX_U32;
    const far = offset >= MAX_U32;
    const version = big || far ? 45 : 20;

    const local = new Fields()
      .u32(0x04034b50)
      .u16(version)
      .u16(UTF8_NAMES)
      .u16(0)
      .u16(time)
      .u16(date)
      .u32(crc)
      .u32(big ? MAX_U32 : size)
      .u32(big ? MAX_U32 : size)
      .u16(name.length)
      .u16(big ? 20 : 0)
      .raw(name);
    if (big) local.u16(0x0001).u16(16).u64(size).u64(size);
    const header = local.done();
    parts.push(header);
    if (entry.data) parts.push(entry.data);

    const extra = (big ? 16 : 0) + (far ? 8 : 0);
    central
      .u32(0x02014b50)
      .u16(MADE_BY)
      .u16(version)
      .u16(UTF8_NAMES)
      .u16(0)
      .u16(time)
      .u16(date)
      .u32(crc)
      .u32(big ? MAX_U32 : size)
      .u32(big ? MAX_U32 : size)
      .u16(name.length)
      .u16(extra > 0 ? extra + 4 : 0)
      .u16(0)
      .u16(0)
      .u16(0)
      .u32(folder ? DIR_MODE : FILE_MODE)
      .u32(far ? MAX_U32 : offset)
      .raw(name);
    if (extra > 0) {
      central.u16(0x0001).u16(extra);
      if (big) central.u64(size).u64(size);
      if (far) central.u64(offset);
    }
    offset += header.length + size;
  }

  const directory = central.done();
  const count = entries.length;
  const end = new Fields();
  if (count >= MAX_U16 || offset >= MAX_U32 || directory.length >= MAX_U32) {
    end
      .u32(0x06064b50)
      .u64(44)
      .u16(MADE_BY)
      .u16(45)
      .u32(0)
      .u32(0)
      .u64(count)
      .u64(count)
      .u64(directory.length)
      .u64(offset)
      .u32(0x07064b50)
      .u32(0)
      .u64(offset + directory.length)
      .u32(1);
  }
  end
    .u32(0x06054b50)
    .u16(0)
    .u16(0)
    .u16(Math.min(count, MAX_U16))
    .u16(Math.min(count, MAX_U16))
    .u32(Math.min(directory.length, MAX_U32))
    .u32(Math.min(offset, MAX_U32))
    .u16(0);
  return new Blob([...parts, directory, end.done()], {
    type: "application/zip",
  });
}

/** The CRC-32 of `blob`, read a chunk at a time. */
async function crcOf(blob: Blob): Promise<number> {
  let crc = 0;
  const reader = blob.stream().getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) return crc;
    crc = crc32(value, crc);
  }
}

/** The CRC-32 of each file written from its start, by path. */
export class Checksums {
  private done = new Map<string, number>();
  private current?: { path: string; crc: number };

  /** A file written whole, from `offset` on. */
  whole(path: string[], offset: number, data: Uint8Array): void {
    if (offset === 0) this.done.set(path.join("/"), crc32(data, 0));
  }

  /** A file written in pieces from `offset` on; one resumed midway gets no checksum here. */
  begin(path: string[], offset: number): void {
    this.current = offset === 0 ? { path: path.join("/"), crc: 0 } : undefined;
  }

  update(data: Uint8Array): void {
    if (this.current) this.current.crc = crc32(data, this.current.crc);
  }

  end(): void {
    if (this.current) this.done.set(this.current.path, this.current.crc);
    this.current = undefined;
  }

  /** Reads the files of `blobs` written without a checksum, such as resumed ones. */
  async fill(
    blobs: Map<string, Blob>,
    onProgress?: (percent: number) => void,
  ): Promise<void> {
    const missing = [...blobs].filter(([name]) => !this.done.has(name));
    const total = missing.reduce((n, [, blob]) => n + blob.size, 0);
    let read = 0;
    for (const [name, blob] of missing) {
      this.done.set(name, await crcOf(blob));
      read += blob.size;
      onProgress?.(total === 0 ? 100 : (read / total) * 100);
    }
  }

  get(name: string): number | undefined {
    return this.done.get(name);
  }
}

/** The zip of an offer's `entries`, given the blob of each file by path. */
export async function zipOffer(
  entries: Entry[],
  blobs: Map<string, Blob>,
  checksums: Checksums,
  onProgress?: (percent: number) => void,
): Promise<Blob> {
  await checksums.fill(blobs, onProgress);
  return zipBlob(
    entries.map((entry) => {
      const name = entry.path.join("/");
      return entry.kind === "dir"
        ? { name }
        : { name, data: blobs.get(name), crc: checksums.get(name) };
    }),
  );
}
