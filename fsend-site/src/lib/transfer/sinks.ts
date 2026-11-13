import type { Entry, Local, TopEntry } from "../proto";
import { formatBytes } from "../format";
import {
  writeZip,
  zipName,
  type BrowserStorage,
  type ZipInput,
} from "../files/storage";

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
  /// Throws unless `remaining` more bytes fit, of an offer of `total`.
  reserve(remaining: number, total: number): Promise<void>;
  createDir(path: string[]): Promise<void>;
  /// Opens a file to write from `offset` on; at 0 whatever was there is dropped.
  open(path: string[], offset: number): Promise<void>;
  write(chunk: Uint8Array): Promise<void>;
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
  let tops: TopEntry[] = [];

  const dirOf = async (path: string[], create: boolean) => {
    let dir = dirHandle;
    for (const name of path) {
      dir = await dir.getDirectoryHandle(name, { create });
    }
    return dir;
  };

  const fileAt = async (path: string[]) => {
    const dir = await dirOf(path.slice(0, -1), false);
    return dir.getFileHandle(path[path.length - 1]);
  };

  const probe = async (dir: FileSystemDirectoryHandle, name: string) => {
    try {
      const file = await dir.getFileHandle(name);
      return { kind: "file", size: (await file.getFile()).size } as const;
    } catch (err) {
      return (err as DOMException).name === "TypeMismatchError"
        ? ({ kind: "dir" } as const)
        : ({ kind: "missing" } as const);
    }
  };

  return {
    async prepare(offered) {
      tops = offered;
      await storage?.keepOnly(offered);
    },

    async probe(entry) {
      let dir: FileSystemDirectoryHandle;
      try {
        dir = await dirOf(entry.path.slice(0, -1), false);
      } catch {
        return { kind: "missing" };
      }
      const name = entry.path[entry.path.length - 1];
      const local = await probe(dir, name);
      // Browser storage only holds earlier transfers, so whatever is in the way is stale.
      if (storage && local.kind !== "missing" && local.kind !== entry.kind) {
        await dir.removeEntry(name, { recursive: true });
        return { kind: "missing" };
      }
      return local;
    },

    async read(path) {
      return (await fileAt(path)).getFile();
    },

    async reserve(remaining, total) {
      const free = await storage?.free();
      if (free === undefined) return;
      // Several files are zipped next to the staged ones.
      const needed =
        remaining + (tops.length === 1 && !tops[0].dir ? 0 : total);
      if (needed > free) {
        throw new Error(
          `Not enough browser storage: ${formatBytes(needed)} needed, ${formatBytes(free)} free. ` +
            "Clear the browser storage, or choose Download as Zip.",
        );
      }
    },

    async createDir(path) {
      await dirOf(path, true);
    },

    async open(path, offset) {
      const dir = await dirOf(path.slice(0, -1), true);
      const file = await dir.getFileHandle(path[path.length - 1], {
        create: true,
      });
      writable = await file.createWritable({ keepExistingData: offset > 0 });
      if (offset > 0) await writable.seek(offset);
    },

    async write(chunk) {
      await writable!.write(chunk as unknown as BufferSource);
    },

    async closeFile() {
      if (!writable) return;
      await writable.close();
      writable = null;
    },

    async finish(entries, tops, onProgress) {
      if (!storage) return;
      const file = await storage.export(entries, tops, onProgress);
      triggerDownload(file, file.name);
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
  const dirs: string[] = [];
  let current: { path: string; chunks: Uint8Array[] } | null = null;

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

    async createDir(path) {
      dirs.push(path.join("/"));
    },

    async open(path) {
      current = { path: path.join("/"), chunks: [] };
    },

    async write(chunk) {
      current!.chunks.push(chunk);
    },

    async closeFile() {
      // A blob lets the browser keep the bytes outside the JS heap.
      if (current)
        files.set(current.path, new Blob(current.chunks as BlobPart[]));
      current = null;
    },

    async finish(_, tops, onProgress) {
      if (tops.length === 1 && !tops[0].dir && files.size === 1) {
        const [[path, blob]] = files;
        triggerDownload(blob, path);
        return;
      }

      const inputs: ZipInput[] = [
        ...dirs.map((name) => ({ name })),
        ...[...files].map(([name, input]) => ({ name, input })),
      ];
      const chunks: Uint8Array[] = [];
      await writeZip(inputs, (chunk) => void chunks.push(chunk), onProgress);
      triggerDownload(
        new Blob(chunks as BlobPart[], { type: "application/zip" }),
        zipName(tops),
      );
    },

    async abandon() {
      // A partial in-memory transfer is worthless — nothing can resume it.
      files.clear();
      dirs.length = 0;
      current = null;
    },
  };
}

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
