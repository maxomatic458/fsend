export interface SelectedEntry {
  kind: "file" | "directory";
  name: string;
  handle?: FileSystemDirectoryHandle | FileSystemFileHandle;
  file?: File;
  files?: { relativePath: string; file: File }[];
}
