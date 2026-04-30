import type { SelectedEntry } from './types';
import { supportsFileSystemAccess } from './fsAccess';

export async function pickFiles(): Promise<SelectedEntry[]> {
  if (supportsFileSystemAccess()) {
    const handles = await window.showOpenFilePicker({ multiple: true });
    return handles.map((h) => ({
      kind: 'file' as const,
      name: h.name,
      handle: h,
    }));
  }
  return pickFilesFallback();
}

export async function pickDirectory(): Promise<SelectedEntry> {
  if (supportsFileSystemAccess()) {
    const handle = await window.showDirectoryPicker();
    return { kind: 'directory', name: handle.name, handle };
  }
  return pickDirectoryFallback();
}

export async function pickSaveDirectory(): Promise<FileSystemDirectoryHandle> {
  return window.showDirectoryPicker({ mode: 'readwrite' });
}

function pickFilesFallback(): Promise<SelectedEntry[]> {
  return new Promise((resolve) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.multiple = true;
    input.onchange = () => {
      const files = Array.from(input.files ?? []);
      resolve(
        files.map((f) => ({
          kind: 'file' as const,
          name: f.name,
          file: f,
        })),
      );
    };
    input.click();
  });
}

function pickDirectoryFallback(): Promise<SelectedEntry> {
  return new Promise((resolve) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.webkitdirectory = true;
    input.onchange = () => {
      const fileList = Array.from(input.files ?? []);
      if (fileList.length === 0) return;
      const dirName = fileList[0].webkitRelativePath.split('/')[0];
      resolve({
        kind: 'directory',
        name: dirName,
        files: fileList.map((f) => ({
          relativePath: f.webkitRelativePath.split('/').slice(1).join('/'),
          file: f,
        })),
      });
    };
    input.click();
  });
}

export async function handleDrop(dataTransfer: DataTransfer): Promise<SelectedEntry[]> {
  const entries: SelectedEntry[] = [];

  if (supportsFileSystemAccess()) {
    const items = Array.from(dataTransfer.items);
    for (const item of items) {
      const handle = await item.getAsFileSystemHandle!();
      if (!handle) continue;
      if (handle.kind === 'file') {
        entries.push({ kind: 'file', name: handle.name, handle: handle as FileSystemFileHandle });
      } else {
        entries.push({
          kind: 'directory',
          name: handle.name,
          handle: handle as FileSystemDirectoryHandle,
        });
      }
    }
    return entries;
  }

  // Fallback: use webkitGetAsEntry
  const items = Array.from(dataTransfer.items);
  for (const item of items) {
    const webkitEntry = item.webkitGetAsEntry?.();
    if (!webkitEntry) continue;
    if (webkitEntry.isFile) {
      const file = await getFileFromEntry(webkitEntry as FileSystemFileEntry);
      entries.push({ kind: 'file', name: file.name, file });
    } else if (webkitEntry.isDirectory) {
      const files = await readDirectoryRecursive(webkitEntry as FileSystemDirectoryEntry);
      entries.push({ kind: 'directory', name: webkitEntry.name, files });
    }
  }
  return entries;
}

function getFileFromEntry(entry: FileSystemFileEntry): Promise<File> {
  return new Promise((resolve, reject) => entry.file(resolve, reject));
}

async function readDirectoryRecursive(
  dirEntry: FileSystemDirectoryEntry,
  prefix = '',
): Promise<{ relativePath: string; file: File }[]> {
  const reader = dirEntry.createReader();
  const results: { relativePath: string; file: File }[] = [];

  const readBatch = (): Promise<FileSystemEntry[]> =>
    new Promise((resolve, reject) => reader.readEntries(resolve, reject));

  let batch = await readBatch();
  while (batch.length > 0) {
    for (const entry of batch) {
      const path = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isFile) {
        const file = await getFileFromEntry(entry as FileSystemFileEntry);
        results.push({ relativePath: path, file });
      } else if (entry.isDirectory) {
        const sub = await readDirectoryRecursive(entry as FileSystemDirectoryEntry, path);
        results.push(...sub);
      }
    }
    batch = await readBatch();
  }
  return results;
}
