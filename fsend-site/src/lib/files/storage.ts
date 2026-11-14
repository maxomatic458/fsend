import { makeZip, predictLength } from "client-zip";
import type { Entry, TopEntry } from "../proto";

/**
 * Where received files go.
 *
 * fs  — a folder: the one the user picks (Chromium), or the browser's private
 *       storage (OPFS) elsewhere, exported to Downloads at the end. Resumable.
 * zip — buffered in memory, saved (zipped if more than one) at the end.
 */
export type StorageMode = "fs" | "zip";

/** Which folder the fs mode writes into. */
export type FolderKind = "picked" | "browser";

/** The File System Access pickers, for sending and for a folder to receive into. */
export function hasFileSystemAccess(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.showOpenFilePicker === "function" &&
    typeof window.showDirectoryPicker === "function"
  );
}

/** Whether OPFS can be written to from the page; it may still be refused, as in private windows. */
export function hasBrowserStorage(): boolean {
  return (
    typeof navigator !== "undefined" &&
    typeof navigator.storage?.getDirectory === "function" &&
    typeof FileSystemFileHandle !== "undefined" &&
    "createWritable" in FileSystemFileHandle.prototype
  );
}

/** The folder kind this browser receives into, if any. */
export function folderKind(): FolderKind | null {
  if (hasFileSystemAccess()) return "picked";
  if (hasBrowserStorage()) return "browser";
  return null;
}

export function chooseFolder(): Promise<FileSystemDirectoryHandle> {
  return window.showDirectoryPicker({ mode: "readwrite" });
}

const ROOT = "fsend";
const LOCK = "fsend-browser-storage";

/**
 * fsend's part of the browser's private storage: transfers arrive in
 * `incoming`, and several files are zipped into `export` to be saved.
 */
export class BrowserStorage {
  private constructor(
    private root: FileSystemDirectoryHandle,
    readonly incoming: FileSystemDirectoryHandle,
    private exports: FileSystemDirectoryHandle,
    private estimate: () => Promise<StorageEstimate>,
  ) {}

  /** Opens it below `origin`, or returns null if the browser refuses. */
  static async open(
    origin?: FileSystemDirectoryHandle,
    estimate = () => navigator.storage.estimate(),
  ): Promise<BrowserStorage | null> {
    try {
      const top = origin ?? (await navigator.storage.getDirectory());
      const root = await top.getDirectoryHandle(ROOT, { create: true });
      return new BrowserStorage(
        root,
        await root.getDirectoryHandle("incoming", { create: true }),
        await root.getDirectoryHandle("export", { create: true }),
        estimate,
      );
    } catch {
      return null;
    }
  }

  /** Runs `task` unless another tab is using the storage; returns whether it ran. */
  async exclusive(task: () => Promise<void>): Promise<boolean> {
    if (!navigator.locks) {
      await task();
      return true;
    }
    return navigator.locks.request(
      LOCK,
      { ifAvailable: true },
      async (lock) => {
        if (!lock) return false;
        await task();
        return true;
      },
    );
  }

  /** Bytes of all files kept. */
  async usage(): Promise<number> {
    return sizeOf(this.root);
  }

  async clear(): Promise<void> {
    await emptyDir(this.incoming);
    await emptyDir(this.exports);
  }

  /** Drops past exports and everything incoming that is not part of `tops`, which may resume. */
  async keepOnly(tops: TopEntry[]): Promise<void> {
    await emptyDir(this.exports);
    const keep = new Set(tops.map((top) => top.name));
    await emptyDir(this.incoming, (name) => !keep.has(name));
  }

  /** Free bytes, if the browser says. */
  async free(): Promise<number | undefined> {
    const { quota, usage } = await this.estimate();
    return quota === undefined ? undefined : quota - (usage ?? 0);
  }

  /**
   * The file to save for a complete transfer of `entries`: a single file as
   * is, anything else zipped into `export`, after which the staged files go.
   */
  async export(
    entries: Entry[],
    tops: TopEntry[],
    onProgress?: (percent: number) => void,
  ): Promise<File> {
    if (tops.length === 1 && !tops[0].dir) {
      return (await this.incoming.getFileHandle(tops[0].name)).getFile();
    }

    const inputs: ZipInput[] = [];
    for (const entry of entries) {
      const name = entry.path.join("/");
      inputs.push(
        entry.kind === "dir"
          ? { name }
          : { name, input: await this.file(entry.path) },
      );
    }
    const handle = await this.exports.getFileHandle(zipName(tops), {
      create: true,
    });
    const writable = await handle.createWritable();
    try {
      await writeZip(
        inputs,
        (chunk) => writable.write(chunk as BufferSource),
        onProgress,
      );
      await writable.close();
    } catch (err) {
      await writable.abort().catch(() => {});
      throw err;
    }
    for (const top of tops) {
      await this.incoming.removeEntry(top.name, { recursive: true });
    }
    return handle.getFile();
  }

  private async file(path: string[]): Promise<File> {
    let dir = this.incoming;
    for (const name of path.slice(0, -1)) {
      dir = await dir.getDirectoryHandle(name);
    }
    return (await dir.getFileHandle(path[path.length - 1])).getFile();
  }
}

/** A file, or a folder without `input`, at `name` in a zip. */
export type ZipInput = { name: string } | { name: string; input: Blob };

/** Zips `inputs` (stored, not compressed), handing each chunk to `write`. */
export async function writeZip(
  inputs: ZipInput[],
  write: (chunk: Uint8Array) => Promise<void> | void,
  onProgress?: (percent: number) => void,
): Promise<void> {
  const total = Number(predictLength(inputs));
  const reader = makeZip(inputs).getReader();
  let written = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) return;
    await write(value);
    written += value.length;
    onProgress?.((written / total) * 100);
  }
}

/** The name a transfer of several files is saved under. */
export const zipName = (tops: TopEntry[]): string =>
  tops.length === 1 ? `${tops[0].name}.zip` : "fsend-files.zip";

async function sizeOf(dir: FileSystemDirectoryHandle): Promise<number> {
  let total = 0;
  for await (const [, handle] of dir.entries()) {
    total +=
      handle.kind === "file"
        ? (await (handle as FileSystemFileHandle).getFile()).size
        : await sizeOf(handle as FileSystemDirectoryHandle);
  }
  return total;
}

/** Removes the entries of `dir`, or those `drop` picks. */
async function emptyDir(
  dir: FileSystemDirectoryHandle,
  drop: (name: string) => boolean = () => true,
): Promise<void> {
  const names = [];
  for await (const [name] of dir.entries()) if (drop(name)) names.push(name);
  for (const name of names) await dir.removeEntry(name, { recursive: true });
}
