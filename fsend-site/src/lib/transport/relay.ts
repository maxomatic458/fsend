import { RELAY_URL } from "../../config";
import {
  NAMEPLATE_LENGTH,
  Pake,
  RELAY_VERSION,
  Role,
  newCode,
  parseServerMessage,
  type Channel,
  type ClientMessage,
  type ConnectionInfo,
  type ErrorCode,
  type ServerMessage,
  type Transport,
} from "../proto";

const APP = "fsend-web";
const TRANSPORTS: Transport[] = ["webrtc"];

type Reply = Exclude<ServerMessage, { type: "error" }>;

/** A WebSocket to the relay; pair it first, then swap connection info. */
export class Relay {
  private frames: (string | Uint8Array)[] = [];
  private closed = false;
  private wake: (() => void) | null = null;
  private channel: Channel | null = null;

  private constructor(private ws: WebSocket) {
    ws.onmessage = (ev) => {
      const data = ev.data;
      this.frames.push(typeof data === "string" ? data : new Uint8Array(data));
      this.notify();
    };
    ws.onclose = () => {
      this.closed = true;
      this.notify();
    };
  }

  /** Connects; aborting closes the connection, failing whatever waits on it. */
  static connect(abort: AbortSignal): Promise<Relay> {
    abort.throwIfAborted();
    const ws = new WebSocket(`${RELAY_URL.replace(/\/+$/, "")}/relay`);
    ws.binaryType = "arraybuffer";
    abort.addEventListener("abort", () => ws.close());
    return new Promise((resolve, reject) => {
      ws.onopen = () => resolve(new Relay(ws));
      ws.onclose = () => reject(new Error("Could not reach the relay"));
    });
  }

  /** Creates a session and pairs with whoever joins it. */
  async pairAsSender(
    onCode: (code: string, expiresInSecs: number) => void,
  ): Promise<void> {
    this.send({
      type: "create",
      version: RELAY_VERSION,
      transports: TRANSPORTS,
    });
    const { nameplate, expires_in } = await this.expect("created");
    const code = newCode(nameplate);
    if (!code) throw pairingError("the relay assigned an invalid nameplate");
    onCode(code, expires_in);
    await this.expect("paired");
    await this.handshake(code, Role.Sender);
  }

  async pairAsReceiver(code: string): Promise<void> {
    this.send({
      type: "join",
      version: RELAY_VERSION,
      nameplate: code.slice(0, NAMEPLATE_LENGTH),
      transports: TRANSPORTS,
    });
    await this.expect("paired");
    await this.handshake(code, Role.Receiver);
  }

  /** SPAKE2, then each side's sealed confirm proves both used the same code. */
  private async handshake(code: string, role: Role): Promise<void> {
    const pake = Pake.start(code);
    this.ws.send(pake.message);
    const channel = pake.finish(await this.signal(), role);
    this.ws.send(channel.seal({ type: "confirm", app: APP }));
    const confirm = channel.open(await this.signal());
    if (confirm.type !== "confirm") {
      throw pairingError("expected a confirm message");
    }
    this.channel = channel;
  }

  sendConnection(info: ConnectionInfo): void {
    this.ws.send(this.paired().seal({ type: "connection", info }));
  }

  async connection(): Promise<ConnectionInfo> {
    const message = this.paired().open(await this.signal());
    if (message.type !== "connection") {
      throw pairingError("repeated confirm message");
    }
    return message.info;
  }

  close(): void {
    this.ws.close();
  }

  private paired(): Channel {
    if (!this.channel) throw pairingError("not paired yet");
    return this.channel;
  }

  private send(message: ClientMessage): void {
    this.ws.send(JSON.stringify(message));
  }

  private async expect<T extends Reply["type"]>(
    type: T,
  ): Promise<Extract<Reply, { type: T }>> {
    const frame = await this.next();
    if (frame instanceof Uint8Array || frame.type !== type) {
      throw pairingError(`expected ${type}`);
    }
    return frame as Extract<Reply, { type: T }>;
  }

  private async signal(): Promise<Uint8Array> {
    const frame = await this.next();
    if (!(frame instanceof Uint8Array)) {
      throw pairingError(`unexpected ${frame.type} message`);
    }
    return frame;
  }

  /** The next signal or relay message; a relay error throws. */
  private async next(): Promise<Uint8Array | Reply> {
    for (;;) {
      const frame = this.frames.shift();
      if (frame instanceof Uint8Array) return frame;
      if (frame === undefined) {
        if (this.closed) throw new Error("Lost the connection to the relay.");
        await new Promise<void>((resolve) => (this.wake = resolve));
        continue;
      }
      const message = parse(frame);
      if (message.type === "error") {
        throw new Error(describe(message.code, message.message));
      }
      return message;
    }
  }

  private notify(): void {
    this.wake?.();
    this.wake = null;
  }
}

function parse(frame: string): ServerMessage {
  try {
    return parseServerMessage(frame);
  } catch {
    throw pairingError("invalid relay message");
  }
}

function pairingError(message: string): Error {
  return new Error(`Pairing failed: ${message}.`);
}

function describe(code: ErrorCode, message: string): string {
  switch (code) {
    case "not_found":
      return "No transfer with this code. Check the code; a code works only once.";
    case "expired":
      return "The code has expired. Start a new transfer.";
    case "version":
      return "This page is out of date. Reload it and try again.";
    case "no_common_transport":
      return "The other side cannot connect to a browser.";
    case "peer_left":
      return "The other side left.";
    case "busy":
      return "The relay is busy. Try again in a moment.";
    case "protocol":
      return `Relay error: ${message}.`;
  }
}
