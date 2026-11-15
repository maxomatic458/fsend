import { openReceiverSession, type Link } from "../transport/session";
import { getConnectionType } from "../transport/webrtc";
import { entrySize, fingerprintOf, offerSizeBytes } from "../files/tree";
import {
  ACK_INTERVAL_MS,
  ACK_STEP,
  HELLO_TIMEOUT_MS,
  IDLE_TIMEOUT_MS,
  RECV_WINDOW,
  Progress,
  acceptPlan,
  filesToCheck,
  planReceive,
  presentBytes,
  topEntries,
  validateOffer,
  type AcceptedFile,
  type CheckedFile,
  type ConflictPolicy,
  type Entry,
  type Local,
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
import { createCodec, type BlockCodec } from "./codec";

const SENDER_IDLE = "the sender stopped sending";
/// How many files can be written at once.
const CONCURRENT_WRITES = 8;

/// Receives a transfer into `sink`; files there that differ from the offer are handled by `policy`.
export async function runReceive(
  code: string,
  sink: TransferSink,
  policy: ConflictPolicy,
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
    const { entries, tops } = await peer.explained(() =>
      receive(peer, sink, policy, emit),
    );
    link.peer.stop();
    link.pc.close();

    await sink.finish(entries, tops, (percent) =>
      emit({ type: "packing", percent }),
    );
    emit({ type: "complete" });
  } catch (err) {
    await sink.abandon();
    if (!abort.aborted) emit({ type: "error", message: messageOf(err) });
  } finally {
    await sink.closeFile().catch(() => {});
    link?.close();
  }
}

/** Returns the offer received, and its top-level entries. */
async function receive(
  peer: ReceiverPeer,
  sink: TransferSink,
  policy: ConflictPolicy,
  emit: TransferListener,
): Promise<{ entries: Entry[]; tops: TopEntry[] }> {
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

  await sink.prepare(tops);
  const local: Local[] = [];
  for (const entry of entries) local.push(await sink.probe(entry));
  const checked = filesToCheck(entries, local);
  const verified =
    checked.length === 0 ? [] : await verify(peer, sink, entries, checked);
  const plan = planReceive(entries, local, verified);
  const files = acceptPlan(plan, policy);
  const remaining = files.reduce(
    (n, { id, offset }) => n + entrySize(entries[id]) - offset,
    0,
  );
  await sink.reserve(remaining, offerSizeBytes(entries));
  peer.send({ type: "accept", files });
  emit(transferring(tops, presentBytes(entries, files)));
  if (plan.conflicts.length > 0) {
    const replaced =
      policy === "replace"
        ? plan.conflicts.filter((conflict) => conflict.replaceable).length
        : 0;
    emit({
      type: "conflicts",
      replaced,
      skipped: plan.conflicts.length - replaced,
    });
  }

  for (const id of plan.dirs) await sink.createDir(entries[id].path);
  const codec = files.some(({ id, offset }) => entrySize(entries[id]) > offset)
    ? await createCodec()
    : undefined;
  try {
    await receiveData(peer, sink, entries, files, topOf, codec, emit);
  } finally {
    codec?.close();
  }

  peer.send({ type: "done" });
  await peer.goodbye();
  return { entries, tops };
}

/** Writes the accepted files from the data stream into `sink`, acking as it goes. */
async function receiveData(
  peer: ReceiverPeer,
  sink: TransferSink,
  entries: Entry[],
  files: AcceptedFile[],
  topOf: number[],
  codec: BlockCodec | undefined,
  emit: TransferListener,
): Promise<void> {
  let written = 0;
  let acked = 0;
  let ackedAt = performance.now();
  // File bytes count as their blocks arrive, in proportion.
  const progress = new Progress(entries, files, topOf);
  let decoded = 0;
  let arrived = 0;
  const blocks: Uint8Array[] = [];
  let empty = true;
  const nextBlock = async () => {
    while (blocks.length === 0) {
      const first = await peer.receive(IDLE_TIMEOUT_MS, SENDER_IDLE);
      // The sender says nothing while it sends.
      if (!(first instanceof Uint8Array)) throw unexpected(first);
      const chunks = [first];
      for (let more = peer.pollData(); more; more = peer.pollData()) {
        chunks.push(more);
      }
      let batch;
      try {
        batch = await codec!.decode(chunks);
      } catch (err) {
        throw protocolError(messageOf(err));
      }
      blocks.push(...batch.blocks);
      empty = batch.empty;
      decoded += batch.blocks.reduce((n, block) => n + block.length, 0);
      const now = decoded + batch.partial;
      if (now > arrived) {
        for (const [entry, n] of progress.advance(now - arrived)) {
          emit({ type: "progress", entry, bytes: n });
        }
        arrived = now;
      }
    }
    return blocks.shift()!;
  };
  const wrote = (n: number) => {
    written += n;
    if (
      written - acked >= ACK_STEP ||
      performance.now() - ackedAt >= ACK_INTERVAL_MS
    ) {
      peer.send({ type: "ack", bytes: written });
      acked = written;
      ackedAt = performance.now();
    }
  };
  // Whole files being written.
  const writing: { bytes: number; done: Promise<void> }[] = [];
  const settle = async () => {
    const { bytes, done } = writing.shift()!;
    await done;
    wrote(bytes);
  };
  // The rest of a block that ends one file and starts the next.
  let pending: Uint8Array | undefined;
  for (const { id, offset } of files) {
    const entry = entries[id];
    const size = entrySize(entry);
    const rest = size - offset;
    if (rest > 0 && !pending) pending = await nextBlock();
    // A file that has arrived in full
    if (rest <= (pending?.length ?? 0)) {
      const data = pending?.subarray(0, rest) ?? new Uint8Array(0);
      if (pending) {
        pending = rest < pending.length ? pending.subarray(rest) : undefined;
      }
      const done = sink.writeFile(entry.path, offset, data);
      done.catch(() => {});
      writing.push({ bytes: rest, done });
      if (writing.length >= CONCURRENT_WRITES) await settle();
      continue;
    }
    // Written as it arrives, once the files before it are done.
    while (writing.length > 0) await settle();
    await sink.open(entry.path, offset);
    for (let position = offset; position < size;) {
      const block = pending ?? (await nextBlock());
      const n = Math.min(block.length, size - position);
      await sink.write(block.subarray(0, n));
      pending = n < block.length ? block.subarray(n) : undefined;
      position += n;
      wrote(n);
    }
    await sink.closeFile();
  }
  while (writing.length > 0) await settle();
  if (pending || blocks.length > 0 || !empty) {
    throw protocolError("the sender sent more bytes than accepted");
  }
}

/** Asks the sender for fingerprints of the `checked` files; returns the ids of those that match ours. */
async function verify(
  peer: ReceiverPeer,
  sink: TransferSink,
  entries: Entry[],
  checked: CheckedFile[],
): Promise<number[]> {
  peer.send({ type: "check", files: checked });
  const answer = await peer.next();
  if (answer.type !== "hashes") throw unexpected(answer);
  if (answer.hashes.length !== checked.length) {
    throw protocolError("the hashes do not match the check");
  }
  const verified = [];
  for (const [i, { id, len }] of checked.entries()) {
    const ours = await fingerprintOf(await sink.read(entries[id].path), len);
    if (ours === answer.hashes[i]) verified.push(id);
  }
  return verified;
}
