import {
  compressGzip,
  decompressGzip,
  frame,
  messages,
  FrameDecoder,
} from "./compression";
import type { SenderToReceiver, ReceiverToSender } from "../types";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export async function sendControlMessage(
  channel: RTCDataChannel,
  msg: SenderToReceiver | ReceiverToSender,
): Promise<void> {
  const json = encoder.encode(JSON.stringify(msg));
  const compressed = await compressGzip(json);
  for (const message of messages(frame(compressed))) {
    channel.send(message as any);
  }
}

export class ControlDecoder {
  private frames = new FrameDecoder();

  /// The control messages completed by this data channel message.
  async onMessage(
    data: ArrayBuffer,
  ): Promise<(SenderToReceiver | ReceiverToSender)[]> {
    const out: (SenderToReceiver | ReceiverToSender)[] = [];
    for (const payload of this.frames.push(new Uint8Array(data))) {
      const decompressed = await decompressGzip(payload);
      out.push(JSON.parse(decoder.decode(decompressed)));
    }
    return out;
  }
}
