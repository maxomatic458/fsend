import {
    fingerprint,
    fingerprintRanges,
    type AcceptedFile,
    type Entry,
    type Fingerprint,
    type Node,
    type Offer,
} from "../proto";
import type { SelectedEntry } from "../types";

/** What a file entry reads from; only its size and slices are used. */
export type Source = Pick<Blob, "size" | "slice">;

/** The offer for a selection, and the source of each entry by id; `null` for a folder. */
export interface Outgoing {
    offer: Offer;
    files: (Source | null)[];
}

/** A file or folder of the selection, before it is sorted into the offer. */
interface Item {
    name: string;
    file?: Source;
    children?: Map<string, Item>;
}

/** Orders names by code point, as the CLI does, so every sender lists a tree alike. */
export function byName(a: string, b: string): number {
    let i = 0;
    let j = 0;
    while (i < a.length && j < b.length) {
        const x = a.codePointAt(i)!;
        const y = b.codePointAt(j)!;
        if (x !== y) return x - y;
        i += x > 0xffff ? 2 : 1;
        j += y > 0xffff ? 2 : 1;
    }
    return Number(i < a.length) - Number(j < b.length);
}

/** The offer of `selected`, sorted by name at every level. */
export async function buildOffer(selected: SelectedEntry[]): Promise<Outgoing> {
    const items = await Promise.all(selected.map(itemOf));
    items.sort((a, b) => byName(a.name, b.name));
    const out: Outgoing = { offer: { entries: [] }, files: [] };
    for (const item of items) out.offer.entries.push(nodeOf(item, out.files));
    return out;
}

async function itemOf(entry: SelectedEntry): Promise<Item> {
    if (entry.kind === "file") {
        return {
            name: entry.name,
            file: "file" in entry ? entry.file : await entry.handle.getFile(),
        };
    }
    if ("handle" in entry) return walk(entry.name, entry.handle);
    const root: Item = { name: entry.name, children: new Map() };
    for (const { relativePath, file } of entry.files) {
        const parts = relativePath.split("/");
        let dir = root;
        for (const part of parts.slice(0, -1)) {
            let next = dir.children!.get(part);
            if (!next) {
                next = { name: part, children: new Map() };
                dir.children!.set(part, next);
            }
            dir = next;
        }
        const name = parts[parts.length - 1];
        dir.children!.set(name, { name, file });
    }
    return root;
}

async function walk(name: string, dir: FileSystemDirectoryHandle): Promise<Item> {
    const children = new Map<string, Item>();
    for await (const [, child] of dir.entries()) {
        children.set(
            child.name,
            child.kind === "file"
                ? { name: child.name, file: await (child as FileSystemFileHandle).getFile() }
                : await walk(child.name, child as FileSystemDirectoryHandle),
        );
    }
    return { name, children };
}

/** The node of `item`; its sources follow in offer order. */
function nodeOf(item: Item, files: (Source | null)[]): Node {
    files.push(item.file ?? null);
    if (item.file) return { kind: "file", name: item.name, size: item.file.size };
    const children = [...item.children!.values()].sort((a, b) => byName(a.name, b.name));
    return { kind: "dir", name: item.name, entries: children.map((child) => nodeOf(child, files)) };
}

/** Zero for a directory. */
export const entrySize = (entry: Entry): number => (entry.kind === "file" ? entry.size : 0);

/** Bytes of the files a node is or holds. */
export const nodeSize = (node: Node): number =>
    node.kind === "file" ? node.size : node.entries.reduce((n, child) => n + nodeSize(child), 0);

export const offerSize = (offer: Offer): number =>
    offer.entries.reduce((n, node) => n + nodeSize(node), 0);

/** The files a node is or holds. */
export const nodeFiles = (node: Node): number =>
    node.kind === "file" ? 1 : node.entries.reduce((n, child) => n + nodeFiles(child), 0);

/** The top-level entry each entry lies in, by id. */
export function topIndices(entries: Entry[]): number[] {
    let top = -1;
    return entries.map((entry) => (entry.path.length === 1 ? ++top : top));
}

/** Bytes of each top-level entry already there once `files` are accepted. */
export function presentBytes(offer: Offer, entries: Entry[], files: AcceptedFile[]): number[] {
    const present = offer.entries.map(nodeSize);
    const topOf = topIndices(entries);
    for (const { id, offset } of files) present[topOf[id]] -= entrySize(entries[id]) - offset;
    return present;
}

/** The fingerprint of the first `len` bytes of `source`. */
export async function fingerprintOf(source: Source, len: number): Promise<Fingerprint> {
    const ranges = fingerprintRanges(len);
    const sampled = new Uint8Array(ranges.reduce((n, [start, end]) => n + end - start, 0));
    let offset = 0;
    for (const [start, end] of ranges) {
        const part = new Uint8Array(await source.slice(start, end).arrayBuffer());
        if (part.length !== end - start) {
            throw new Error("A file is shorter than expected");
        }
        sampled.set(part, offset);
        offset += part.length;
    }
    return fingerprint(sampled);
}
