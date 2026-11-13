import init, * as wasm from "./pkg/fsend_proto_wasm";
import type {
  AcceptedFile,
  CheckedFile,
  ConflictPolicy,
  Entry,
  Fingerprint,
  Local,
  PairingError,
  ParsedCode,
  Plan,
  ReceiverMessage,
  Sealed,
  SenderMessage,
  ServerMessage,
  TopEntry,
} from "./generated/protocol";

export * from "./generated/protocol";
export { Role } from "./pkg/fsend_proto_wasm";

let loading: Promise<unknown> | undefined;

/** Loads the wasm module; every other export needs it loaded. */
export async function loadProto(): Promise<void> {
  await (loading ??= init());
}

/** A code, checked and upper-cased. */
export const parseCode = (input: string): ParsedCode =>
  JSON.parse(wasm.parseCode(input));

/** The share link for `code` on `site`. */
export const codeLink = (site: string, code: string): string =>
  wasm.codeLink(site, code);

/** A fresh code for the nameplate the relay assigned, if it is valid. */
export const newCode = (nameplate: string): string | undefined =>
  wasm.newCode(nameplate);

/** A relay text frame; throws unless it is a valid `ServerMessage`. */
export const parseServerMessage = (text: string): ServerMessage =>
  JSON.parse(wasm.parseServerMessage(text));

const PAIRING_ERRORS: Record<PairingError, string> = {
  wrong_code:
    "The codes did not match. A code works only once, so start a new transfer and try again.",
  invalid_pake: "Pairing failed: the other side sent an invalid key exchange.",
  tampered: "Pairing failed: a message from the other side did not open.",
  malformed: "Pairing failed: the other side sent a malformed message.",
  unexpected_signal: "Pairing failed: unexpected message from the other side.",
};

export class PairingFailure extends Error {
  constructor(readonly error: PairingError) {
    super(PAIRING_ERRORS[error]);
  }
}

/** Rethrows the wasm's JSON `PairingError` as a {@link PairingFailure}. */
function pairing<T>(call: () => T): T {
  try {
    return call();
  } catch (error) {
    throw new PairingFailure(JSON.parse((error as Error).message));
  }
}

/** SPAKE2 for a code; takes and returns whole relay frames, like {@link Channel}. */
export class Pake {
  private constructor(private inner: wasm.Pake) {}

  static start(code: string): Pake {
    return new Pake(wasm.Pake.start(code));
  }

  /** The first frame for the peer. */
  get message(): Uint8Array {
    return this.inner.message;
  }

  finish(frame: Uint8Array, role: wasm.Role): Channel {
    return new Channel(pairing(() => this.inner.finish(frame, role)));
  }
}

/** Seals messages for the peer and opens theirs, in order. */
export class Channel {
  constructor(private inner: wasm.Channel) {}

  seal(message: Sealed): Uint8Array {
    return this.inner.seal(JSON.stringify(message));
  }

  open(frame: Uint8Array): Sealed {
    return JSON.parse(pairing(() => this.inner.open(frame)));
  }
}

/** A message of either side. */
export type PeerMessage = SenderMessage | ReceiverMessage;

/** One control frame of `message`; throws unless `from` sends it. */
export const encodeFrame = (
  message: PeerMessage,
  from: wasm.Role,
): Uint8Array => wasm.encodeFrame(JSON.stringify(message), from);

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

/** A data stream block of `data`, 1 to `MAX_BLOCK_BYTES` file bytes, compressed if that helps. */
export const encodeBlock = (data: Uint8Array): Uint8Array =>
  wasm.encodeBlock(data, true);

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
  next(): Uint8Array | undefined {
    return this.inner.nextBlock();
  }
}

/** Turns counts of data stream bytes into progress per top-level entry, in stream order. */
export class Progress {
  private inner: wasm.Progress;

  constructor(entries: Entry[], files: AcceptedFile[], topOf: number[]) {
    this.inner = new wasm.Progress(
      JSON.stringify(entries),
      JSON.stringify(files),
      Uint32Array.from(topOf),
    );
  }

  /** The bytes of each top-level entry among the next `n`, as `[entry, bytes]`. */
  advance(n: number): [number, number][] {
    const flat = this.inner.advance(n);
    const steps: [number, number][] = [];
    for (let i = 0; i < flat.length; i += 2) steps.push([flat[i], flat[i + 1]]);
    return steps;
  }
}

/** The highest protocol version both sides speak. */
export const negotiate = (
  version: number,
  minVersion: number,
): number | undefined => wasm.negotiate(version, minVersion);

/** Throws unless `entries` follows the offer rules. */
export const validateOffer = (entries: Entry[]): void =>
  wasm.validateOffer(JSON.stringify(entries));

/** The control frame of an offer of `entries`; throws unless they follow the offer rules. */
export const encodeOffer = (entries: Entry[]): Uint8Array =>
  wasm.encodeOffer(JSON.stringify(entries));

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
export const planReceive = (
  entries: Entry[],
  local: Local[],
  verified: number[],
): Plan =>
  JSON.parse(
    wasm.planReceive(
      JSON.stringify(entries),
      JSON.stringify(local),
      Uint32Array.from(verified),
    ),
  );

/** The files to accept under `policy`. */
export const acceptPlan = (
  plan: Plan,
  policy: ConflictPolicy,
): AcceptedFile[] =>
  JSON.parse(wasm.acceptPlan(JSON.stringify(plan), JSON.stringify(policy)));

/** The top-level entries of a valid offer, and the index of the one each entry lies in. */
export const topEntries = (entries: Entry[]): [TopEntry[], number[]] =>
  JSON.parse(wasm.topEntries(JSON.stringify(entries)));

/** Bytes of each top-level entry already present once `files` are accepted. */
export const presentBytes = (
  entries: Entry[],
  files: AcceptedFile[],
): number[] =>
  JSON.parse(wasm.presentBytes(JSON.stringify(entries), JSON.stringify(files)));
