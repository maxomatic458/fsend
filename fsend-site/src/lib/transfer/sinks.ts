import { CONCURRENT_FILES } from "../../config";
import type { Bytes, Entry, Local, TopEntry } from "../proto";
import { formatBytes } from "../format";
import type { BrowserStorage } from "../files/storage";
import { Checksums, zipName, zipOffer } from "../files/zip";
import { eachAtOnce } from "../parallel";

const STORAGE_ADVICE = "Clear the browser storage, or choose Download as Zip.";

/// Long enough for the browser to have started reading the blob.
const REVOKE_DELAY_MS = 60_000;

/// Bytes are written into this sink
export interface TransferSink {
  /// Called once the offer of `tops` is accepted, before anything is probed.
  prepare(tops: TopEntry[]): Promise<void>;
  /// What is already at the path of `entry`.
  probe(entry: Entry): Promise<Local>;
  /// The file at `path`, which `probe` found.
  read(path: string[]): Promise<Blob>;
  /// Throws unless `remaining` more bytes fit; `onAsk` runs while the user is asked for more room.
  reserve(remaining: number, onAsk: () => void): Promise<void>;
  createDir(path: string[]): Promise<void>;
  /// Writes the rest of a file, from `offset` on, in one go; several may run at once.
  writeFile(path: string[], offset: number, data: Bytes): Promise<void>;
  /// Opens a file to write from `offset` on; at 0 whatever was there is dropped.
  open(path: string[], offset: number): Promise<void>;
  write(chunk: Bytes): Promise<void>;
  closeFile(): Promise<void>;
  // Finish the transfer, by either flushing writes and closing files or by
  // zipping and downloading the received files.
  finish(
    entries: Entry[],
    tops: TopEntry[],
    onProgress?: (percent: number) => void,
  ): Promise<void>;
  abandon(): Promise<void>;
}

/// Writes into a folder: the picked one, or `storage`'s incoming folder,
/// which is saved to Downloads at the end.
export function createFolderSink(
  dirHandle: FileSystemDirectoryHandle,
  storage?: BrowserStorage,
): TransferSink {
  let writable: FileSystemWritableFileStream | null = null;
  // Kept only for a zip of browser storage.
  let checksums: Checksums | undefined;

  // Handles of the directories reached so far, by path.
  const dirs = new Map<string, FileSystemDirectoryHandle>([["", dirHandle]]);
  const dirOf = async (
    path: string[],
    create: boolean,
  ): Promise<FileSystemDirectoryHandle> => {
    const cached = dirs.get(path.join("/"));
    if (cached) return cached;
    const parent = await dirOf(path.slice(0, -1), create);
    const dir = await parent.getDirectoryHandle(path[path.length - 1], {
      create,
    });
    dirs.set(path.join("/"), dir);
    return dir;
  };

  const fileAt = async (path: string[]) => {
    const dir = await dirOf(path.slice(0, -1), false);
    return dir.getFileHandle(path[path.length - 1]);
  };

  // Some browsers, Chromium in private windows for one, report more storage than they allow.
  const writing = async (write: () => Promise<void>) => {
    try {
      await write();
    } catch (err) {
      if (
        storage &&
        err instanceof DOMException &&
        err.name === "QuotaExceededError"
      ) {
        throw new Error(
          `Browser storage ran out, though the browser reported enough room. ${STORAGE_ADVICE}`,
        );
      }
      throw err;
    }
  };

  // What each directory held when first probed, so a path costs no call of its own.
  const listings = new Map<
    string,
    Promise<Map<string, FileSystemHandle> | null>
  >();
  const listingOf = (path: string[]) => {
    const key = path.join("/");
    let listing = listings.get(key);
    if (!listing) {
      listing = (async () => {
        let dir: FileSystemDirectoryHandle;
        try {
          dir = await dirOf(path, false);
        } catch {
          return null;
        }
        const children = new Map<string, FileSystemHandle>();
        for await (const [name, handle] of dir.entries()) {
          children.set(name, handle);
        }
        return children;
      })();
      listings.set(key, listing);
    }
    return listing;
  };

  return {
    async prepare(tops) {
      if (storage && isZip(tops)) checksums = new Checksums();
      await storage?.keepOnly(tops);
    },

    async probe(entry) {
      const parent = entry.path.slice(0, -1);
      const name = entry.path[entry.path.length - 1];
      const handle = (await listingOf(parent))?.get(name);
      if (!handle) return { kind: "missing" };
      const kind = handle.kind === "directory" ? "dir" : "file";
      // Browser storage only holds earlier transfers, so whatever is in the way is stale.
      if (storage && kind !== entry.kind) {
        await (
          await dirOf(parent, false)
        ).removeEntry(name, {
          recursive: true,
        });
        const path = entry.path.join("/");
        for (const key of dirs.keys()) {
          if (key === path || key.startsWith(`${path}/`)) dirs.delete(key);
        }
        return { kind: "missing" };
      }
      if (kind === "dir") return { kind };
      return {
        kind,
        size: (await (handle as FileSystemFileHandle).getFile()).size,
      };
    },

    async read(path) {
      return (await fileAt(path)).getFile();
    },

    async reserve(remaining, onAsk) {
      if (!storage) return;
      let free = await storage.free();
      if (free === undefined || remaining <= free) return;
      onAsk();
      if (await storage.persist()) {
        free = await storage.free();
        if (free === undefined || remaining <= free) return;
      }
      throw new Error(
        `Not enough browser storage: ${formatBytes(remaining)} needed, ${formatBytes(free)} free. ${STORAGE_ADVICE}`,
      );
    },

    async createDir(path) {
      await dirOf(path, true);
    },

    async writeFile(path, offset, data) {
      await writing(async () => {
        const dir = await dirOf(path.slice(0, -1), true);
        const file = await dir.getFileHandle(path[path.length - 1], {
          create: true,
        });
        const writable = await file.createWritable({
          keepExistingData: offset > 0,
        });
        if (offset > 0) await writable.seek(offset);
        await writable.write(data);
        await writable.close();
      });
      checksums?.whole(path, offset, data);
    },

    async open(path, offset) {
      await writing(async () => {
        const dir = await dirOf(path.slice(0, -1), true);
        const file = await dir.getFileHandle(path[path.length - 1], {
          create: true,
        });
        writable = await file.createWritable({ keepExistingData: offset > 0 });
        if (offset > 0) await writable.seek(offset);
      });
      checksums?.begin(path, offset);
    },

    async write(chunk) {
      await writing(() => writable!.write(chunk));
      checksums?.update(chunk);
    },

    async closeFile() {
      if (!writable) return;
      await writing(() => writable!.close());
      writable = null;
      checksums?.end();
    },

    async finish(entries, tops, onProgress) {
      if (!storage) return;
      if (!checksums) {
        const file = await (await fileAt([tops[0].name])).getFile();
        triggerDownload(file, file.name);
        return;
      }
      // The zip is made of the stored files themselves, which stay until the next transfer.
      const blobs = new Map<string, Blob>();
      const files = entries.filter((entry) => entry.kind === "file");
      await eachAtOnce(files, CONCURRENT_FILES, async ({ path }) => {
        blobs.set(path.join("/"), await (await fileAt(path)).getFile());
      });
      const zip = await zipOffer(entries, blobs, checksums, onProgress);
      triggerDownload(zip, zipName(tops));
    },

    async abandon() {
      // Closing flushes what arrived, which is what resume reads back.
      await writable?.close().catch(() => {});
      writable = null;
    },
  };
}

/// Buffers in memory and saves the files at the end, zipped if more than one.
export function createZipSink(): TransferSink {
  const files = new Map<string, Blob>();
  const checksums = new Checksums();
  let current: { path: string; chunks: Bytes[] } | null = null;

  return {
    async prepare() {},

    async probe() {
      // Nothing is kept between attempts, so there is never anything to skip.
      return { kind: "missing" };
    },

    async read() {
      throw new Error("Nothing is stored before the transfer");
    },

    async reserve() {
      // The browser says nothing reliable about free memory.
    },

    async createDir() {
      // Folders are part of the offer, which the zip is made from.
    },

    async writeFile(path, offset, data) {
      files.set(path.join("/"), new Blob([data]));
      checksums.whole(path, offset, data);
    },

    async open(path, offset) {
      current = { path: path.join("/"), chunks: [] };
      checksums.begin(path, offset);
    },

    async write(chunk) {
      current!.chunks.push(chunk);
      checksums.update(chunk);
    },

    async closeFile() {
      // A blob lets the browser keep the bytes outside the JS heap.
      if (current) files.set(current.path, new Blob(current.chunks));
      current = null;
      checksums.end();
    },

    async finish(entries, tops, onProgress) {
      if (!isZip(tops)) {
        const [[path, blob]] = files;
        triggerDownload(blob, path);
        return;
      }
      triggerDownload(
        await zipOffer(entries, files, checksums, onProgress),
        zipName(tops),
      );
    },

    async abandon() {
      // A partial in-memory transfer is worthless — nothing can resume it.
      files.clear();
      current = null;
    },
  };
}

/// Whether a transfer of `tops` is saved as a zip: anything but a single file.
const isZip = (tops: TopEntry[]) => tops.length !== 1 || tops[0].dir;

function triggerDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);

  // The click only *schedules* the download — the browser reads the blob after
  // this task ends. Revoking straight away truncates or cancels it, which shows
  // up on exactly the large zips this path exists for. The browser keeps the
  // data alive while it is downloading, so holding the URL costs nothing.
  const timer = setTimeout(() => URL.revokeObjectURL(url), REVOKE_DELAY_MS);
  // Browsers return a number; Node returns a Timeout that would otherwise hold
  // the test runner open for the full delay.
  (timer as { unref?: () => void }).unref?.();
}
