import type { FilesAvailable, FilesToSkip } from "../types";

/// Long enough for the browser to have started reading the blob.
const REVOKE_DELAY_MS = 60_000;

/// Bytes are written into this sink
export interface TransferSink {
  /** Whether a partially received transfer can be continued later. */
  readonly canResume: boolean;
  /** Bytes already present per offered entry, so the sender can skip them. */
  existing(offered: FilesAvailable[]): Promise<(FilesToSkip | null)[]>;
  /** Begin a file; `skipBytes` of it are already accounted for. */
  open(path: string, skipBytes: number): Promise<void>;
  write(chunk: Uint8Array): Promise<void>;
  /** Finish the file opened by the last `open`. Safe to call with none open. */
  closeFile(): Promise<void>;
  /**
   * All files received — hand them over (e.g. browser download).
   * `onProgress` reports 0-100 e.g. for packing zips
   */
  finish(
    offered: FilesAvailable[],
    onProgress?: (percent: number) => void,
  ): Promise<void>;
  /** Transfer ended early; salvage whatever is worth keeping. */
  abandon(): Promise<void>;
}

// Disk if the File System Access API is available
export function createDiskSink(
  dirHandle: FileSystemDirectoryHandle,
): TransferSink {
  let writable: FileSystemWritableFileStream | null = null;

  return {
    canResume: true,

    async existing(offered) {
      const result: (FilesToSkip | null)[] = [];
      for (const entry of offered) {
        result.push(await existingFor(dirHandle, entry));
      }
      return result;
    },

    async open(path, skipBytes) {
      const parts = path.split("/");
      let dir = dirHandle;
      for (let i = 0; i < parts.length - 1; i++) {
        dir = await dir.getDirectoryHandle(parts[i], { create: true });
      }
      const file = await dir.getFileHandle(parts[parts.length - 1], {
        create: true,
      });
      writable = await file.createWritable({ keepExistingData: skipBytes > 0 });
      if (skipBytes > 0) await writable.seek(skipBytes);
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

async function existingFor(
  dir: FileSystemDirectoryHandle,
  offered: FilesAvailable,
): Promise<FilesToSkip | null> {
  if (offered.type === "File") {
    try {
      const handle = await dir.getFileHandle(offered.name);
      const file = await handle.getFile();
      if (file.size > 0) {
        return { type: "File", name: offered.name, skip: file.size };
      }
    } catch {
      // Not there yet — nothing to skip.
    }
    return null;
  }

  try {
    const sub = await dir.getDirectoryHandle(offered.name);
    const files: FilesToSkip[] = [];
    for (const child of offered.files) {
      const skip = await existingFor(sub, child);
      if (skip) files.push(skip);
    }
    return files.length > 0 ? { type: "Dir", name: offered.name, files } : null;
  } catch {
    return null;
  }
}

/// Download to memory
export function createDownloadSink(): TransferSink {
  const buffers = new Map<string, Uint8Array[]>();
  let current: Uint8Array[] | null = null;
  let order: string[] = [];

  return {
    canResume: false,

    async existing(offered) {
      // Nothing is kept between attempts, so there is never anything to skip.
      return offered.map(() => null);
    },

    async open(path) {
      current = buffers.get(path) ?? [];
      if (!buffers.has(path)) {
        buffers.set(path, current);
        order.push(path);
      }
    },

    async write(chunk) {
      current!.push(chunk);
    },

    async closeFile() {
      current = null;
    },

    async finish(offered, onProgress) {
      const single =
        order.length === 1 &&
        offered.length === 1 &&
        offered[0].type === "File";

      if (single) {
        const path = order[0];
        triggerDownload(
          new Blob(buffers.get(path)! as BlobPart[]),
          path.split("/").pop()!,
        );
        return;
      }

      const JSZip = (await import("jszip")).default;
      const zip = new JSZip();
      for (const [path, chunks] of buffers) {
        zip.file(path, new Blob(chunks as BlobPart[]));
      }
      const blob = await zip.generateAsync({ type: "blob" }, (meta) =>
        onProgress?.(meta.percent),
      );
      const name =
        offered.length === 1 ? `${offered[0].name}.zip` : "fsend-files.zip";
      triggerDownload(blob, name);
    },

    async abandon() {
      // A partial in-memory transfer is worthless — nothing can resume it.
      buffers.clear();
      order = [];
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
