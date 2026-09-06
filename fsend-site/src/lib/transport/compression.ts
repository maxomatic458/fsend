import { MAX_MESSAGE_BYTES, FRAME_HEADER_BYTES } from "../../config";

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

/// One control frame: a big-endian u32 length, then the payload.
export function frame(payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(FRAME_HEADER_BYTES + payload.length);
  new DataView(out.buffer).setUint32(0, payload.length);
  out.set(payload, FRAME_HEADER_BYTES);
  return out;
}

/// Slices a byte stream into data channel messages.
export function messages(bytes: Uint8Array): Uint8Array[] {
  const out: Uint8Array[] = [];
  for (let offset = 0; offset < bytes.length; offset += MAX_MESSAGE_BYTES) {
    out.push(bytes.subarray(offset, offset + MAX_MESSAGE_BYTES));
  }
  return out;
}

/// Reassembles frames from the messages of the control channel. A message
/// may end mid-frame or carry the start of the next one.
export class FrameDecoder {
  private buffered: Uint8Array = new Uint8Array(0);

  push(chunk: Uint8Array): Uint8Array[] {
    this.buffered = concat([this.buffered, chunk]);
    const frames: Uint8Array[] = [];
    while (this.buffered.length >= FRAME_HEADER_BYTES) {
      const view = new DataView(
        this.buffered.buffer,
        this.buffered.byteOffset,
        this.buffered.byteLength,
      );
      const length = view.getUint32(0);
      const end = FRAME_HEADER_BYTES + length;
      if (this.buffered.length < end) break;
      frames.push(this.buffered.slice(FRAME_HEADER_BYTES, end));
      this.buffered = this.buffered.slice(end);
    }
    return frames;
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
