import assert from "node:assert/strict";
import { crc32 } from "node:zlib";

const MAX_U32 = 0xffffffff;

const view = (bytes) => new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

/** The zip64 extra field's values, in the order the zip format lists them. */
function zip64Extra(bytes, from, to) {
    const at = view(bytes);
    for (let i = from; i + 4 <= to; i += 4 + at.getUint16(i + 2, true)) {
        if (at.getUint16(i, true) !== 0x0001) continue;
        const values = [];
        for (let j = i + 4; j + 8 <= i + 4 + at.getUint16(i + 2, true); j += 8) {
            values.push(Number(at.getBigUint64(j, true)));
        }
        return values;
    }
    return [];
}

/**
 * The files of a stored (uncompressed) zip of `size` bytes, read through
 * `read(offset, length)`, as `{ name, crc, size, start }`; folders are left
 * out. Checks that each local header matches the central directory. Zip64
 * is understood, so the zip may hold any number of files of any size.
 */
export async function zipEntries(read, size) {
    const tailLength = Math.min(size, 22 + 0xffff);
    const tail = await read(size - tailLength, tailLength);
    const tailView = view(tail);
    let end = tail.length - 22;
    while (tailView.getUint32(end, true) !== 0x06054b50) end--;
    let count = tailView.getUint16(end + 10, true);
    let dirSize = tailView.getUint32(end + 12, true);
    let dirOffset = tailView.getUint32(end + 16, true);
    if (end >= 20 && tailView.getUint32(end - 20, true) === 0x07064b50) {
        const record = await read(Number(tailView.getBigUint64(end - 12, true)), 56);
        const at = view(record);
        assert.equal(at.getUint32(0, true), 0x06064b50, "zip64 end record");
        count = Number(at.getBigUint64(32, true));
        dirSize = Number(at.getBigUint64(40, true));
        dirOffset = Number(at.getBigUint64(48, true));
    }

    const dir = await read(dirOffset, dirSize);
    const at = view(dir);
    const entries = [];
    for (let i = 0, p = 0; i < count; i++) {
        assert.equal(at.getUint32(p, true), 0x02014b50, "central header");
        assert.equal(at.getUint16(p + 10, true), 0, "stored");
        const crc = at.getUint32(p + 16, true);
        const nameLen = at.getUint16(p + 28, true);
        const extraLen = at.getUint16(p + 30, true);
        const name = new TextDecoder().decode(dir.subarray(p + 46, p + 46 + nameLen));
        const extra = zip64Extra(dir, p + 46 + nameLen, p + 46 + nameLen + extraLen);
        let fileSize = at.getUint32(p + 24, true);
        let local = at.getUint32(p + 42, true);
        if (fileSize === MAX_U32) fileSize = extra.shift();
        if (at.getUint32(p + 20, true) === MAX_U32) extra.shift();
        if (local === MAX_U32) local = extra.shift();
        p += 46 + nameLen + extraLen + at.getUint16(p + 32, true);

        const header = await read(local, 30);
        const h = view(header);
        assert.equal(h.getUint32(0, true), 0x04034b50, `local header of ${name}`);
        assert.equal(h.getUint32(14, true), crc, `local CRC of ${name}`);
        const localNameLen = h.getUint16(26, true);
        const localExtraLen = h.getUint16(28, true);
        let localSize = h.getUint32(22, true);
        if (localSize === MAX_U32) {
            const localExtra = await read(local + 30 + localNameLen, localExtraLen);
            [localSize] = zip64Extra(localExtra, 0, localExtraLen);
        }
        assert.equal(localSize, fileSize, `local size of ${name}`);
        if (!name.endsWith("/")) {
            const start = local + 30 + localNameLen + localExtraLen;
            entries.push({ name, crc, size: fileSize, start });
        }
    }
    return entries;
}

/**
 * The files of a stored zip, as `{ "path/to/file": Uint8Array }`; folders are
 * left out. Checks each file's CRC-32 and its headers.
 */
export async function unzip(blob) {
    const bytes = new Uint8Array(await blob.arrayBuffer());
    const read = async (at, length) => bytes.subarray(at, at + length);
    const files = {};
    for (const { name, crc, size, start } of await zipEntries(read, blob.size)) {
        const content = bytes.slice(start, start + size);
        assert.equal(crc32(content), crc, `CRC-32 of ${name}`);
        files[name] = content;
    }
    return files;
}
