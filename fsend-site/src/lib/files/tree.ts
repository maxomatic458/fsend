import type { Entry } from "../proto";
import type { SelectedEntry } from "../types";

/** What a file entry reads from; only its size and slices are used. */
export type Source = Pick<Blob, "size" | "slice">;

/** The offer for a selection, parents first, and the source of each file by id. */
export interface Outgoing {
  entries: Entry[];
  files: (Source | null)[];
}

export async function buildOffer(selected: SelectedEntry[]): Promise<Outgoing> {
  const out: Outgoing = { entries: [], files: [] };
  const addFile = (path: string[], file: Source) => {
    out.entries.push({ kind: "file", path, size: file.size });
    out.files.push(file);
  };
  const addDir = (path: string[]) => {
    out.entries.push({ kind: "dir", path });
    out.files.push(null);
  };

  const walk = async (dir: FileSystemDirectoryHandle, path: string[]) => {
    addDir(path);
    for await (const [, child] of dir.entries()) {
      const childPath = [...path, child.name];
      if (child.kind === "file") {
        addFile(childPath, await (child as FileSystemFileHandle).getFile());
      } else {
        await walk(child as FileSystemDirectoryHandle, childPath);
      }
    }
  };

  for (const entry of selected) {
    if (entry.kind === "file") {
      const file = entry.handle
        ? await (entry.handle as FileSystemFileHandle).getFile()
        : entry.file;
      if (file) addFile([entry.name], file);
    } else if (entry.handle) {
      await walk(entry.handle as FileSystemDirectoryHandle, [entry.name]);
    } else if (entry.files) {
      addDir([entry.name]);
      const dirs = new Set<string>();
      for (const { relativePath, file } of entry.files) {
        const parts = relativePath.split("/");
        for (let depth = 1; depth < parts.length; depth++) {
          const dir = parts.slice(0, depth);
          if (dirs.has(dir.join("/"))) continue;
          dirs.add(dir.join("/"));
          addDir([entry.name, ...dir]);
        }
        addFile([entry.name, ...parts], file);
      }
    }
  }
  return out;
}

/** Zero for a directory. */
export const entrySize = (entry: Entry): number =>
  entry.kind === "file" ? entry.size : 0;

export const offerSizeBytes = (entries: Entry[]): number =>
  entries.reduce((total, entry) => total + entrySize(entry), 0);
