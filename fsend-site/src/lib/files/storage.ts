/**
 * Where the files are stored/written to during a transfer.
 * If the browser supports the File System Access API, the user can choose.
 * 
 * disk     — streams into a chosen folder, bounded by disk, resumable.
 * download — buffered in memory, saved (zipped if more than one) at the end.
 */
export type StorageMode = "disk" | "download";

export function hasFileSystemAccess(): boolean {
  return (
    typeof window !== "undefined" &&
    "showOpenFilePicker" in window &&
    "showDirectoryPicker" in window
  );
}

export function chooseFolder(): Promise<FileSystemDirectoryHandle> {
  return window.showDirectoryPicker({ mode: "readwrite" });
}
