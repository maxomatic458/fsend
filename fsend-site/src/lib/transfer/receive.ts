import { CONCURRENT_FILES } from "../../config";
import { connectReceiver, pairReceiver, type Link } from "../transport/session";
import type { Relay } from "../transport/relay";
import { getConnectionType } from "../transport/webrtc";
import { entrySize, fingerprintOf, topIndices } from "../files/tree";
import {
    ACK_INTERVAL_MS,
    ACK_STEP,
    IDLE_TIMEOUT_MS,
    WINDOW,
    Progress,
    acceptPlan,
    filesToCheck,
    flattenOffer,
    planReceive,
    type AcceptedFile,
    type Algorithm,
    type Bytes,
    type CheckedFile,
    type ConflictPolicy,
    type Entry,
    type Local,
    type Offer,
} from "../proto";
import {
    Peer,
    TransferFailure,
    type ReceiverPeer,
    messageOf,
    protocolError,
    transferring,
    unexpected,
} from "./peer";
import { NotEnoughRoom, type Asking, type TransferSink } from "./sinks";
import type { TransferListener } from "./events";
import { createCodec, type BlockCodec } from "./codec";
import { expectChecksum, verifyPresent } from "./checksums";

const SENDER_IDLE = "the sender stopped sending";

export interface ReceiveOptions {
    /// Where the transfer goes instead if `sink` has no room for it.
    fallback?: () => TransferSink;
    /// Resolves once the user would rather not wait for the browser to allow more room.
    skipAsking?: Promise<void>;
}

/// Receives a transfer into `sink`; files there that differ from the offer are handled by `policy`.
export async function runReceive(
    code: string,
    sink: TransferSink,
    policy: ConflictPolicy,
    emit: TransferListener,
    abort: AbortSignal,
    options: ReceiveOptions = {},
): Promise<void> {
    let relay: Relay | null = null;
    let link: Link | null = null;
    // The sink in use, which the fallback may replace before anything is written.
    let current = sink;

    try {
        emit({ type: "connecting" });
        let offer: Offer;
        ({ relay, offer } = await pairReceiver(code, abort));
        if (!(await decide(offer, relay, emit, abort))) {
            relay.reject();
            throw new TransferFailure("Transfer rejected");
        }
        link = await connectReceiver(relay, abort);
        relay = null;

        getConnectionType(link.pc)
            .then((kind) => emit({ type: "connectionType", kind }))
            .catch(() => {});

        const peer = Peer.receiver(link, abort);
        const entries = flattenOffer(offer);
        await peer.explained(() =>
            receive(peer, offer, entries, sink, policy, emit, options, (next) => (current = next)),
        );
        link.close();

        await current.finish(entries, offer.entries, (percent) =>
            emit({ type: "packing", percent }),
        );
        emit({ type: "complete" });
    } catch (err) {
        relay?.close();
        await current.abandon();
        if (!abort.aborted) emit({ type: "error", message: messageOf(err) });
    } finally {
        link?.close();
    }
}

/** Whether the user takes `offer`; fails if the sender leaves or the user cancels meanwhile. */
function decide(
    offer: Offer,
    relay: Relay,
    emit: TransferListener,
    abort: AbortSignal,
): Promise<boolean> {
    const ended = relay.ended();
    ended.catch(() => {});
    return Promise.race([
        new Promise<boolean>((resolve) =>
            emit({
                type: "offered",
                offer,
                accept: () => resolve(true),
                reject: () => resolve(false),
            }),
        ),
        ended,
        new Promise<never>((_, reject) => {
            const aborted = () => reject(new DOMException("Aborted", "AbortError"));
            if (abort.aborted) aborted();
            else abort.addEventListener("abort", aborted, { once: true });
        }),
    ]);
}

async function receive(
    peer: ReceiverPeer,
    offer: Offer,
    entries: Entry[],
    sink: TransferSink,
    policy: ConflictPolicy,
    emit: TransferListener,
    { fallback, skipAsking }: ReceiveOptions,
    use: (sink: TransferSink) => void,
): Promise<void> {
    let { plan, files } = await planFor(peer, sink, offer, entries, policy);
    const asking: Asking = {
        start: () => emit({ type: "askingStorage" }),
        skipped: skipAsking ?? new Promise(() => {}),
    };
    try {
        const remaining = files.reduce(
            (n, { id, offset }) => n + entrySize(entries[id]) - offset,
            0,
        );
        await Promise.race([sink.reserve(remaining, asking), peer.stopped]);
    } catch (err) {
        if (!(err instanceof NotEnoughRoom) || !fallback) throw err;
        sink = fallback();
        use(sink);
        emit({ type: "heldInMemory" });
        ({ plan, files } = await planFor(peer, sink, offer, entries, policy));
    }
    // Before accepting, as the sender then waits for acks only so long.
    for (const { id, offset } of files) {
        if (offset > 0) {
            await Promise.race([sink.reopen(entries[id].path, offset), peer.stopped]);
        }
    }
    const algorithm = offer.checksums;
    const receiving = files.some(({ id, offset }) => entrySize(entries[id]) > offset);
    const codec = receiving || algorithm ? await createCodec() : undefined;
    try {
        if (algorithm) {
            const checked = verifyPresent(
                codec!,
                algorithm,
                entries,
                plan.complete,
                files,
                (path) => sink.read(path),
                (done, total) => emit({ type: "verifying", done, total }),
                sink.mismatchAdvice,
            );
            await Promise.race([checked, peer.stopped]);
        }
        peer.send({ type: "accept", files, window: WINDOW });
        emit(transferring(offer, entries, files));
        if (plan.conflicts.length > 0) {
            const replaced =
                policy === "replace"
                    ? plan.conflicts.filter((conflict) => conflict.replaceable).length
                    : 0;
            emit({
                type: "conflicts",
                replaced,
                skipped: plan.conflicts.length - replaced,
            });
        }
        for (const id of plan.dirs) await sink.createDir(entries[id].path);
        await receiveData(peer, sink, entries, files, codec, algorithm, emit);
    } finally {
        codec?.close();
    }

    peer.send({ type: "done" });
    await peer.goodbye();
}

/** What to receive into `sink`, given what it already holds. */
async function planFor(
    peer: ReceiverPeer,
    sink: TransferSink,
    offer: Offer,
    entries: Entry[],
    policy: ConflictPolicy,
) {
    await sink.prepare(offer.entries);
    const local: Local[] = [];
    for (const entry of entries) local.push(await sink.probe(entry));
    const checked = filesToCheck(entries, local);
    const verified = checked.length === 0 ? [] : await verify(peer, sink, entries, checked);
    const plan = planReceive(entries, local, verified);
    return { plan, files: acceptPlan(plan, policy) };
}

/** Writes the accepted files from the data stream into `sink`, acking as it goes. */
async function receiveData(
    peer: ReceiverPeer,
    sink: TransferSink,
    entries: Entry[],
    files: AcceptedFile[],
    codec: BlockCodec | undefined,
    algorithm: Algorithm | undefined,
    emit: TransferListener,
): Promise<void> {
    const topOf = topIndices(entries);
    // A copy goes to the file's hasher, as the sink may keep the bytes it is given.
    const hash = (id: number, bytes: Uint8Array) => {
        if (algorithm && bytes.length > 0) {
            codec!.hash(id, algorithm, bytes.slice()).catch(() => {});
        }
    };
    const check = async (id: number) => {
        if (algorithm) {
            await expectChecksum(codec!, algorithm, entries[id], id, sink.mismatchAdvice);
        }
    };
    let written = 0;
    let acked = 0;
    let ackedAt = performance.now();
    // File bytes count as their blocks arrive, in proportion.
    const progress = new Progress(entries, files);
    let decoded = 0;
    let arrived = 0;
    const blocks: Bytes[] = [];
    let empty = true;
    const nextBlock = async () => {
        while (blocks.length === 0) {
            const first = await peer.receive(IDLE_TIMEOUT_MS, SENDER_IDLE);
            // The sender says nothing while it sends.
            if (!(first instanceof Uint8Array)) throw unexpected(first);
            const chunks = [first];
            for (let more = peer.pollData(); more; more = peer.pollData()) {
                chunks.push(more);
            }
            let batch;
            try {
                batch = await codec!.decode(chunks);
            } catch (err) {
                throw protocolError(messageOf(err));
            }
            blocks.push(...batch.blocks);
            empty = batch.empty;
            decoded += batch.blocks.reduce((n, block) => n + block.length, 0);
            const now = decoded + batch.partial;
            if (now > arrived) {
                for (const [id, n] of progress.advance(now - arrived)) {
                    emit({ type: "progress", entry: topOf[id], bytes: n });
                }
                arrived = now;
            }
        }
        return blocks.shift()!;
    };
    const wrote = (n: number) => {
        written += n;
        if (written - acked >= ACK_STEP || performance.now() - ackedAt >= ACK_INTERVAL_MS) {
            peer.send({ type: "ack", bytes: written });
            acked = written;
            ackedAt = performance.now();
        }
    };
    // Whole files being written.
    const writing: { bytes: number; done: Promise<void> }[] = [];
    const settle = async () => {
        const { bytes, done } = writing.shift()!;
        await done;
        wrote(bytes);
    };
    // The rest of a block that ends one file and starts the next.
    let pending: Bytes | undefined;
    for (const { id, offset } of files) {
        const entry = entries[id];
        const size = entrySize(entry);
        const rest = size - offset;
        if (rest > 0 && !pending) pending = await nextBlock();
        // A file that has arrived in full
        if (rest <= (pending?.length ?? 0)) {
            const data = pending?.subarray(0, rest) ?? new Uint8Array(0);
            if (pending) {
                pending = rest < pending.length ? pending.subarray(rest) : undefined;
            }
            hash(id, data);
            const done = Promise.all([sink.writeFile(entry.path, offset, data), check(id)]).then(
                () => {},
            );
            done.catch(() => {});
            writing.push({ bytes: rest, done });
            if (writing.length >= CONCURRENT_FILES) await settle();
            continue;
        }
        // Written as it arrives, once the files before it are done.
        while (writing.length > 0) await settle();
        await sink.open(entry.path, offset);
        for (let position = offset; position < size;) {
            const block = pending ?? (await nextBlock());
            const n = Math.min(block.length, size - position);
            const chunk = block.subarray(0, n);
            hash(id, chunk);
            await sink.write(chunk);
            pending = n < block.length ? block.subarray(n) : undefined;
            position += n;
            wrote(n);
        }
        await sink.closeFile();
        await check(id);
    }
    while (writing.length > 0) await settle();
    if (pending || blocks.length > 0 || !empty) {
        throw protocolError("the sender sent more bytes than accepted");
    }
}

/** Asks the sender for fingerprints of the `checked` files; returns the ids of those that match ours. */
async function verify(
    peer: ReceiverPeer,
    sink: TransferSink,
    entries: Entry[],
    checked: CheckedFile[],
): Promise<number[]> {
    peer.send({ type: "check", files: checked });
    const answer = await peer.next();
    if (answer.type !== "hashes") throw unexpected(answer);
    if (answer.hashes.length !== checked.length) {
        throw protocolError("the hashes do not match the check");
    }
    const verified = [];
    for (const [i, { id, len }] of checked.entries()) {
        const ours = await fingerprintOf(await sink.read(entries[id].path), len);
        if (ours === answer.hashes[i]) verified.push(id);
    }
    return verified;
}
