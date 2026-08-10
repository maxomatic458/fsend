import {
  createDiskSink,
  createDownloadSink,
  type TransferSink,
} from "../transfer/sinks";

/// If the browser supports the File System API we can directly write to disk, otherwise we have to download to memory.
export type Storage = DiskStorage | DownloadStorage;

export interface DiskStorage {
  kind: "disk";
  /// Files stream to a folder, so size is bounded by disk, not memory.
  canResume: true;
  /// Must be called from a user gesture.
  chooseFolder(): Promise<FileSystemDirectoryHandle>;
  createSink(folder: FileSystemDirectoryHandle): TransferSink;
}

export interface DownloadStorage {
  kind: "download";
  /// Nothing is kept between attempts, so there is nothing to continue.
  canResume: false;
  createSink(): TransferSink;
}

/// The single probe for this API; nothing else should test for it.
export function hasFileSystemAccess(): boolean {
  let f =
    typeof window !== "undefined" &&
    "showOpenFilePicker" in window &&
    "showDirectoryPicker" in window;
  console.log(f);
  return f;
}

export function detectStorage(): Storage {
  if (hasFileSystemAccess()) {
    return {
      kind: "disk",
      canResume: true,
      chooseFolder: () => window.showDirectoryPicker({ mode: "readwrite" }),
      createSink: createDiskSink,
    };
  }
  return {
    kind: "download",
    canResume: false,
    createSink: createDownloadSink,
  };
}
