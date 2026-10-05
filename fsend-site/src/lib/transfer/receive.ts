import { PROTO_VERSION } from "../../config";
import {
  openReceiverSession,
  type ReceiverSession,
} from "../transport/session";
import { getConnectionType } from "../transport/webrtc";
import {
  flattenTree,
  applySkip,
  isSafeOffer,
  retainFiles,
  treeSizeBytes,
  treeSkipBytes,
  type FlatFile,
} from "../files/tree";
import type { ResumeSource, TransferSink } from "./sinks";
import type { TransferListener } from "./events";
import type {
  FilesAvailable,
  FilesToSkip,
  FileSendRecvTree,
  ReceiverToSender,
} from "../types";

/// Receives a transfer into `sink`.
export async function runReceive(
  code: string,
  sink: TransferSink,
  resume: boolean,
  emit: TransferListener,
  abort: AbortSignal,
): Promise<void> {
  let session: ReceiverSession | null = null;

  try {
    emit({ type: "connecting" });

    session = await openReceiverSession(code, abort, {
      onHandshaking: () => emit({ type: "handshaking" }),
    });
    const { control, dataChannel, disconnected } = session;

    getConnectionType(session.pc)
      .then((kind) => emit({ type: "connectionType", kind }))
      .catch(() => {});

    // handshake
    const request = await control.next();
    if (request.type !== "ConnRequest") throw new Error("Expected ConnRequest");
    if (request.version !== PROTO_VERSION) {
      await control.send({ type: "WrongVersion", expected: PROTO_VERSION });
      await control.flush();
      throw new Error(
        `Version mismatch: sender has ${request.version}, we need ${PROTO_VERSION}`,
      );
    }
    await control.send({ type: "Ok" } as ReceiverToSender);

    // Offer file info
    const info = await control.next();
    if (info.type !== "FileInfo") throw new Error("Expected FileInfo");
    const offered = info.files;
    if (!isSafeOffer(offered)) {
      await control.send({ type: "RejectFiles" });
      await control.flush();
      throw new Error("The sender offered a file with an invalid name");
    }

    const accepted = await Promise.race([
      new Promise<boolean>((resolve) => {
        emit({
          type: "offered",
          files: offered,
          accept: () => resolve(true),
          reject: () => resolve(false),
        });
      }),
      disconnected,
    ]);

    if (!accepted) {
      await control.send({ type: "RejectFiles" } as ReceiverToSender);
      await control.flush();
      throw new Error("Transfer rejected");
    }

    const skipInfo =
      resume && sink.resume
        ? await resumable(control, sink.resume, offered)
        : offered.map(() => null);

    await control.send({
      type: "AcceptFilesSkip",
      files: skipInfo,
    } as ReceiverToSender);

    const trees: FileSendRecvTree[] = [];
    for (let i = 0; i < offered.length; i++) {
      const tree = applySkip(offered[i], skipInfo[i]);
      if (tree) trees.push(tree);
    }

    const flat = flattenTree(trees);
    emit({
      type: "transferring",
      entries: trees.map((t) => ({
        name: t.name,
        sizeBytes: treeSizeBytes(t),
        skipBytes: treeSkipBytes(t),
        isDir: t.type === "Dir",
      })),
    });

    if (flat.length > 0) {
      await Promise.race([
        receiveFiles(dataChannel, sink, flat, emit),
        disconnected,
      ]);
    }
    await control.send({ type: "Done" });
    await session.goodbye();
    session.close();

    await sink.finish(offered, (percent) => emit({ type: "packing", percent }));
    emit({ type: "complete" });
  } catch (err: any) {
    await sink.abandon();
    if (!abort.aborted) {
      emit({ type: "error", message: err?.message ?? String(err) });
    }
  } finally {
    await sink.closeFile().catch(() => {});
    session?.close();
  }
}

/// The candidates whose local prefix hashes the same as the sender's.
async function resumable(
  control: ReceiverSession["control"],
  source: ResumeSource,
  offered: FilesAvailable[],
): Promise<(FilesToSkip | null)[]> {
  const candidates = await source.candidates(offered);
  if (!candidates.some(Boolean)) return candidates;

  await control.send({ type: "ResumeCandidates", files: candidates });
  const reply = await control.next();
  if (reply.type !== "PrefixHashes") throw new Error("Expected PrefixHashes");
  const ours = await source.prefixHashes(candidates);
  if (ours.length !== reply.hashes.length) {
    throw new Error("Unexpected PrefixHashes");
  }
  const keep = ours.map((hash, i) => hash !== null && hash === reply.hashes[i]);
  return retainFiles(candidates, keep);
}

/// Writes the data channel's bytes into `sink`, file after file.
async function receiveFiles(
  dataChannel: RTCDataChannel,
  sink: TransferSink,
  flat: FlatFile[],
  emit: TransferListener,
): Promise<void> {
  let fileIdx = 0;
  let writtenBytes = flat[0].skipBytes;

  // Buffer chunks from the very first one
  const queue: Uint8Array[] = [];
  dataChannel.onmessage = (ev) => {
    if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0) return;
    queue.push(new Uint8Array(ev.data));
  };

  await sink.open(flat[0].path, flat[0].skipBytes);

  return new Promise<void>((resolve, reject) => {
    let draining = false;

    /// Close finished files and open the next, returns true if the transfer is complete.
    const advance = async () => {
      while (fileIdx < flat.length && writtenBytes >= flat[fileIdx].sizeBytes) {
        await sink.closeFile();
        fileIdx++;
        if (fileIdx < flat.length) {
          writtenBytes = flat[fileIdx].skipBytes;
          await sink.open(flat[fileIdx].path, flat[fileIdx].skipBytes);
        }
      }
      if (fileIdx >= flat.length) {
        resolve();
        return true;
      }
      return false;
    };

    const drain = async () => {
      if (draining) return;
      draining = true;
      try {
        // File that is empty or has been fully skipped (done)
        if (await advance()) return;

        while (queue.length > 0) {
          const chunk = queue.shift()!;
          if (fileIdx >= flat.length) break;
          await sink.write(chunk);
          writtenBytes += chunk.length;
          emit({ type: "progress", bytes: chunk.length });
          if (await advance()) return;
        }
      } catch (err) {
        reject(err);
      } finally {
        draining = false;
      }
    };

    dataChannel.onmessage = (ev) => {
      if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0) return;
      queue.push(new Uint8Array(ev.data));
      drain();
    };

    drain();
  });
}
