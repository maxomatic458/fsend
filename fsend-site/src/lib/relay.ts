import type { ClientMessage, ServerMessage, ConnectionInfo } from './types';

export class RelayClient {
  private ws: WebSocket;
  private queue: ServerMessage[] = [];
  private waiters: Array<(msg: ServerMessage) => void> = [];

  private constructor(ws: WebSocket) {
    this.ws = ws;
    ws.onmessage = (ev) => {
      const msg: ServerMessage = JSON.parse(ev.data);
      if (this.waiters.length > 0) {
        this.waiters.shift()!(msg);
      } else {
        this.queue.push(msg);
      }
    };
  }

  static connect(url: string): Promise<RelayClient> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      ws.onopen = () => resolve(new RelayClient(ws));
      ws.onerror = () => reject(new Error('Failed to connect to relay'));
    });
  }

  private send(msg: ClientMessage): void {
    this.ws.send(JSON.stringify(msg));
  }

  private recv(): Promise<ServerMessage> {
    if (this.queue.length > 0) {
      return Promise.resolve(this.queue.shift()!);
    }
    return new Promise((resolve) => {
      this.waiters.push(resolve);
    });
  }

  async createSession(): Promise<string> {
    this.send({ type: 'create_session', capabilities: ['web_rtc'] });
    const msg = await this.recv();
    if (msg.type === 'error') throw new Error(msg.message);
    if (msg.type !== 'create_session') throw new Error(`unexpected: ${msg.type}`);
    return msg.code;
  }

  async joinSession(code: string): Promise<void> {
    this.send({ type: 'join_session', code, capabilities: ['web_rtc'] });
    const msg = await this.recv();
    if (msg.type === 'error') throw new Error(msg.message);
    if (msg.type !== 'join_session') throw new Error(`unexpected: ${msg.type}`);
  }

  async waitForPeer(): Promise<void> {
    const msg = await this.recv();
    if (msg.type === 'error') throw new Error(msg.message);
    if (msg.type !== 'peer_joined') throw new Error(`unexpected: ${msg.type}`);
  }

  sendExchange(info: ConnectionInfo): void {
    this.send({ type: 'exchange', connection_info: info });
  }

  async recvExchange(): Promise<ConnectionInfo> {
    const msg = await this.recv();
    if (msg.type === 'error') throw new Error(msg.message);
    if (msg.type !== 'exchange') throw new Error(`unexpected: ${msg.type}`);
    return msg.connection_info;
  }

  close(): void {
    this.ws.close();
  }
}
