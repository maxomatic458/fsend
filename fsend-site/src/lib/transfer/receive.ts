import { openReceiverSession, type Link } from "../transport/session";
import { getConnectionType } from "../transport/webrtc";
import { entrySize } from "../files/tree";
import {
  ACK_INTERVAL_MS,
  ACK_STEP,
  HELLO_TIMEOUT_MS,
  IDLE_TIMEOUT_MS,
  RECV_WINDOW,
  acceptFiles,
  presentBytes,
  topEntries,
  validateOffer,
  type TopEntry,
} from "../proto";
import {
  Peer,
  TransferFailure,
  type ReceiverPeer,
  messageOf,
  protocolError,
  transferring,
  unexpected,
} from "./peer";
import type { TransferSink } from "./sinks";
import type { TransferListener } from "./events";

const SENDER_IDLE = "the sender stopped sending";

/// Receives a transfer into `sink`.
export async function runReceive(
  code: string,
  sink: TransferSink,
  resume: boolean,
  emit: TransferListener,
  abort: AbortSignal,
): Promise<void> {
  let link: Link | null = null;

  try {
    emit({ type: "connecting" });

    link = await openReceiverSession(code, abort, {
      onHandshaking: () => emit({ type: "handshaking" }),
    });
    if (!link) return;

    getConnectionType(link.pc)
      .then((kind) => emit({ type: "connectionType", kind }))
      .catch(() => {});

    const peer = Peer.receiver(link, abort);
    const tops = await peer.explained(() =>
      receive(peer, sink, resume && sink.canResume, emit),
    );
    link.peer.stop();
    link.pc.close();

    await sink.finish(tops, (percent) => emit({ type: "packing", percent }));
    emit({ type: "complete" });
  } catch (err) {
    await sink.abandon();
    if (!abort.aborted) emit({ type: "error", message: messageOf(err) });
  } finally {
    await sink.closeFile().catch(() => {});
    link?.close();
  }
}

/** Returns the top-level entries received. */
async function receive(
  peer: ReceiverPeer,
  sink: TransferSink,
  resume: boolean,
  emit: TransferListener,
): Promise<TopEntry[]> {
  await peer.hello("the sender did not say hello in time", RECV_WINDOW);
  const offer = await peer.next(
    HELLO_TIMEOUT_MS,
    "the sender did not make an offer in time",
  );
  if (offer.type !== "offer") throw unexpected(offer);
  const { entries } = offer;
  try {
    validateOffer(entries);
  } catch (err) {
    throw new TransferFailure(`Invalid offer: ${messageOf(err)}`, {
      code: "invalid_offer",
      message: messageOf(err),
    });
  }
  const [tops, topOf] = topEntries(entries);

  const accepted = await Promise.race([
    new Promise<boolean>((resolve) =>
      emit({
        type: "offered",
        files: tops,
        accept: () => resolve(true),
        reject: () => resolve(false),
      }),
    ),
    peer.stopped,
  ]);
  if (!accepted) {
    peer.send({ type: "reject" });
    await peer.goodbye();
    throw new TransferFailure("Transfer rejected");
  }

  const local: (number | null)[] = [];
  for (const entry of entries) {
    local.push(
      resume && entry.kind === "file" ? await sink.probe(entry.path) : null,
    );
  }
  const files = acceptFiles(entries, local);
  peer.send({ type: "accept", files });
  emit(transferring(tops, presentBytes(entries, files)));

  for (const entry of entries) {
    if (entry.kind === "dir") await sink.createDir(entry.path);
  }
  let written = 0;
  let acked = 0;
  let ackedAt = performance.now();
  // The rest of a chunk that ends one file and starts the next.
  let pending: Uint8Array | undefined;
  for (const { id, offset } of files) {
    const entry = entries[id];
    const size = entrySize(entry);
    await sink.open(entry.path, offset);
    for (let position = offset; position < size;) {
      const chunk =
        pending ?? (await peer.receive(IDLE_TIMEOUT_MS, SENDER_IDLE));
      // The sender says nothing while it sends.
      if (!(chunk instanceof Uint8Array)) throw unexpected(chunk);
      const n = Math.min(chunk.length, size - position);
      await sink.write(chunk.subarray(0, n));
      pending = n < chunk.length ? chunk.subarray(n) : undefined;
      position += n;
      written += n;
      emit({ type: "progress", entry: topOf[id], bytes: n });
      if (
        written - acked >= ACK_STEP ||
        performance.now() - ackedAt >= ACK_INTERVAL_MS
      ) {
        peer.send({ type: "ack", bytes: written });
        acked = written;
        ackedAt = performance.now();
      }
    }
    await sink.closeFile();
  }
  if (pending) throw protocolError("the sender sent more bytes than accepted");

  peer.send({ type: "done" });
  await peer.goodbye();
  return tops;
}
