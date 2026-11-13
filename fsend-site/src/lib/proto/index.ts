import init, * as wasm from "./pkg/fsend_proto_wasm";
import type { PairingError, ParsedCode, Sealed } from "./generated/protocol";

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
