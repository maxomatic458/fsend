import { compressGzip, decompressGzip, fragment, Defragmenter } from './compression';
import type { SenderToReceiver, ReceiverToSender } from './types';

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export async function sendControlMessage(
  channel: RTCDataChannel,
  msg: SenderToReceiver | ReceiverToSender,
): Promise<void> {
  const json = encoder.encode(JSON.stringify(msg));
  const compressed = await compressGzip(json);
  const fragments = fragment(compressed);
  for (const frag of fragments) {
    channel.send(frag as any);
  }
}

export class ControlDecoder {
  private defrag = new Defragmenter();

  async onMessage(data: ArrayBuffer): Promise<(SenderToReceiver | ReceiverToSender) | null> {
    const assembled = this.defrag.push(new Uint8Array(data));
    if (!assembled) return null;
    const decompressed = await decompressGzip(assembled);
    const json = decoder.decode(decompressed);
    return JSON.parse(json);
  }
}
