import init, * as wasm from "./pkg/fsend_proto_wasm";
import type {
  AcceptedFile,
  Entry,
  PairingError,
  ParsedCode,
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

/** Every file not complete yet, given the size of each entry already present. */
export const acceptFiles = (
  entries: Entry[],
  local: (number | null)[],
): AcceptedFile[] =>
  JSON.parse(wasm.acceptFiles(JSON.stringify(entries), JSON.stringify(local)));

/** The top-level entries of a valid offer, and the index of the one each entry lies in. */
export const topEntries = (entries: Entry[]): [TopEntry[], number[]] =>
  JSON.parse(wasm.topEntries(JSON.stringify(entries)));

/** Bytes of each top-level entry already present once `files` are accepted. */
export const presentBytes = (
  entries: Entry[],
  files: AcceptedFile[],
): number[] =>
  JSON.parse(wasm.presentBytes(JSON.stringify(entries), JSON.stringify(files)));
