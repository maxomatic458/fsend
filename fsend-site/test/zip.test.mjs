import { test } from "node:test";
import assert from "node:assert/strict";
import { crc32 } from "node:zlib";
import { unzip } from "./harness/zip.mjs";

const { zipBlob } = await import("./.build/app.mjs");

const file = (name, text) => {
  const data = new TextEncoder().encode(text);
  return { name, data: new Blob([data]), crc: crc32(data) };
};

test("folders, empty files and any names", async () => {
  const zip = zipBlob([
    { name: "d" },
    file("d/a.txt", "hello"),
    file("d/empty", ""),
    { name: "d/sub" },
    file("d/sub/über ✓.txt", "unicode"),
  ]);
  const files = await unzip(zip);
  assert.deepEqual(Object.keys(files).sort(), [
    "d/a.txt",
    "d/empty",
    "d/sub/über ✓.txt",
  ]);
  assert.equal(new TextDecoder().decode(files["d/a.txt"]), "hello");
  assert.equal(files["d/empty"].length, 0);
});

test("only the headers are new bytes", async () => {
  const big = new Blob([new Uint8Array(1_000_000)]);
  const zip = zipBlob([{ name: "big.bin", data: big, crc: 0 }]);
  // A local header, a central entry and the end record around the data.
  assert.equal(zip.size, 1_000_000 + 30 + 7 + 46 + 7 + 22);
});

test("zip64 end records once there are too many entries", async () => {
  const entries = Array.from({ length: 70_000 }, (_, i) => ({ name: `d${i}` }));
  const bytes = new Uint8Array(await zipBlob(entries).arrayBuffer());
  const view = new DataView(bytes.buffer);
  // End of central directory, after the zip64 record and its locator.
  const end = bytes.length - 22;
  assert.equal(view.getUint32(end, true), 0x06054b50);
  assert.equal(view.getUint16(end + 10, true), 0xffff);
  const locator = end - 20;
  assert.equal(view.getUint32(locator, true), 0x07064b50);
  const record = Number(view.getBigUint64(locator + 8, true));
  assert.equal(view.getUint32(record, true), 0x06064b50);
  assert.equal(Number(view.getBigUint64(record + 32, true)), 70_000);
});
