import type { FilesAvailable, FilesToSkip } from "./types";

export function supportsFileSystemAccess(): boolean {
  return (
    typeof window !== "undefined" &&
    "showOpenFilePicker" in window &&
    "showDirectoryPicker" in window
  );
}

export async function getExistingFileSizes(
  dirHandle: FileSystemDirectoryHandle,
  offered: FilesAvailable[],
): Promise<(FilesToSkip | null)[]> {
  const result: (FilesToSkip | null)[] = [];
  for (const entry of offered) {
    result.push(await getSkipInfo(dirHandle, entry));
  }
  return result;
}

async function getSkipInfo(
  dirHandle: FileSystemDirectoryHandle,
  offered: FilesAvailable,
): Promise<FilesToSkip | null> {
  if (offered.type === "File") {
    try {
      const fh = await dirHandle.getFileHandle(offered.name);
      const file = await fh.getFile();
      if (file.size > 0) {
        return { type: "File", name: offered.name, skip: file.size };
      }
    } catch {
      // file doesn't exist
    }
    return null;
  }

  // Directory
  try {
    const subDir = await dirHandle.getDirectoryHandle(offered.name);
    const skipFiles: FilesToSkip[] = [];
    for (const child of offered.files) {
      const skip = await getSkipInfo(subDir, child);
      if (skip) skipFiles.push(skip);
    }
    if (skipFiles.length === 0) return null;
    return { type: "Dir", name: offered.name, files: skipFiles };
  } catch {
    return null;
  }
}
