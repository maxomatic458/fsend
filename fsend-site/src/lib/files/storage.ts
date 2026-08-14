/**
 * Where a received transfer is written. With the File System Access API the
 * receiver picks; without it, "download" is the only option.
 *
 * disk     — streams into a chosen folder, bounded by disk, resumable.
 * download — buffered in memory, saved (zipped if more than one) at the end.
 */
export type StorageMode = "disk" | "download";

/// The single probe for this API; nothing else should test for it.
export function hasFileSystemAccess(): boolean {
  return (
    typeof window !== "undefined" &&
    "showOpenFilePicker" in window &&
    "showDirectoryPicker" in window
  );
}

/// Must be called from a user gesture.
export function chooseFolder(): Promise<FileSystemDirectoryHandle> {
  return window.showDirectoryPicker({ mode: "readwrite" });
}
