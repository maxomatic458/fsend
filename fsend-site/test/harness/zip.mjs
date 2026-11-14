import assert from "node:assert/strict";
/** The files of a stored (uncompressed) zip, as `{ "path/to/file": Uint8Array }`; folders are left out. */
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
    const size = view.getUint32(at + 20, true);
    const nameLen = view.getUint16(at + 28, true);
    const skip =
      nameLen + view.getUint16(at + 30, true) + view.getUint16(at + 32, true);
    const local = view.getUint32(at + 42, true);
    const name = new TextDecoder().decode(
      bytes.subarray(at + 46, at + 46 + nameLen),
    );
    const data =
      local +
      30 +
      view.getUint16(local + 26, true) +
      view.getUint16(local + 28, true);
    if (!name.endsWith("/")) files[name] = bytes.slice(data, data + size);
    at += 46 + skip;
  }
  return files;
}
