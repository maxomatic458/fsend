import type { TopEntry } from "../proto";

/**
 * Where received files go.
 *
 * fs  — a folder: the one the user picks (Chromium), or the browser's private
 *       storage (OPFS) elsewhere, saved to Downloads at the end. Resumable.
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
const INCOMING = "incoming";
const LOCK = "fsend-browser-storage";

/**
 * fsend's part of the browser's private storage: transfers arrive in
 * `incoming`, and stay there until the next transfer or a clear.
 */
export class BrowserStorage {
  private constructor(
    private root: FileSystemDirectoryHandle,
    readonly incoming: FileSystemDirectoryHandle,
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
        await root.getDirectoryHandle(INCOMING, { create: true }),
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
    await emptyDir(this.root, (name) => name !== INCOMING);
  }

  /** Drops everything that is not part of `tops`, which may resume. */
  async keepOnly(tops: TopEntry[]): Promise<void> {
    const keep = new Set(tops.map((top) => top.name));
    await emptyDir(this.incoming, (name) => !keep.has(name));
    await emptyDir(this.root, (name) => name !== INCOMING);
  }

  /** Free bytes, if the browser says. */
  async free(): Promise<number | undefined> {
    const { quota, usage } = await this.estimate();
    return quota === undefined ? undefined : quota - (usage ?? 0);
  }
}

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
