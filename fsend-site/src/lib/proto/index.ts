import init, * as wasm from "./pkg/fsend_proto_wasm";
import type {
  Action,
  ParsedCode,
  Sealed,
  Transport,
} from "./generated/protocol";

export * from "./generated/protocol";

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

/** Sans-IO pairing over the relay: feed it frames, drain `poll`. */
export class Rendezvous {
  private constructor(private inner: wasm.Rendezvous) {}

  static sender(transports: Transport[], app: string): Rendezvous {
    return new Rendezvous(
      wasm.Rendezvous.sender(JSON.stringify(transports), app),
    );
  }

  static receiver(
    code: string,
    transports: Transport[],
    app: string,
  ): Rendezvous {
    return new Rendezvous(
      wasm.Rendezvous.receiver(code, JSON.stringify(transports), app),
    );
  }

  text(frame: string): void {
    this.inner.text(frame);
  }

  binary(frame: Uint8Array): void {
    this.inner.binary(frame);
  }

  closed(): void {
    this.inner.closed();
  }

  sendSealed(message: Sealed): void {
    this.inner.sendSealed(JSON.stringify(message));
  }

  poll(): Action | undefined {
    const action: Uint8Array | string | undefined = this.inner.poll();
    if (action instanceof Uint8Array)
      return { type: "send_binary", data: action };
    return action === undefined ? undefined : JSON.parse(action);
  }
}
