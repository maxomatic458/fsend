/**
 * In-memory File System Access API — the subset the receiver uses:
 * getDirectoryHandle/getFileHandle with { create }, createWritable with
 * { keepExistingData }, seek/write/close, getFile and entries.
 *
 * Files live as byte arrays so a test can assert on exact contents, and a
 * writable can be abandoned mid-write to model a transfer that was cut off.
 */

class NotFoundError extends Error {
  constructor(name) {
    super(`NotFoundError: ${name}`);
    this.name = "NotFoundError";
  }
}

export class MemoryFileHandle {
  constructor(name) {
    this.kind = "file";
    this.name = name;
    this.bytes = new Uint8Array(0);
  }

  async getFile() {
    return new File([this.bytes], this.name);
  }

  async createWritable({ keepExistingData = false } = {}) {
    return new MemoryWritable(this, keepExistingData);
  }
}

class MemoryWritable {
  constructor(handle, keepExistingData) {
    this._handle = handle;
    this._pos = 0;
    this._closed = false;
    this._buf = keepExistingData
      ? Array.from(handle.bytes)
      : [];
  }

  async seek(pos) {
    this._pos = pos;
  }

  async truncate(size) {
    this._buf.length = size;
    if (this._pos > size) this._pos = size;
  }

  async write(chunk) {
    if (this._closed) throw new Error("write after close");
    const data =
      chunk instanceof Uint8Array
        ? chunk
        : chunk instanceof ArrayBuffer
          ? new Uint8Array(chunk)
          : new Uint8Array(chunk.data ?? chunk);
    for (let i = 0; i < data.length; i++) this._buf[this._pos + i] = data[i];
    this._pos += data.length;
    // Commit as we go, so an abandoned writable still leaves partial data on
    // "disk" — which is exactly what resume relies on.
    this._handle.bytes = Uint8Array.from(this._buf);
  }

  async close() {
    this._closed = true;
    this._handle.bytes = Uint8Array.from(this._buf);
  }

  async abort() {
    this._closed = true;
  }
}

export class MemoryDirectoryHandle {
  constructor(name = "root") {
    this.kind = "directory";
    this.name = name;
    this._children = new Map();
  }

  async getDirectoryHandle(name, { create = false } = {}) {
    const existing = this._children.get(name);
    if (existing) {
      if (existing.kind !== "directory") throw new NotFoundError(name);
      return existing;
    }
    if (!create) throw new NotFoundError(name);
    const dir = new MemoryDirectoryHandle(name);
    this._children.set(name, dir);
    return dir;
  }

  async getFileHandle(name, { create = false } = {}) {
    const existing = this._children.get(name);
    if (existing) {
      if (existing.kind !== "file") throw new NotFoundError(name);
      return existing;
    }
    if (!create) throw new NotFoundError(name);
    const fh = new MemoryFileHandle(name);
    this._children.set(name, fh);
    return fh;
  }

  async removeEntry(name) {
    this._children.delete(name);
  }

  async *entries() {
    for (const [name, handle] of this._children) yield [name, handle];
  }

  /** Flatten to { "path/to/file": Uint8Array } for assertions. */
  snapshot(prefix = "") {
    const out = {};
    for (const [name, handle] of this._children) {
      const path = prefix ? `${prefix}/${name}` : name;
      if (handle.kind === "file") out[path] = handle.bytes;
      else Object.assign(out, handle.snapshot(path));
    }
    return out;
  }
}
