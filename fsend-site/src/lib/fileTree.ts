import type { FilesAvailable, FilesToSkip, FileSendRecvTree, SelectedEntry } from './types';

export async function buildFileTree(entries: SelectedEntry[]): Promise<FilesAvailable[]> {
  const result: FilesAvailable[] = [];
  for (const entry of entries) {
    if (entry.kind === 'file') {
      if (entry.handle) {
        const file = await (entry.handle as FileSystemFileHandle).getFile();
        result.push({ type: 'File', name: entry.name, size: file.size });
      } else if (entry.file) {
        result.push({ type: 'File', name: entry.name, size: entry.file.size });
      }
    } else {
      if (entry.handle) {
        result.push(await dirTreeFromHandle(entry.handle as FileSystemDirectoryHandle, entry.name));
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
    if (childHandle.kind === 'file') {
      const file = await (childHandle as FileSystemFileHandle).getFile();
      children.push({ type: 'File', name: childHandle.name, size: file.size });
    } else {
      children.push(await dirTreeFromHandle(childHandle as FileSystemDirectoryHandle, childHandle.name));
    }
  }
  return { type: 'Dir', name, files: children };
}

function dirTreeFromFileList(
  name: string,
  files: { relativePath: string; file: File }[],
): FilesAvailable {
  const root: Map<string, FilesAvailable> = new Map();
  const dirs: Map<string, FilesAvailable[]> = new Map();

  for (const { relativePath, file } of files) {
    const parts = relativePath.split('/');
    let currentChildren = dirs.get('') ?? [];
    if (!dirs.has('')) dirs.set('', currentChildren);

    for (let i = 0; i < parts.length - 1; i++) {
      const dirPath = parts.slice(0, i + 1).join('/');
      if (!dirs.has(dirPath)) {
        dirs.set(dirPath, []);
        const parentPath = i === 0 ? '' : parts.slice(0, i).join('/');
        const parentChildren = dirs.get(parentPath)!;
        const dirEntry: FilesAvailable = { type: 'Dir', name: parts[i], files: dirs.get(dirPath)! };
        parentChildren.push(dirEntry);
      }
    }

    const parentPath = parts.length > 1 ? parts.slice(0, -1).join('/') : '';
    const parentChildren = dirs.get(parentPath)!;
    parentChildren.push({ type: 'File', name: file.name, size: file.size });
  }

  return { type: 'Dir', name, files: dirs.get('') ?? [] };
}

export function totalSize(files: FilesAvailable[]): number {
  let total = 0;
  for (const f of files) {
    if (f.type === 'File') total += f.size;
    else total += totalSize(f.files);
  }
  return total;
}

export function entrySize(entry: FilesAvailable): number {
  if (entry.type === 'File') return entry.size;
  return totalSize(entry.files);
}

export function flattenTree(
  trees: FileSendRecvTree[],
  prefix = '',
): Array<{ path: string; skip: number; size: number }> {
  const result: Array<{ path: string; skip: number; size: number }> = [];
  for (const tree of trees) {
    const path = prefix ? `${prefix}/${tree.name}` : tree.name;
    if (tree.type === 'File') {
      result.push({ path, skip: tree.skip, size: tree.size });
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

  if (available.type === 'File' && skip.type === 'File') {
    if (available.size <= skip.skip) return null; // fully transferred
    return { type: 'File', name: available.name, skip: skip.skip, size: available.size };
  }

  if (available.type === 'Dir' && skip.type === 'Dir') {
    const remaining: FileSendRecvTree[] = [];
    for (const child of available.files) {
      const childSkip = skip.files.find((s) => s.name === child.name) ?? null;
      const result = applySkip(child, childSkip);
      if (result) remaining.push(result);
    }
    if (remaining.length === 0) return null;
    return { type: 'Dir', name: available.name, files: remaining };
  }

  return toSendRecvTree(available);
}

export function treeSize(tree: FileSendRecvTree): number {
  if (tree.type === 'File') return tree.size;
  return tree.files.reduce((sum, f) => sum + treeSize(f), 0);
}

export function treeSkip(tree: FileSendRecvTree): number {
  if (tree.type === 'File') return tree.skip;
  return tree.files.reduce((sum, f) => sum + treeSkip(f), 0);
}

export function toSendRecvTree(available: FilesAvailable): FileSendRecvTree {
  if (available.type === 'File') {
    return { type: 'File', name: available.name, skip: 0, size: available.size };
  }
  return {
    type: 'Dir',
    name: available.name,
    files: available.files.map(toSendRecvTree),
  };
}
