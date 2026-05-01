// Augment Window with File System Access API methods
interface Window {
  showOpenFilePicker(options?: {
    multiple?: boolean;
    types?: Array<{ description?: string; accept: Record<string, string[]> }>;
  }): Promise<FileSystemFileHandle[]>;
  showDirectoryPicker(options?: {
    mode?: "read" | "readwrite";
  }): Promise<FileSystemDirectoryHandle>;
}

// Augment DataTransferItem
interface DataTransferItem {
  getAsFileSystemHandle?(): Promise<
    FileSystemFileHandle | FileSystemDirectoryHandle | null
  >;
}

// Augment HTMLInputElement
interface HTMLInputElement {
  webkitdirectory: boolean;
}
