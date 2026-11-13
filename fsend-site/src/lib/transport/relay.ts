import { RELAY_URL } from "../../config";
import type {
  ConnectionInfo,
  Event,
  Rendezvous,
  RendezvousError,
} from "../proto";

/** Drives a Rendezvous over a WebSocket to the relay. */
export class RelayConnection {
  private ws: WebSocket;
  private opened = false;
  private outbox: (string | Uint8Array)[] = [];
  private events: Event[] = [];
  private failure: Error | null = null;
  private wake: (() => void) | null = null;

  constructor(private rendezvous: Rendezvous) {
    this.ws = new WebSocket(`${RELAY_URL.replace(/\/+$/, "")}/relay`);
    this.ws.binaryType = "arraybuffer";
    this.ws.onopen = () => {
      this.opened = true;
      for (const frame of this.outbox.splice(0)) this.ws.send(frame);
    };
    this.ws.onmessage = (ev) => {
      if (typeof ev.data === "string") this.rendezvous.text(ev.data);
      else this.rendezvous.binary(new Uint8Array(ev.data));
      this.pump();
    };
    this.ws.onclose = () => {
      if (!this.opened) this.fail(new Error("Could not reach the relay"));
      this.rendezvous.closed();
      this.pump();
    };
    this.pump();
  }

  /** Resolves with the next event, rejects once pairing failed. */
  async next(): Promise<Event> {
    for (;;) {
      const event = this.events.shift();
      if (event) return event;
      if (this.failure) throw this.failure;
      await new Promise<void>((resolve) => (this.wake = resolve));
    }
  }

  async expect<T extends Event["type"]>(
    type: T,
  ): Promise<Extract<Event, { type: T }>> {
    const event = await this.next();
    if (event.type !== type) {
      throw new Error(`Unexpected ${event.type} event during pairing`);
    }
    return event as Extract<Event, { type: T }>;
  }

  sendConnection(info: ConnectionInfo): void {
    this.rendezvous.sendSealed({ type: "connection", info });
    this.pump();
  }

  async connection(): Promise<ConnectionInfo> {
    const { message } = await this.expect("sealed");
    if (message.type !== "connection") {
      throw new Error(`Unexpected ${message.type} message during pairing`);
    }
    return message.info;
  }

  close(): void {
    this.fail(new Error("The relay connection was closed"));
    this.ws.close();
  }

  private pump(): void {
    for (
      let action = this.rendezvous.poll();
      action;
      action = this.rendezvous.poll()
    ) {
      switch (action.type) {
        case "send":
          this.send(action.text);
          break;
        case "send_binary":
          this.send(action.data);
          break;
        case "event":
          this.events.push(action.event);
          break;
        case "failed":
          this.fail(new Error(describe(action.error)));
          this.ws.close();
          break;
      }
    }
    this.notify();
  }

  private send(frame: string | Uint8Array): void {
    if (this.opened) this.ws.send(frame);
    else this.outbox.push(frame);
  }

  private fail(error: Error): void {
    this.failure ??= error;
    this.notify();
  }

  private notify(): void {
    this.wake?.();
    this.wake = null;
  }
}

function describe(error: RendezvousError): string {
  switch (error.type) {
    case "wrong_code":
      return "The codes did not match. A code works only once, so start a new transfer and try again.";
    case "closed":
      return "Lost the connection to the relay.";
    case "protocol":
      return `Pairing failed: ${error.message}.`;
    case "relay":
      switch (error.code) {
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
        default:
          return `Relay error: ${error.message}.`;
      }
  }
}
