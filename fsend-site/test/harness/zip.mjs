import assert from "node:assert/strict";
import { crc32 } from "node:zlib";

/**
 * The files of a stored (uncompressed) zip, as `{ "path/to/file": Uint8Array }`;
 * folders are left out. Checks each file's CRC-32 and that its local header
 * matches the central directory.
 */
export async function unzip(blob) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const view = new DataView(bytes.buffer);
  let end = bytes.length - 22;
  while (view.getUint32(end, true) !== 0x06054b50) end--;
  const count = view.getUint16(end + 10, true);
  let at = view.getUint32(end + 16, true);
  const files = {};
  for (let i = 0; i < count; i++) {
    assert.equal(view.getUint32(at, true), 0x02014b50, "central header");
    const crc = view.getUint32(at + 16, true);
    const size = view.getUint32(at + 20, true);
    const nameLen = view.getUint16(at + 28, true);
    const skip =
      nameLen + view.getUint16(at + 30, true) + view.getUint16(at + 32, true);
    const local = view.getUint32(at + 42, true);
    const name = new TextDecoder().decode(
      bytes.subarray(at + 46, at + 46 + nameLen),
    );
    assert.equal(
      view.getUint32(local, true),
      0x04034b50,
      `local header of ${name}`,
    );
    assert.equal(view.getUint32(local + 14, true), crc, `local CRC of ${name}`);
    assert.equal(
      view.getUint32(local + 22, true),
      size,
      `local size of ${name}`,
    );
    const data =
      local +
      30 +
      view.getUint16(local + 26, true) +
      view.getUint16(local + 28, true);
    if (!name.endsWith("/")) {
      const content = bytes.slice(data, data + size);
      assert.equal(crc32(content), crc, `CRC-32 of ${name}`);
      files[name] = content;
    }
    at += 46 + skip;
  }
  return files;
}
