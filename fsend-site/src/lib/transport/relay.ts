import { onAbort } from "../abort";
import type { ClientMessage, ServerMessage, ConnectionInfo } from "../types";

export class RelayClient {
  private queue: ServerMessage[] = [];
  private waiters: Array<(msg: ServerMessage) => void> = [];
  private fail: (err: Error) => void = () => {};
  /** Rejects once the connection is gone. */
  private readonly lost: Promise<never>;

  private constructor(private readonly ws: WebSocket) {
    this.lost = new Promise<never>((_, reject) => (this.fail = reject));
    this.lost.catch(() => {});
    ws.onmessage = (ev) => {
      const msg: ServerMessage = JSON.parse(ev.data);
      const waiter = this.waiters.shift();
      if (waiter) waiter(msg);
      else this.queue.push(msg);
    };
    ws.onclose = () => this.fail(new Error("Lost the connection to the relay"));
  }

  /** Connects to the relay; aborting closes the connection. */
  static async connect(url: string, abort: AbortSignal): Promise<RelayClient> {
    const relay = new RelayClient(new WebSocket(url));
    const opened = new Promise<void>((resolve, reject) => {
      relay.ws.onopen = () => resolve();
      relay.ws.onerror = () => reject(new Error("Failed to connect to relay"));
    });
    onAbort(abort, () => relay.close());
    await Promise.race([opened, relay.lost]);
    return relay;
  }

  private send(msg: ClientMessage): void {
    this.ws.send(JSON.stringify(msg));
  }

  private recv(): Promise<ServerMessage> {
    const queued = this.queue.shift();
    if (queued) return Promise.resolve(queued);
    const next = new Promise<ServerMessage>((resolve) => {
      this.waiters.push(resolve);
    });
    return Promise.race([next, this.lost]);
  }

  async createSession(): Promise<string> {
    this.send({ type: "create_session", capabilities: ["web_rtc"] });
    const msg = await this.recv();
    if (msg.type === "error") throw new Error(msg.message);
    if (msg.type !== "create_session")
      throw new Error(`unexpected: ${msg.type}`);
    return msg.code;
  }

  async joinSession(code: string): Promise<void> {
    this.send({ type: "join_session", code, capabilities: ["web_rtc"] });
    const msg = await this.recv();
    if (msg.type === "error") throw new Error(msg.message);
    if (msg.type !== "join_session") throw new Error(`unexpected: ${msg.type}`);
  }

  async waitForPeer(): Promise<void> {
    const msg = await this.recv();
    if (msg.type === "error") throw new Error(msg.message);
    if (msg.type !== "peer_joined") throw new Error(`unexpected: ${msg.type}`);
  }

  sendExchange(info: ConnectionInfo): void {
    this.send({ type: "exchange", connection_info: info });
  }

  async recvExchange(): Promise<ConnectionInfo> {
    const msg = await this.recv();
    if (msg.type === "error") throw new Error(msg.message);
    if (msg.type !== "exchange") throw new Error(`unexpected: ${msg.type}`);
    return msg.connection_info;
  }

  close(): void {
    this.ws.close();
    this.fail(new Error("Closed the connection to the relay"));
  }
}
