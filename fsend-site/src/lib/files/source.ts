import type { SelectedEntry } from "../types";
import { hasFileSystemAccess } from "./storage";

/// Choose files to send
export async function pickFiles(): Promise<SelectedEntry[]> {
  if (hasFileSystemAccess()) {
    const handles = await window.showOpenFilePicker({ multiple: true });
    return handles.map((handle) => ({
      kind: "file" as const,
      name: handle.name,
      handle,
    }));
  }
  return pickViaInput({ multiple: true });
}

export async function pickDirectory(): Promise<SelectedEntry> {
  if (hasFileSystemAccess()) {
    const handle = await window.showDirectoryPicker();
    return { kind: "directory", name: handle.name, handle };
  }
  return pickViaInput({ directory: true }).then((entries) => entries[0]);
}

function pickViaInput(opts: {
  multiple?: boolean;
  directory?: boolean;
}): Promise<SelectedEntry[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    if (opts.multiple) input.multiple = true;
    if (opts.directory) input.webkitdirectory = true;

    input.onchange = () => {
      const picked = Array.from(input.files ?? []);
      if (picked.length === 0) return resolve([]);

      if (!opts.directory) {
        return resolve(
          picked.map((file) => ({ kind: "file", name: file.name, file })),
        );
      }

      const root = picked[0].webkitRelativePath.split("/")[0];
      resolve([
        {
          kind: "directory",
          name: root,
          files: picked.map((file) => ({
            relativePath: file.webkitRelativePath
              .split("/")
              .slice(1)
              .join("/"),
            file,
          })),
        },
      ]);
    };
    input.click();
  });
}

/// Drag and drop
export async function handleDrop(
  dataTransfer: DataTransfer,
): Promise<SelectedEntry[]> {
  const entries: SelectedEntry[] = [];
  const items = Array.from(dataTransfer.items);

  if (hasFileSystemAccess()) {
    for (const item of items) {
      const handle = await item.getAsFileSystemHandle!();
      if (!handle) continue;
      entries.push(
        handle.kind === "file"
          ? { kind: "file", name: handle.name, handle: handle as FileSystemFileHandle }
          : {
              kind: "directory",
              name: handle.name,
              handle: handle as FileSystemDirectoryHandle,
            },
      );
    }
    return entries;
  }

  for (const item of items) {
    const entry = item.webkitGetAsEntry?.();
    if (!entry) continue;
    if (entry.isFile) {
      const file = await fileFromEntry(entry as FileSystemFileEntry);
      entries.push({ kind: "file", name: file.name, file });
    } else if (entry.isDirectory) {
      entries.push({
        kind: "directory",
        name: entry.name,
        files: await readDirectory(entry as FileSystemDirectoryEntry),
      });
    }
  }
  return entries;
}

function fileFromEntry(entry: FileSystemFileEntry): Promise<File> {
  return new Promise((resolve, reject) => entry.file(resolve, reject));
}

async function readDirectory(
  dir: FileSystemDirectoryEntry,
  prefix = "",
): Promise<{ relativePath: string; file: File }[]> {
  const reader = dir.createReader();
  const results: { relativePath: string; file: File }[] = [];

  const readBatch = (): Promise<FileSystemEntry[]> =>
    new Promise((resolve, reject) => reader.readEntries(resolve, reject));

  // readEntries returns at most a page at a time; keep going until it's empty.
  let batch = await readBatch();
  while (batch.length > 0) {
    for (const entry of batch) {
      const path = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isFile) {
        results.push({
          relativePath: path,
          file: await fileFromEntry(entry as FileSystemFileEntry),
        });
      } else if (entry.isDirectory) {
        results.push(
          ...(await readDirectory(entry as FileSystemDirectoryEntry, path)),
        );
      }
    }
    batch = await readBatch();
  }
  return results;
}

/// Reading at transfer time
/// Flattens selected entries to the `path -> File` map the sender reads from
export async function collectFiles(
  entries: SelectedEntry[],
): Promise<Map<string, File>> {
  const files = new Map<string, File>();

  for (const entry of entries) {
    if (entry.kind === "file") {
      if (entry.handle) {
        files.set(
          entry.name,
          await (entry.handle as FileSystemFileHandle).getFile(),
        );
      } else if (entry.file) {
        files.set(entry.name, entry.file);
      }
    } else if (entry.handle) {
      await collectFromHandle(
        entry.handle as FileSystemDirectoryHandle,
        entry.name,
        files,
      );
    } else if (entry.files) {
      for (const { relativePath, file } of entry.files) {
        files.set(`${entry.name}/${relativePath}`, file);
      }
    }
  }

  return files;
}

async function collectFromHandle(
  dir: FileSystemDirectoryHandle,
  prefix: string,
  into: Map<string, File>,
): Promise<void> {
  for await (const [, child] of dir.entries()) {
    const path = `${prefix}/${child.name}`;
    if (child.kind === "file") {
      into.set(path, await (child as FileSystemFileHandle).getFile());
    } else {
      await collectFromHandle(child as FileSystemDirectoryHandle, path, into);
    }
  }
}
