import { DATA_CHUNK_BYTES, MAX_BUFFERED_BYTES } from "../../config";
import { openSenderSession, type Link } from "../transport/session";
import { getConnectionType } from "../transport/webrtc";
import {
  buildOffer,
  entrySize,
  fingerprintOf,
  type Outgoing,
  type Source,
} from "../files/tree";
import {
  IDLE_TIMEOUT_MS,
  MAX_BLOCK_BYTES,
  MIN_RECV_WINDOW,
  checkAccept,
  checkLengths,
  Progress,
  encodeOffer,
  loadProto,
  presentBytes,
  topEntries,
  type AcceptedFile,
  type Entry,
  type ReceiverMessage,
} from "../proto";
import {
  Peer,
  TransferFailure,
  type SenderPeer,
  messageOf,
  protocolError,
  transferring,
  unexpected,
} from "./peer";
import type { TransferListener } from "./events";
import { createCodec } from "./codec";
import type { SelectedEntry } from "../types";

const RECEIVER_IDLE = "the receiver stopped responding";
/// Blocks compressing while one is sent.
const ENCODING_AHEAD = 3;

export async function runSend(
  entries: SelectedEntry[],
  emit: TransferListener,
  abort: AbortSignal,
): Promise<void> {
  let link: Link | null = null;

  try {
    const outgoing = await buildOffer(entries);
    await loadProto();
    let offer: Uint8Array;
    try {
      offer = encodeOffer(outgoing.entries);
    } catch (err) {
      throw new Error(`Cannot send these files: ${messageOf(err)}`);
    }

    link = await openSenderSession(abort, {
      onCode: (code, expiresInSecs) =>
        emit({ type: "code", code, expiresInSecs }),
      onWaitingPeer: () => emit({ type: "waitingPeer" }),
      onHandshaking: () => emit({ type: "handshaking" }),
    });
    if (!link) return;
    const { dataChannel } = link;

    // cosmetic: fetched in the background, ignored if it fails.
    getConnectionType(link.pc)
      .then((kind) => emit({ type: "connectionType", kind }))
      .catch(() => {});

    const peer = Peer.sender(link, abort);
    await peer.explained(() => send(peer, dataChannel, outgoing, offer, emit));
    emit({ type: "complete" });
  } catch (err) {
    if (!abort.aborted) emit({ type: "error", message: messageOf(err) });
  } finally {
    link?.close();
  }
}

async function send(
  peer: SenderPeer,
  channel: RTCDataChannel,
  { entries, files: sources }: Outgoing,
  offer: Uint8Array,
  emit: TransferListener,
): Promise<void> {
  const window = await peer.hello("the receiver did not say hello in time");
  if (window === undefined || window < MIN_RECV_WINDOW) {
    throw protocolError("the receiver announced no usable window");
  }

  peer.sendFrame(offer);
  emit({ type: "waitingAccept" });
  let answer = await peer.next();
  if (answer.type === "check") {
    try {
      checkLengths(entries, answer.files);
    } catch (err) {
      throw protocolError(messageOf(err));
    }
    const hashes = [];
    for (const { id, len } of answer.files) {
      hashes.push(await fingerprintOf(sources[id]!, len));
    }
    peer.send({ type: "hashes", hashes });
    answer = await peer.next();
  }
  if (answer.type === "reject") {
    throw new TransferFailure("The receiver rejected the files");
  }
  if (answer.type !== "accept") throw unexpected(answer);
  const { files } = answer;
  try {
    checkAccept(entries, files);
  } catch (err) {
    throw protocolError(messageOf(err));
  }
  const [tops, topOf] = topEntries(entries);
  emit(transferring(tops, presentBytes(entries, files)));

  let sent = 0;
  let acked = 0;
  const ack = (message: ReceiverMessage) => {
    if (message.type !== "ack") throw unexpected(message);
    if (message.bytes < acked || message.bytes > sent) {
      throw protocolError("an ack outside the bytes sent");
    }
    acked = message.bytes;
  };

  channel.bufferedAmountLowThreshold = MAX_BUFFERED_BYTES / 2;
  const reader = new BlockReader(entries, sources, files);
  const progress = new Progress(entries, files, topOf);
  const codec = reader.done ? undefined : await createCodec();
  try {
    const encoding: { size: number; frame: Promise<Uint8Array> }[] = [];
    while (!reader.done || encoding.length > 0) {
      for (let m = peer.poll(); m; m = peer.poll()) ack(m);
      while (
        encoding.length < ENCODING_AHEAD &&
        !reader.done &&
        sent - acked < window
      ) {
        const bytes = await reader.read(window - (sent - acked));
        sent += bytes.length;
        const frame = codec!.encode(bytes);
        frame.catch(() => {});
        encoding.push({ size: bytes.length, frame });
      }
      const next = encoding.shift();
      if (!next) {
        ack(await peer.next(IDLE_TIMEOUT_MS, RECEIVER_IDLE));
        continue;
      }
      await sendBlock(next.size, await next.frame);
    }
  } finally {
    codec?.close();
  }

  /** Sends a block of `size` file bytes, which count in proportion to its bytes sent. */
  async function sendBlock(size: number, block: Uint8Array) {
    let counted = 0;
    for (let offset = 0; offset < block.length; offset += DATA_CHUNK_BYTES) {
      if (channel.bufferedAmount > MAX_BUFFERED_BYTES) {
        await Promise.race([
          new Promise((resolve) => (channel.onbufferedamountlow = resolve)),
          peer.stopped,
        ]);
      }
      const end = Math.min(offset + DATA_CHUNK_BYTES, block.length);
      channel.send(block.slice(offset, end));
      const now = Math.floor((size * end) / block.length);
      for (const [entry, n] of progress.advance(now - counted)) {
        emit({ type: "progress", entry, bytes: n });
      }
      counted = now;
    }
  }

  for (;;) {
    const message = await peer.next(IDLE_TIMEOUT_MS, RECEIVER_IDLE);
    if (message.type === "done") return;
    ack(message);
  }
}

/** Reads the accepted bytes of the offered files in order, a block at a time. */
class BlockReader {
  /** Bytes still to read. */
  private left: number;
  private file = 0;
  private position: number;

  constructor(
    private entries: Entry[],
    private sources: (Source | null)[],
    private files: AcceptedFile[],
  ) {
    this.left = files.reduce(
      (n, { id, offset }) => n + entrySize(entries[id]) - offset,
      0,
    );
    this.position = files[0]?.offset ?? 0;
  }

  get done(): boolean {
    return this.left === 0;
  }

  /** Up to `max` (and a block's worth of) bytes. */
  async read(max: number): Promise<Uint8Array> {
    const bytes = new Uint8Array(Math.min(max, this.left, MAX_BLOCK_BYTES));
    let filled = 0;
    while (filled < bytes.length) {
      const { id } = this.files[this.file];
      const size = entrySize(this.entries[id]);
      if (this.position === size) {
        this.position = this.files[++this.file].offset;
        continue;
      }
      const n = Math.min(size - this.position, bytes.length - filled);
      const slice = this.sources[id]!.slice(this.position, this.position + n);
      bytes.set(new Uint8Array(await slice.arrayBuffer()), filled);
      filled += n;
      this.position += n;
    }
    this.left -= filled;
    return bytes;
  }
}
