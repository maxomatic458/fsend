import { CHUNK_SIZE, MAX_DC_PAYLOAD, FRAG_MORE, FRAG_LAST } from "../config";

export async function compressGzip(data: Uint8Array): Promise<Uint8Array> {
  const cs = new CompressionStream("gzip");
  const writer = cs.writable.getWriter();
  writer.write(data as any);
  writer.close();

  const chunks: Uint8Array[] = [];
  const reader = cs.readable.getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(new Uint8Array(value));
  }
  return concat(chunks);
}

export async function decompressGzip(data: Uint8Array): Promise<Uint8Array> {
  const ds = new DecompressionStream("gzip");
  const writer = ds.writable.getWriter();
  writer.write(data as any);
  writer.close();

  const chunks: Uint8Array[] = [];
  const reader = ds.readable.getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
  }
  return concat(chunks);
}

export function fragment(data: Uint8Array): Uint8Array[] {
  const fragments: Uint8Array[] = [];
  let offset = 0;
  while (offset < data.length) {
    const end = Math.min(offset + MAX_DC_PAYLOAD, data.length);
    const isLast = end >= data.length;
    const frag = new Uint8Array(1 + (end - offset));
    frag[0] = isLast ? FRAG_LAST : FRAG_MORE;
    frag.set(data.subarray(offset, end), 1);
    fragments.push(frag);
    offset = end;
  }
  if (fragments.length === 0) {
    fragments.push(new Uint8Array([FRAG_LAST]));
  }
  return fragments;
}

export class Defragmenter {
  private chunks: Uint8Array[] = [];

  push(chunk: Uint8Array): Uint8Array | null {
    if (chunk.length === 0) return null;
    const header = chunk[0];
    const payload = chunk.subarray(1);
    this.chunks.push(payload);
    if (header === FRAG_LAST) {
      const result = concat(this.chunks);
      this.chunks = [];
      return result;
    }
    return null;
  }
}

function concat(arrays: Uint8Array[]): Uint8Array {
  let totalLen = 0;
  for (const a of arrays) totalLen += a.length;
  const result = new Uint8Array(totalLen);
  let offset = 0;
  for (const a of arrays) {
    result.set(a, offset);
    offset += a.length;
  }
  return result;
}
