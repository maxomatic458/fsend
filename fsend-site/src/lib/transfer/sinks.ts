import type { Local, TopEntry } from "../proto";

/// Long enough for the browser to have started reading the blob.
const REVOKE_DELAY_MS = 60_000;

/// Bytes are written into this sink
export interface TransferSink {
  /// What is already at `path`.
  probe(path: string[]): Promise<Local>;
  /// The file at `path`, which `probe` found.
  read(path: string[]): Promise<Blob>;
  createDir(path: string[]): Promise<void>;
  /// Opens a file to write from `offset` on; at 0 whatever was there is dropped.
  open(path: string[], offset: number): Promise<void>;
  write(chunk: Uint8Array): Promise<void>;
  closeFile(): Promise<void>;
  // Finish the transfer, by either flushing writes and closing files or by
  // zipping and downloading the in-memory buffers.
  finish(
    entries: TopEntry[],
    onProgress?: (percent: number) => void,
  ): Promise<void>;
  abandon(): Promise<void>;
}

// Disk if the File System Access API is available
export function createDiskSink(
  dirHandle: FileSystemDirectoryHandle,
): TransferSink {
  let writable: FileSystemWritableFileStream | null = null;

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

  return {
    async probe(path) {
      let dir: FileSystemDirectoryHandle;
      try {
        dir = await dirOf(path.slice(0, -1), false);
      } catch {
        return { kind: "missing" };
      }
      try {
        const file = await dir.getFileHandle(path[path.length - 1]);
        return { kind: "file", size: (await file.getFile()).size };
      } catch (err) {
        return (err as DOMException).name === "TypeMismatchError"
          ? { kind: "dir" }
          : { kind: "missing" };
      }
    },

    async read(path) {
      return (await fileAt(path)).getFile();
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

    async finish() {
      // Already on disk.
    },

    async abandon() {
      // Closing flushes what arrived, which is what resume reads back.
      await writable?.close().catch(() => {});
      writable = null;
    },
  };
}

/// Download to memory
export function createDownloadSink(): TransferSink {
  const buffers = new Map<string, Uint8Array[]>();
  const dirs: string[] = [];
  let current: Uint8Array[] | null = null;

  return {
    async probe() {
      // Nothing is kept between attempts, so there is never anything to skip.
      return { kind: "missing" };
    },

    async read() {
      throw new Error("Nothing is stored before the transfer");
    },

    async createDir(path) {
      dirs.push(path.join("/"));
    },

    async open(path) {
      const key = path.join("/");
      current = buffers.get(key) ?? [];
      buffers.set(key, current);
    },

    async write(chunk) {
      current!.push(chunk);
    },

    async closeFile() {
      current = null;
    },

    async finish(entries, onProgress) {
      if (entries.length === 1 && !entries[0].dir && buffers.size === 1) {
        const [[path, chunks]] = buffers;
        triggerDownload(new Blob(chunks as BlobPart[]), path);
        return;
      }

      const JSZip = (await import("jszip")).default;
      const zip = new JSZip();
      for (const dir of dirs) zip.folder(dir);
      for (const [path, chunks] of buffers) {
        zip.file(path, new Blob(chunks as BlobPart[]));
      }
      const blob = await zip.generateAsync({ type: "blob" }, (meta) =>
        onProgress?.(meta.percent),
      );
      const name =
        entries.length === 1 ? `${entries[0].name}.zip` : "fsend-files.zip";
      triggerDownload(blob, name);
    },

    async abandon() {
      // A partial in-memory transfer is worthless — nothing can resume it.
      buffers.clear();
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
