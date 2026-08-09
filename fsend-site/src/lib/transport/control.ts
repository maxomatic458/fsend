import { sendControlMessage, ControlDecoder } from "./protocol";
import { flushChannel } from "./webrtc";


// The control channel as a typed request/response stream
export interface ControlChannel<In, Out> {
  /// Resolves with the next message, or rejects if the peer disconnects.
  next(): Promise<In>;
  send(message: Out): Promise<void>;
  /// Drain queued data
  flush(): Promise<void>;
}

export function createControlChannel<In, Out>(
  channel: RTCDataChannel,
  disconnected: Promise<never>,
): ControlChannel<In, Out> {
  const decoder = new ControlDecoder();
  const queued: In[] = [];
  let notify: (() => void) | null = null;

  channel.onmessage = async (ev) => {
    const data =
      ev.data instanceof ArrayBuffer ? ev.data : await ev.data.arrayBuffer();
    const message = await decoder.onMessage(data);
    if (message) {
      queued.push(message as In);
      notify?.();
    }
  };

  return {
    next() {
      const pending =
        queued.length > 0
          ? Promise.resolve(queued.shift()!)
          : new Promise<In>((resolve) => {
              notify = () => {
                notify = null;
                resolve(queued.shift()!);
              };
            });
      return Promise.race([pending, disconnected]);
    },
    send(message) {
      return sendControlMessage(channel, message as any);
    },
    flush() {
      return flushChannel(channel);
    },
  };
}
