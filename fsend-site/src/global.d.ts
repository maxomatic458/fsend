// Baked in by vite.config from scripts/buildinfo.mjs. The commit is null when
// the build had no git checkout and no CI environment to read it from.
declare const __BUILD_COMMIT__: string | null;
declare const __BUILD_COMMIT_FULL__: string | null;
declare const __BUILD_TIME__: string;
declare const __BUILD_TIMESTAMP__: number;

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
