import { PROTO_VERSION } from "../../config";
import { openReceiverSession } from "../transport/session";
import { getConnectionType } from "../transport/webrtc";
import { flattenTree, applySkip, treeSize, treeSkip } from "../files/tree";
import type { TransferSink } from "./sinks";
import type { TransferListener } from "./events";
import type { FilesToSkip, FileSendRecvTree, ReceiverToSender } from "../types";

/// Receives a transfer into `sink`.
export async function runReceive(
  code: string,
  sink: TransferSink,
  resume: boolean,
  emit: TransferListener,
  abort: AbortSignal,
): Promise<void> {
  let session: Awaited<ReturnType<typeof openReceiverSession>> = null;

  try {
    emit({ type: "connecting" });

    session = await openReceiverSession(code, abort, {
      onHandshaking: () => emit({ type: "handshaking" }),
    });
    if (!session) return;
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

    // Resume negotiation
    const skipInfo: (FilesToSkip | null)[] =
      resume && sink.canResume
        ? await sink.existing(offered)
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
        size: treeSize(t),
        skip: treeSkip(t),
        isDir: t.type === "Dir",
      })),
    });

    if (flat.length === 0) {
      await sink.finish(offered);
      emit({ type: "complete" });
      return;
    }

    // data
    let fileIdx = 0;
    let written = flat[0].skip;

    // Buffer chunks from the very first one
    const queue: Uint8Array[] = [];
    dataChannel.onmessage = (ev) => {
      if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0) return;
      queue.push(new Uint8Array(ev.data));
    };

    await sink.open(flat[0].path, flat[0].skip);

    const received = new Promise<void>((resolve, reject) => {
      let draining = false;

      /// Close finished files and open the next, returns true if the transfer is complete.
      const advance = async () => {
        while (fileIdx < flat.length && written >= flat[fileIdx].size) {
          await sink.closeFile();
          fileIdx++;
          if (fileIdx < flat.length) {
            written = flat[fileIdx].skip;
            await sink.open(flat[fileIdx].path, flat[fileIdx].skip);
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
            written += chunk.length;
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
        if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0)
          return;
        queue.push(new Uint8Array(ev.data));
        drain();
      };

      drain();
    });

    await Promise.race([received, disconnected]);
    session.peer.stop();
    session.pc.close();

    await sink.finish(offered);
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
