class NotFoundError extends Error {
  constructor(name) {
    super(`NotFoundError: ${name}`);
    this.name = "NotFoundError";
  }
}

/** What browsers throw for an entry of the other kind. */
class TypeMismatchError extends Error {
  constructor(name) {
    super(`TypeMismatchError: ${name}`);
    this.name = "TypeMismatchError";
  }
}

/** Bytes writes may still add, as a browser's real limit; a test may lower it. */
export const storageLimit = { left: Infinity };

/** How long reopening a partial file takes, as browsers copy it first; and when one last finished. */
export const reopening = { ms: 0, doneAt: 0 };

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
    if (keepExistingData && reopening.ms > 0) {
      await new Promise((resolve) => setTimeout(resolve, reopening.ms));
      reopening.doneAt = Date.now();
    }
    return new MemoryWritable(this, keepExistingData);
  }
}

class MemoryWritable {
  constructor(handle, keepExistingData) {
    this._handle = handle;
    this._pos = 0;
    this._closed = false;
    this._buf = keepExistingData ? Array.from(handle.bytes) : [];
  }

  async seek(pos) {
    this._pos = pos;
  }

  async write(chunk) {
    if (this._closed) throw new Error("write after close");
    const data =
      chunk instanceof Uint8Array
        ? chunk
        : chunk instanceof ArrayBuffer
          ? new Uint8Array(chunk)
          : new Uint8Array(chunk.data ?? chunk);
    if ((storageLimit.left -= data.length) < 0) {
      throw new DOMException(
        "The operation failed because it would cause the application to exceed its storage quota.",
        "QuotaExceededError",
      );
    }
    for (let i = 0; i < data.length; i++) this._buf[this._pos + i] = data[i];
    this._pos += data.length;
  }

  /** Like a browser's, only this commits what was written. */
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
      if (existing.kind !== "directory") throw new TypeMismatchError(name);
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
      if (existing.kind !== "file") throw new TypeMismatchError(name);
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
