import type {
  FilesAvailable,
  FilesToSkip,
  FileSendRecvTree,
  SelectedEntry,
} from "../types";

export async function buildFileTree(
  entries: SelectedEntry[],
): Promise<FilesAvailable[]> {
  const result: FilesAvailable[] = [];
  for (const entry of entries) {
    if (entry.kind === "file") {
      if (entry.handle) {
        const file = await (entry.handle as FileSystemFileHandle).getFile();
        result.push({ type: "File", name: entry.name, size: file.size });
      } else if (entry.file) {
        result.push({ type: "File", name: entry.name, size: entry.file.size });
      }
    } else {
      if (entry.handle) {
        result.push(
          await dirTreeFromHandle(
            entry.handle as FileSystemDirectoryHandle,
            entry.name,
          ),
        );
      } else if (entry.files) {
        result.push(dirTreeFromFileList(entry.name, entry.files));
      }
    }
  }
  return result;
}

async function dirTreeFromHandle(
  handle: FileSystemDirectoryHandle,
  name: string,
): Promise<FilesAvailable> {
  const children: FilesAvailable[] = [];
  for await (const [, childHandle] of handle.entries()) {
    if (childHandle.kind === "file") {
      const file = await (childHandle as FileSystemFileHandle).getFile();
      children.push({ type: "File", name: childHandle.name, size: file.size });
    } else {
      children.push(
        await dirTreeFromHandle(
          childHandle as FileSystemDirectoryHandle,
          childHandle.name,
        ),
      );
    }
  }
  return { type: "Dir", name, files: children };
}

function dirTreeFromFileList(
  name: string,
  files: { relativePath: string; file: File }[],
): FilesAvailable {
  const dirs: Map<string, FilesAvailable[]> = new Map();

  for (const { relativePath, file } of files) {
    const parts = relativePath.split("/");
    let currentChildren = dirs.get("") ?? [];
    if (!dirs.has("")) dirs.set("", currentChildren);

    for (let i = 0; i < parts.length - 1; i++) {
      const dirPath = parts.slice(0, i + 1).join("/");
      if (!dirs.has(dirPath)) {
        dirs.set(dirPath, []);
        const parentPath = i === 0 ? "" : parts.slice(0, i).join("/");
        const parentChildren = dirs.get(parentPath)!;
        const dirEntry: FilesAvailable = {
          type: "Dir",
          name: parts[i],
          files: dirs.get(dirPath)!,
        };
        parentChildren.push(dirEntry);
      }
    }

    const parentPath = parts.length > 1 ? parts.slice(0, -1).join("/") : "";
    const parentChildren = dirs.get(parentPath)!;
    parentChildren.push({ type: "File", name: file.name, size: file.size });
  }

  return { type: "Dir", name, files: dirs.get("") ?? [] };
}

export function totalSizeBytes(files: FilesAvailable[]): number {
  let total = 0;
  for (const f of files) {
    if (f.type === "File") total += f.size;
    else total += totalSizeBytes(f.files);
  }
  return total;
}

export function entrySizeBytes(entry: FilesAvailable): number {
  if (entry.type === "File") return entry.size;
  return totalSizeBytes(entry.files);
}

export interface FlatFile {
  path: string;
  skipBytes: number;
  sizeBytes: number;
}

export function flattenTree(
  trees: FileSendRecvTree[],
  prefix = "",
): FlatFile[] {
  const result: FlatFile[] = [];
  for (const tree of trees) {
    const path = prefix ? `${prefix}/${tree.name}` : tree.name;
    if (tree.type === "File") {
      result.push({ path, skipBytes: tree.skip, sizeBytes: tree.size });
    } else {
      result.push(...flattenTree(tree.files, path));
    }
  }
  return result;
}

export function applySkip(
  available: FilesAvailable,
  skip: FilesToSkip | null,
): FileSendRecvTree | null {
  if (!skip) return toSendRecvTree(available);

  if (available.type === "File" && skip.type === "File") {
    if (available.size <= skip.skip) return null; // fully transferred
    return {
      type: "File",
      name: available.name,
      skip: skip.skip,
      size: available.size,
    };
  }

  if (available.type === "Dir" && skip.type === "Dir") {
    const remaining: FileSendRecvTree[] = [];
    for (const child of available.files) {
      const childSkip = skip.files.find((s) => s.name === child.name) ?? null;
      const result = applySkip(child, childSkip);
      if (result) remaining.push(result);
    }
    if (remaining.length === 0) return null;
    return { type: "Dir", name: available.name, files: remaining };
  }

  return toSendRecvTree(available);
}

export function treeSizeBytes(tree: FileSendRecvTree): number {
  if (tree.type === "File") return tree.size;
  return tree.files.reduce((sum, f) => sum + treeSizeBytes(f), 0);
}

export function treeSkipBytes(tree: FileSendRecvTree): number {
  if (tree.type === "File") return tree.skip;
  return tree.files.reduce((sum, f) => sum + treeSkipBytes(f), 0);
}

export function toSendRecvTree(available: FilesAvailable): FileSendRecvTree {
  if (available.type === "File") {
    return {
      type: "File",
      name: available.name,
      skip: 0,
      size: available.size,
    };
  }
  return {
    type: "Dir",
    name: available.name,
    files: available.files.map(toSendRecvTree),
  };
}

const UNSAFE_CHARS = /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/;

/** Whether `name` is a single plain path component that displays as written. */
export function isSafeName(name: string): boolean {
  return (
    name !== "" &&
    name !== "." &&
    name !== ".." &&
    !/[/\\]/.test(name) &&
    !UNSAFE_CHARS.test(name)
  );
}

export function isSafeOffer(files: FilesAvailable[]): boolean {
  return files.every(
    (f) => isSafeName(f.name) && (f.type === "File" || isSafeOffer(f.files)),
  );
}

/**
 * Every file in `candidates` as its path and skip, in tree order. `root` maps a
 * top-level entry to its path; anything that does not resolve is null.
 */
export function candidateFiles(
  candidates: (FilesToSkip | null)[],
  root: (index: number, name: string) => string | null,
): { path: string | null; skip: number }[] {
  const out: { path: string | null; skip: number }[] = [];
  const walk = (node: FilesToSkip, path: string | null) => {
    if (node.type === "File") {
      out.push({ path, skip: node.skip });
      return;
    }
    for (const child of node.files) {
      const safe = path !== null && isSafeName(child.name);
      walk(child, safe ? `${path}/${child.name}` : null);
    }
  };
  candidates.forEach((c, i) => c && walk(c, root(i, c.name)));
  return out;
}

/** Keeps the candidate files whose flag in `keep` (in tree order) is set. */
export function retainFiles(
  candidates: (FilesToSkip | null)[],
  keep: boolean[],
): (FilesToSkip | null)[] {
  let next = 0;
  const walk = (node: FilesToSkip): FilesToSkip | null => {
    if (node.type === "File") return keep[next++] ? node : null;
    const files = node.files
      .map((child) => walk(child))
      .filter((child) => child !== null);
    return files.length > 0 ? { ...node, files } : null;
  };
  return candidates.map((c) => c && walk(c));
}
