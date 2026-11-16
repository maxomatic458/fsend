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

/// Choose a folder to send; undefined if none was chosen.
export async function pickDirectory(): Promise<SelectedEntry | undefined> {
    if (hasFileSystemAccess()) {
        const handle = await window.showDirectoryPicker();
        return { kind: "directory", name: handle.name, handle };
    }
    const [picked] = await pickViaInput({ directory: true });
    return picked;
}

function pickViaInput(opts: { multiple?: boolean; directory?: boolean }): Promise<SelectedEntry[]> {
    return new Promise((resolve) => {
        const input = document.createElement("input");
        input.type = "file";
        if (opts.multiple) input.multiple = true;
        if (opts.directory) input.webkitdirectory = true;
        // Some browsers only report the choice for an input in the document.
        input.hidden = true;
        document.body.append(input);
        input.oncancel = () => {
            input.remove();
            resolve([]);
        };

        input.onchange = () => {
            input.remove();
            const picked = Array.from(input.files ?? []);
            if (picked.length === 0) return resolve([]);

            if (!opts.directory) {
                return resolve(picked.map((file) => ({ kind: "file", name: file.name, file })));
            }

            const root = picked[0].webkitRelativePath.split("/")[0];
            resolve([
                {
                    kind: "directory",
                    name: root,
                    files: picked.map((file) => ({
                        relativePath: file.webkitRelativePath.split("/").slice(1).join("/"),
                        file,
                    })),
                },
            ]);
        };
        input.click();
    });
}

/// Drag and drop
export async function handleDrop(dataTransfer: DataTransfer): Promise<SelectedEntry[]> {
    const entries: SelectedEntry[] = [];
    // Items can only be read during the drop event, so all are read before awaiting any.
    const items = Array.from(dataTransfer.items).filter((item) => item.kind === "file");

    if (hasFileSystemAccess()) {
        const handles = await Promise.all(items.map((item) => item.getAsFileSystemHandle!()));
        for (const handle of handles) {
            if (!handle) continue;
            entries.push(
                handle.kind === "file"
                    ? {
                          kind: "file",
                          name: handle.name,
                          handle: handle as FileSystemFileHandle,
                      }
                    : {
                          kind: "directory",
                          name: handle.name,
                          handle: handle as FileSystemDirectoryHandle,
                      },
            );
        }
        return entries;
    }

    for (const entry of items.map((item) => item.webkitGetAsEntry?.())) {
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
                results.push(...(await readDirectory(entry as FileSystemDirectoryEntry, path)));
            }
        }
        batch = await readBatch();
    }
    return results;
}
