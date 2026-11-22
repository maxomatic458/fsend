import { CONCURRENT_FILES, MAX_BUFFERED_BYTES } from "../../config";
import { openSenderSession, type Link } from "../transport/session";
import { dataMessageBytes, getConnectionType } from "../transport/webrtc";
import { buildOffer, entrySize, fingerprintOf, topIndices, type Source } from "../files/tree";
import {
    IDLE_TIMEOUT_MS,
    MAX_BLOCK_BYTES,
    MIN_WINDOW,
    checkAccept,
    checkLengths,
    Progress,
    encodeOffer,
    flattenOffer,
    loadProto,
    type AcceptedFile,
    type Algorithm,
    type Bytes,
    type Entry,
    type Offer,
    type ReceiverMessage,
} from "../proto";
import {
    Peer,
    TransferFailure,
    type SenderPeer,
    messageOf,
    protocolError,
    transferring,
    unexpected,
} from "./peer";
import type { TransferListener } from "./events";
import { eachAtOnce } from "../parallel";
import { createCodec } from "./codec";
import type { SelectedEntry } from "../types";
import { addChecksums } from "./checksums";

const RECEIVER_IDLE = "the receiver stopped responding";

/** The offer, its entries by id, and the source of each. */
interface Sending {
    offer: Offer;
    entries: Entry[];
    sources: (Source | null)[];
}
/// Blocks compressing while one is sent.
const ENCODING_AHEAD = 3;

export interface SendOptions {
    /// Offers a checksum of every file under this algorithm, reading every file first.
    checksums?: Algorithm;
}

export async function runSend(
    entries: SelectedEntry[],
    emit: TransferListener,
    abort: AbortSignal,
    { checksums }: SendOptions = {},
): Promise<void> {
    let link: Link | null = null;

    try {
        const { offer, files: sources } = await buildOffer(entries);
        await loadProto();
        if (checksums) {
            const onProgress = (done: number, total: number) =>
                emit({ type: "hashing", done, total });
            await addChecksums(offer, sources, checksums, onProgress, abort);
        }
        let plaintext: Bytes;
        try {
            plaintext = encodeOffer(offer);
        } catch (err) {
            throw new Error(`Cannot send these files: ${messageOf(err)}`);
        }
        const sending = { offer, entries: flattenOffer(offer), sources };

        link = await openSenderSession(plaintext, abort, {
            onCode: (code, expiresInSecs) => emit({ type: "code", code, expiresInSecs }),
            onOffered: () => emit({ type: "waitingAccept" }),
            onAccepted: () => emit({ type: "handshaking" }),
        });
        const { dataChannel } = link;

        // cosmetic: fetched in the background, ignored if it fails.
        getConnectionType(link.pc)
            .then((kind) => emit({ type: "connectionType", kind }))
            .catch(() => {});

        const peer = Peer.sender(link, abort);
        const messageBytes = dataMessageBytes(link.pc);
        await peer.explained(() => send(peer, dataChannel, messageBytes, sending, emit));
        emit({ type: "complete" });
    } catch (err) {
        if (!abort.aborted) emit({ type: "error", message: messageOf(err) });
    } finally {
        link?.close();
    }
}

async function send(
    peer: SenderPeer,
    channel: RTCDataChannel,
    messageBytes: number,
    { offer, entries, sources }: Sending,
    emit: TransferListener,
): Promise<void> {
    // No timeout: the receiver's user may be deciding about storage or conflicts.
    let answer = await peer.next();
    if (answer.type === "check") {
        try {
            checkLengths(entries, answer.files);
        } catch (err) {
            throw protocolError(messageOf(err));
        }
        const hashes = [];
        for (const { id, len } of answer.files) {
            hashes.push(await fingerprintOf(sources[id]!, len));
        }
        peer.send({ type: "hashes", hashes });
        answer = await peer.next();
    }
    if (answer.type !== "accept") throw unexpected(answer);
    const { files, window } = answer;
    if (window < MIN_WINDOW) throw protocolError("the receiver's window is too small");
    try {
        checkAccept(entries, files);
    } catch (err) {
        throw protocolError(messageOf(err));
    }
    emit(transferring(offer, entries, files));
    const topOf = topIndices(entries);

    let sent = 0;
    let acked = 0;
    const ack = (message: ReceiverMessage) => {
        if (message.type !== "ack") throw unexpected(message);
        if (message.bytes < acked || message.bytes > sent) {
            throw protocolError("an ack outside the bytes sent");
        }
        acked = message.bytes;
    };

    channel.bufferedAmountLowThreshold = MAX_BUFFERED_BYTES / 2;
    const reader = new BlockReader(entries, sources, files);
    const progress = new Progress(entries, files);
    const codec = reader.done ? undefined : await createCodec();
    try {
        const encoding: { size: number; frame: Promise<Bytes> }[] = [];
        while (!reader.done || encoding.length > 0) {
            for (let m = peer.poll(); m; m = peer.poll()) ack(m);
            while (encoding.length < ENCODING_AHEAD && !reader.done && sent - acked < window) {
                const bytes = await reader.read(window - (sent - acked));
                sent += bytes.length;
                const frame = codec!.encode(bytes);
                frame.catch(() => {});
                encoding.push({ size: bytes.length, frame });
            }
            const next = encoding.shift();
            if (!next) {
                ack(await peer.next(IDLE_TIMEOUT_MS, RECEIVER_IDLE));
                continue;
            }
            await sendBlock(next.size, await next.frame);
        }
    } finally {
        codec?.close();
    }

    /** Waits for the channel to take more, as long as the receiver keeps reading. */
    async function drained() {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const idle = new Promise((_, reject) => {
            timer = setTimeout(() => {
                const report = { code: "timeout", message: RECEIVER_IDLE } as const;
                reject(new TransferFailure(`Timed out: ${RECEIVER_IDLE}`, report));
            }, IDLE_TIMEOUT_MS);
        });
        try {
            await Promise.race([
                new Promise((resolve) => (channel.onbufferedamountlow = resolve)),
                peer.stopped,
                idle,
            ]);
        } finally {
            clearTimeout(timer);
        }
    }

    /** Sends a block of `size` file bytes, which count in proportion to its bytes sent. */
    async function sendBlock(size: number, block: Bytes) {
        let counted = 0;
        for (let offset = 0; offset < block.length; offset += messageBytes) {
            if (channel.bufferedAmount > MAX_BUFFERED_BYTES) await drained();
            const end = Math.min(offset + messageBytes, block.length);
            channel.send(block.subarray(offset, end));
            const now = Math.floor((size * end) / block.length);
            for (const [id, n] of progress.advance(now - counted)) {
                emit({ type: "progress", entry: topOf[id], bytes: n });
            }
            counted = now;
        }
    }

    for (;;) {
        const message = await peer.next(IDLE_TIMEOUT_MS, RECEIVER_IDLE);
        if (message.type === "done") return;
        ack(message);
    }
}

/** Reads the accepted bytes of the offered files in order, a block at a time. */
class BlockReader {
    /** Bytes still to read. */
    private left: number;
    private file = 0;
    private position: number;

    constructor(
        private entries: Entry[],
        private sources: (Source | null)[],
        private files: AcceptedFile[],
    ) {
        this.left = files.reduce((n, { id, offset }) => n + entrySize(entries[id]) - offset, 0);
        this.position = files[0]?.offset ?? 0;
    }

    get done(): boolean {
        return this.left === 0;
    }

    /** Up to `max` (and a block's worth of) bytes, reading a few files at once. */
    async read(max: number): Promise<Uint8Array> {
        const length = Math.min(max, this.left, MAX_BLOCK_BYTES);
        this.left -= length;
        // Where each file's part of the block comes from and goes.
        const slices: { source: Source; start: number; end: number; at: number }[] = [];
        for (let filled = 0; filled < length;) {
            const { id } = this.files[this.file];
            const size = entrySize(this.entries[id]);
            if (this.position === size) {
                this.position = this.files[++this.file].offset;
                continue;
            }
            const n = Math.min(size - this.position, length - filled);
            const start = this.position;
            slices.push({
                source: this.sources[id]!,
                start,
                end: start + n,
                at: filled,
            });
            filled += n;
            this.position += n;
        }
        const sliceBytes = async ({ source, start, end }: (typeof slices)[0]) => {
            const bytes = new Uint8Array(await source.slice(start, end).arrayBuffer());
            if (bytes.length !== end - start) throw new Error("A file is shorter than expected");
            return bytes;
        };
        // Part of one file: its bytes are the block.
        if (slices.length === 1) return sliceBytes(slices[0]);
        const bytes = new Uint8Array(length);
        await eachAtOnce(slices, CONCURRENT_FILES, async (slice) =>
            bytes.set(await sliceBytes(slice), slice.at),
        );
        return bytes;
    }
}
