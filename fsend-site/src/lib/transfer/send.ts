import { DATA_CHUNK_BYTES, MAX_BUFFERED_BYTES } from "../../config";
import { openSenderSession, type Link } from "../transport/session";
import { getConnectionType } from "../transport/webrtc";
import { buildOffer, entrySize, type Outgoing } from "../files/tree";
import {
  IDLE_TIMEOUT_MS,
  MIN_RECV_WINDOW,
  checkAccept,
  encodeOffer,
  loadProto,
  presentBytes,
  topEntries,
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
import type { SelectedEntry } from "../types";

const RECEIVER_IDLE = "the receiver stopped responding";

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
  const answer = await peer.next();
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
  for (const { id, offset } of files) {
    const source = sources[id]!;
    const size = entrySize(entries[id]);
    for (let position = offset; position < size;) {
      // Also notices a peer that is gone.
      for (let m = peer.poll(); m; m = peer.poll()) ack(m);
      while (sent - acked >= window) {
        ack(await peer.next(IDLE_TIMEOUT_MS, RECEIVER_IDLE));
      }
      if (channel.bufferedAmount > MAX_BUFFERED_BYTES) {
        await Promise.race([
          new Promise((resolve) => (channel.onbufferedamountlow = resolve)),
          peer.stopped,
        ]);
      }

      const n = Math.min(
        DATA_CHUNK_BYTES,
        size - position,
        window - (sent - acked),
      );
      channel.send(await source.slice(position, position + n).arrayBuffer());
      position += n;
      sent += n;
      emit({ type: "progress", entry: topOf[id], bytes: n });
    }
  }

  for (;;) {
    const message = await peer.next(IDLE_TIMEOUT_MS, RECEIVER_IDLE);
    if (message.type === "done") return;
    ack(message);
  }
}
