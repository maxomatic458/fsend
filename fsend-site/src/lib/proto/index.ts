import init, * as wasm from "./pkg/fsend_proto_wasm";
import type {
    AcceptedFile,
    Algorithm,
    CheckedFile,
    ConflictPolicy,
    Entry,
    Fingerprint,
    Local,
    Offer,
    PairingError,
    ParsedCode,
    Plan,
    ReceiverMessage,
    Sealed,
    SenderMessage,
    ServerMessage,
} from "./generated/protocol";

export * from "./generated/protocol";
export { Role } from "./pkg/fsend_proto_wasm";

let loading: Promise<unknown> | undefined;

/** Loads the wasm module; every other export needs it loaded. */
export async function loadProto(): Promise<void> {
    // A failed load is tried again next time.
    await (loading ??= init().catch((err) => {
        loading = undefined;
        throw err;
    }));
}

/** A code, checked and upper-cased. */
export const parseCode = (input: string): ParsedCode => JSON.parse(wasm.parseCode(input));

/** The share link for `code` on `site`. */
export const codeLink = (site: string, code: string): string => wasm.codeLink(site, code);

/** A fresh code for the nameplate the relay assigned, if it is valid. */
export const newCode = (nameplate: string): string | undefined => wasm.newCode(nameplate);

/** A relay text frame; throws unless it is a valid `ServerMessage`. */
export const parseServerMessage = (text: string): ServerMessage =>
    JSON.parse(wasm.parseServerMessage(text));

const PAIRING_ERRORS: Record<PairingError, string> = {
    wrong_code:
        "The codes did not match. A code works only once, so start a new transfer and try again.",
    invalid_pake: "Pairing failed: the other side sent an invalid key exchange.",
    tampered: "Pairing failed: a message from the other side did not open.",
    malformed: "Pairing failed: the other side sent a malformed message.",
    too_large: "Pairing failed: a message from the other side was too large.",
    unexpected_signal: "Pairing failed: unexpected message from the other side.",
};

/** Rethrows the wasm's JSON `PairingError` with a message for the user. */
function pairing<T>(call: () => T): T {
    try {
        return call();
    } catch (error) {
        const code: PairingError = JSON.parse((error as Error).message);
        throw new Error(PAIRING_ERRORS[code]);
    }
}

/** SPAKE2 for a code; takes and returns whole relay frames, like {@link Channel}. */
export class Pake {
    private constructor(private inner: wasm.Pake) {}

    static start(code: string): Pake {
        return new Pake(wasm.Pake.start(code));
    }

    /** The first frame for the peer. */
    get message(): Bytes {
        return this.inner.message as Bytes;
    }

    finish(frame: Uint8Array, role: wasm.Role): Channel {
        return new Channel(pairing(() => this.inner.finish(frame, role)));
    }
}

/** Seals messages for the peer and opens theirs, in order; a large one takes several frames. */
export class Channel {
    constructor(private inner: wasm.Channel) {}

    seal(message: Sealed): Bytes[] {
        return this.inner.seal(JSON.stringify(message)) as Bytes[];
    }

    /** The frames of a plaintext from {@link encodeOffer}. */
    sealPlaintext(plaintext: Uint8Array): Bytes[] {
        return this.inner.sealPlaintext(plaintext) as Bytes[];
    }

    /** The message once `frame` completes one. */
    open(frame: Uint8Array): Sealed | undefined {
        const message = pairing(() => this.inner.open(frame));
        return message === undefined ? undefined : JSON.parse(message);
    }
}

/** A message of either side. */
export type PeerMessage = SenderMessage | ReceiverMessage;

/** One control frame of `message`; throws unless `from` sends it. */
export const encodeFrame = (message: PeerMessage, from: wasm.Role): Bytes =>
    wasm.encodeFrame(JSON.stringify(message), from) as Bytes;

/** Reassembles the control messages `from` sends, as `M`, from bytes split anywhere. */
export class FrameDecoder<M extends PeerMessage> {
    private inner: wasm.FrameDecoder;

    constructor(from: wasm.Role) {
        this.inner = new wasm.FrameDecoder(from);
    }

    push(data: Uint8Array): void {
        this.inner.push(data);
    }

    /** The next complete message; throws on a malformed frame or a message `from` does not send. */
    next(): M | undefined {
        const message = this.inner.nextMessage();
        return message === undefined ? undefined : JSON.parse(message);
    }
}

export type Bytes = Uint8Array<ArrayBuffer>;

/** A data stream block of `data`, 1 to `MAX_BLOCK_BYTES` file bytes, compressed if that helps. */
export const encodeBlock = (data: Uint8Array): Bytes => wasm.encodeBlock(data, true) as Bytes;

/** The CRC-32 of `data` following bytes whose CRC-32 is `previous`. */
export const crc32 = (data: Uint8Array, previous: number): number => wasm.crc32(data, previous);

/** Reassembles the file bytes of blocks from data stream bytes split anywhere. */
export class BlockDecoder {
    private inner = new wasm.BlockDecoder();

    push(data: Uint8Array): void {
        this.inner.push(data);
    }

    /** Whether no partial block is buffered. */
    isEmpty(): boolean {
        return this.inner.isEmpty();
    }

    /** The file bytes of the first buffered block, in proportion to how much of it arrived. */
    partial(): number {
        return this.inner.partial();
    }

    /** The file bytes of the next complete block; throws on a malformed one. */
    next(): Bytes | undefined {
        return this.inner.nextBlock() as Bytes | undefined;
    }
}

/** Turns counts of data stream bytes into progress per accepted file, in stream order. */
export class Progress {
    private inner: wasm.Progress;

    constructor(entries: Entry[], files: AcceptedFile[]) {
        this.inner = new wasm.Progress(JSON.stringify(entries), JSON.stringify(files));
    }

    /** The bytes of each file among the next `n`, as `[id, bytes]`. */
    advance(n: number): [number, number][] {
        const flat = this.inner.advance(n);
        const steps: [number, number][] = [];
        for (let i = 0; i < flat.length; i += 2) steps.push([flat[i], flat[i + 1]]);
        return steps;
    }
}

/** How each checksum algorithm is named to the user. */
export const ALGORITHM_NAMES: Record<Algorithm, string> = { sha256: "SHA-256" };

/** Hashes a file's bytes as they come. */
export class Hasher {
    private inner: wasm.Hasher;

    constructor(algorithm: Algorithm) {
        this.inner = new wasm.Hasher(algorithm);
    }

    update(data: Uint8Array): void {
        this.inner.update(data);
    }

    /** The digest as lowercase hex; the hasher is spent. */
    finish(): string {
        return this.inner.finish();
    }
}

/** Throws unless `offer` follows the offer rules. */
export const validateOffer = (offer: Offer): void => wasm.validateOffer(JSON.stringify(offer));

/** The sealed plaintext of `offer`; throws unless it follows the offer rules. */
export const encodeOffer = (offer: Offer): Bytes =>
    wasm.encodeOffer(JSON.stringify(offer)) as Bytes;

/** Every entry of `offer` with its path, each folder before what it holds; an entry's id is its index. */
export const flattenOffer = (offer: Offer): Entry[] =>
    JSON.parse(wasm.flattenOffer(JSON.stringify(offer)));

/** Throws unless `files` names files of `entries`, in order, with offsets within them. */
export const checkAccept = (entries: Entry[], files: AcceptedFile[]): void =>
    wasm.checkAccept(JSON.stringify(entries), JSON.stringify(files));

/** Throws unless `files` names files of `entries`, in order, with lengths within them. */
export const checkLengths = (entries: Entry[], files: CheckedFile[]): void =>
    wasm.checkLengths(JSON.stringify(entries), JSON.stringify(files));

/** The byte ranges of a file's first `len` bytes that are fingerprinted. */
export const fingerprintRanges = (len: number): [number, number][] =>
    JSON.parse(wasm.fingerprintRanges(len));

/** The fingerprint of the bytes in {@link fingerprintRanges}, concatenated. */
export const fingerprint = (sampled: Uint8Array): Fingerprint =>
    JSON.parse(wasm.fingerprint(sampled));

/** The local files, given what is at each entry's path, that may be a prefix of the offered ones. */
export const filesToCheck = (entries: Entry[], local: Local[]): CheckedFile[] =>
    JSON.parse(wasm.filesToCheck(JSON.stringify(entries), JSON.stringify(local)));

/** What to do with each entry, given the ids of the checked files whose fingerprints matched. */
export const planReceive = (entries: Entry[], local: Local[], verified: number[]): Plan =>
    JSON.parse(
        wasm.planReceive(
            JSON.stringify(entries),
            JSON.stringify(local),
            Uint32Array.from(verified),
        ),
    );

/** The files to accept under `policy`. */
export const acceptPlan = (plan: Plan, policy: ConflictPolicy): AcceptedFile[] =>
    JSON.parse(wasm.acceptPlan(JSON.stringify(plan), JSON.stringify(policy)));
