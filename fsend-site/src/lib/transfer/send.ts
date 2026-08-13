import {
  PROTO_VERSION,
  DATA_CHUNK_BYTES,
  MAX_BUFFERED_BYTES,
} from "../../config";
import { openSenderSession } from "../transport/session";
import { getConnectionType } from "../transport/webrtc";
import {
  buildFileTree,
  flattenTree,
  applySkip,
  treeSizeBytes,
  treeSkipBytes,
} from "../files/tree";
import { collectFiles } from "../files/source";
import type { TransferListener } from "./events";
import type {
  SelectedEntry,
  FileSendRecvTree,
  SenderToReceiver,
} from "../types";

export async function runSend(
  entries: SelectedEntry[],
  emit: TransferListener,
  abort: AbortSignal,
): Promise<void> {
  let session: Awaited<ReturnType<typeof openSenderSession>> = null;

  try {
    const fileTree = await buildFileTree(entries);
    const files = await collectFiles(entries);

    session = await openSenderSession(abort, {
      onCode: (code) => emit({ type: "code", code }),
      onWaitingPeer: () => emit({ type: "waitingPeer" }),
      onHandshaking: () => emit({ type: "handshaking" }),
    });
    if (!session) return;
    const { control, dataChannel, peer, disconnected } = session;

    // TODO
    getConnectionType(session.pc)
      .then((kind) => emit({ type: "connectionType", kind }))
      .catch(() => {});

    // handshake
    await control.send({
      type: "ConnRequest",
      version: PROTO_VERSION,
    } as SenderToReceiver);

    const version = await control.next();
    if (version.type === "WrongVersion") {
      throw new Error(`Version mismatch: peer expects ${version.expected}`);
    }
    if (version.type !== "Ok") {
      throw new Error("Unexpected response to version handshake");
    }

    // offer files
    await control.send({
      type: "FileInfo",
      files: fileTree,
    } as SenderToReceiver);
    emit({ type: "waitingAccept" });

    const answer = await control.next();
    if (answer.type === "RejectFiles") {
      throw new Error("Receiver rejected the files");
    }
    if (answer.type !== "AcceptFilesSkip") {
      throw new Error("Unexpected response");
    }

    // data
    const trees: FileSendRecvTree[] = [];
    for (let i = 0; i < fileTree.length; i++) {
      const tree = applySkip(fileTree[i], answer.files[i] ?? null);
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

    dataChannel.bufferedAmountLowThreshold = MAX_BUFFERED_BYTES / 2;

    for (const { path, skipBytes, sizeBytes } of flat) {
      const file = files.get(path);
      if (!file) throw new Error(`File not found: ${path}`);

      let sentBytes = skipBytes;
      while (sentBytes < sizeBytes) {
        if (abort.aborted) return;
        if (peer.isDown()) throw new Error("Peer disconnected");

        if (dataChannel.bufferedAmount > MAX_BUFFERED_BYTES) {
          await Promise.race([
            new Promise<void>((resolve) => {
              dataChannel.onbufferedamountlow = () => resolve();
            }),
            disconnected,
          ]);
        }

        const endBytes = Math.min(sentBytes + DATA_CHUNK_BYTES, sizeBytes);
        dataChannel.send(await file.slice(sentBytes, endBytes).arrayBuffer());
        emit({ type: "progress", bytes: endBytes - sentBytes });
        sentBytes = endBytes;
      }
    }

    // The receiver closes once it has consumed everything.
    await disconnected.catch(() => {});
    emit({ type: "complete" });
  } catch (err: any) {
    if (!abort.aborted) {
      emit({ type: "error", message: err?.message ?? String(err) });
    }
  } finally {
    session?.close();
  }
}
