import { entrySize, type Source } from "../files/tree";
import type { AcceptedFile, Algorithm, Entry, Node, Offer } from "../proto";
import { createCodec, type BlockCodec } from "./codec";
import { TransferFailure } from "./peer";

/** Files the sender hashes at once. */
const HASHERS = 4;
/** Bytes read and hashed at a time. */
const CHUNK_BYTES = 4 * 1024 * 1024;

/** Feeds `codec` the first `len` bytes of `source` as file `id`; `onBytes` counts them. */
async function feed(
    codec: BlockCodec,
    id: number,
    algorithm: Algorithm,
    source: Source,
    len: number,
    onBytes: (n: number) => void,
    abort?: AbortSignal,
): Promise<void> {
    for (let at = 0; at < len; at += CHUNK_BYTES) {
        abort?.throwIfAborted();
        const end = Math.min(at + CHUNK_BYTES, len);
        const bytes = new Uint8Array(await source.slice(at, end).arrayBuffer());
        if (bytes.length !== end - at) throw new Error("A file is shorter than expected");
        await codec.hash(id, algorithm, bytes);
        onBytes(bytes.length);
    }
}

/** Gives every file of `offer` its checksum under `algorithm`, reading `sources` by id. */
export async function addChecksums(
    offer: Offer,
    sources: (Source | null)[],
    algorithm: Algorithm,
    onProgress: (done: number, total: number) => void,
    abort: AbortSignal,
): Promise<void> {
    const files = sources.flatMap((source, id) => (source ? [id] : []));
    const total = files.reduce((n, id) => n + sources[id]!.size, 0);
    const digests = new Map<number, string>();
    let done = 0;
    let next = 0;
    onProgress(done, total);
    const codecs = await Promise.all(
        Array.from({ length: Math.min(HASHERS, files.length) }, createCodec),
    );
    try {
        await Promise.all(
            codecs.map(async (codec) => {
                while (next < files.length) {
                    const id = files[next++];
                    const source = sources[id]!;
                    const counted = (n: number) => onProgress((done += n), total);
                    await feed(codec, id, algorithm, source, source.size, counted, abort);
                    digests.set(id, await codec.digest(id, algorithm));
                }
            }),
        );
    } finally {
        for (const codec of codecs) codec.close();
    }

    offer.checksums = algorithm;
    let id = 0;
    const set = (node: Node) => {
        const at = id++;
        if (node.kind === "file") node.checksum = digests.get(at);
        else node.entries.forEach(set);
    };
    offer.entries.forEach(set);
}

/** Fails silently to the peer, as if the connection dropped; `advice` says what the user can do. */
export const checksumMismatch = (path: string[], advice?: string): TransferFailure =>
    new TransferFailure(
        [`${path.join("/")} does not match its checksum.`, advice].filter(Boolean).join(" "),
    );

/** Throws unless what file `id` was fed matches its checksum. */
export async function expectChecksum(
    codec: BlockCodec,
    algorithm: Algorithm,
    entry: Entry,
    id: number,
    advice?: string,
): Promise<void> {
    const digest = await codec.digest(id, algorithm);
    if (entry.kind !== "file" || digest !== entry.checksum) {
        throw checksumMismatch(entry.path, advice);
    }
}

/**
 * Checks the files already complete against their checksums, and feeds `codec`
 * the part of each resumed file already here, to go on with as the rest arrives.
 */
export async function verifyPresent(
    codec: BlockCodec,
    algorithm: Algorithm,
    entries: Entry[],
    complete: number[],
    files: AcceptedFile[],
    read: (path: string[]) => Promise<Source>,
    onProgress: (done: number, total: number) => void,
    advice?: string,
): Promise<void> {
    const jobs = [
        ...complete.map((id) => ({ id, len: entrySize(entries[id]) })),
        ...files.filter(({ offset }) => offset > 0).map(({ id, offset }) => ({ id, len: offset })),
    ];
    const total = jobs.reduce((n, { len }) => n + len, 0);
    let done = 0;
    for (const { id, len } of jobs) {
        const source = await read(entries[id].path);
        await feed(codec, id, algorithm, source, len, (n) => onProgress((done += n), total));
    }
    for (const id of complete) await expectChecksum(codec, algorithm, entries[id], id, advice);
}
