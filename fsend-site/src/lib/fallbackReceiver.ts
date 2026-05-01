import { RELAY_URL, PROTO_VERSION } from "../config";
import { RelayClient } from "./relay";
import { createAnswerer, waitConnected } from "./webrtc";
import { sendControlMessage, ControlDecoder } from "./protocol";
import { flattenTree, toSendRecvTree, treeSize } from "./fileTree";
import type {
  FilesAvailable,
  FileSendRecvTree,
  SenderToReceiver,
  ReceiverToSender,
} from "./types";
import type { ReceiverCallbacks } from "./receiver";

export async function runFallbackReceiver(
  code: string,
  callbacks: ReceiverCallbacks,
  abort: AbortSignal,
): Promise<void> {
  let relay: RelayClient | undefined;
  let pc: RTCPeerConnection | undefined;

  try {
    callbacks.onConnecting();

    relay = await RelayClient.connect(RELAY_URL);
    if (abort.aborted) return;

    await relay.joinSession(code.toUpperCase());

    const senderInfo = await relay.recvExchange();
    if (abort.aborted) return;

    const answerer = await createAnswerer(senderInfo.sdp);
    pc = answerer.pc;

    abort.addEventListener("abort", () => {
      relay?.close();
      pc?.close();
    });

    relay.sendExchange({
      type: "web_rtc",
      sdp: answerer.answerSdp,
      ice_candidates: [],
    });

    const { controlChannel, dataChannel } = await answerer.channelsReady;
    await waitConnected(pc, [controlChannel, dataChannel]);
    if (abort.aborted) return;

    let transferDone = false;
    const disconnectPromise = new Promise<never>((_, reject) => {
      dataChannel.addEventListener("close", () => {
        if (!transferDone) reject(new Error("Peer disconnected"));
      });
    });
    disconnectPromise.catch(() => {});

    callbacks.onHandshaking();

    const decoder = new ControlDecoder();
    const controlMessages: SenderToReceiver[] = [];
    let controlResolve: (() => void) | null = null;

    controlChannel.onmessage = async (ev) => {
      const data =
        ev.data instanceof ArrayBuffer ? ev.data : await ev.data.arrayBuffer();
      const msg = await decoder.onMessage(data);
      if (msg) {
        controlMessages.push(msg as SenderToReceiver);
        controlResolve?.();
      }
    };

    const waitControl = (): Promise<SenderToReceiver> => {
      const inner =
        controlMessages.length > 0
          ? Promise.resolve(controlMessages.shift()!)
          : new Promise<SenderToReceiver>((resolve) => {
              controlResolve = () => {
                controlResolve = null;
                resolve(controlMessages.shift()!);
              };
            });
      return Promise.race([inner, disconnectPromise]);
    };

    const connReq = await waitControl();
    if (connReq.type !== "ConnRequest") throw new Error("Expected ConnRequest");
    if (connReq.version !== PROTO_VERSION) {
      await sendControlMessage(controlChannel, {
        type: "WrongVersion",
        expected: PROTO_VERSION,
      } as ReceiverToSender);
      throw new Error(`Version mismatch: sender has ${connReq.version}`);
    }
    await sendControlMessage(controlChannel, {
      type: "Ok",
    } as ReceiverToSender);

    const fileInfoMsg = await waitControl();
    if (fileInfoMsg.type !== "FileInfo") throw new Error("Expected FileInfo");
    const offeredFiles = fileInfoMsg.files;

    const accepted = await Promise.race([
      new Promise<boolean>((resolve) => {
        callbacks.onFilesOffered(
          offeredFiles,
          () => resolve(true),
          () => resolve(false),
        );
      }),
      disconnectPromise,
    ]);

    if (!accepted) {
      await sendControlMessage(controlChannel, {
        type: "RejectFiles",
      } as ReceiverToSender);
      throw new Error("Transfer rejected");
    }

    const skipInfo = offeredFiles.map(() => null);
    await sendControlMessage(controlChannel, {
      type: "AcceptFilesSkip",
      files: skipInfo,
    } as ReceiverToSender);

    const recvTrees: FileSendRecvTree[] = offeredFiles.map(toSendRecvTree);
    const flatFiles = flattenTree(recvTrees);
    callbacks.onTransferring(
      recvTrees.map((t) => ({
        name: t.name,
        size: treeSize(t),
        skip: 0,
        isDir: t.type === "Dir",
      })),
    );

    if (flatFiles.length === 0) {
      callbacks.onComplete();
      return;
    }

    const buffers = new Map<string, Uint8Array[]>();
    for (const f of flatFiles) buffers.set(f.path, []);

    let fileIdx = 0;
    let fileWritten = 0;

    const dataPromise = new Promise<void>((resolve, reject) => {
      dataChannel.onmessage = (ev) => {
        try {
          if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0)
            return;
          const chunk = new Uint8Array(ev.data);

          if (fileIdx >= flatFiles.length) return;

          buffers.get(flatFiles[fileIdx].path)!.push(chunk);
          fileWritten += chunk.length;
          callbacks.onProgress(chunk.length);

          while (
            fileIdx < flatFiles.length &&
            fileWritten >= flatFiles[fileIdx].size
          ) {
            fileIdx++;
            fileWritten = 0;
          }

          if (fileIdx >= flatFiles.length) {
            resolve();
          }
        } catch (err) {
          reject(err);
        }
      };
    });

    await Promise.race([dataPromise, disconnectPromise]);
    transferDone = true;
    pc?.close();

    const hasDirectories = offeredFiles.some((f) => f.type === "Dir");
    const multipleFiles = offeredFiles.length > 1;

    if (!hasDirectories && !multipleFiles && flatFiles.length === 1) {
      // Single file download
      const chunks = buffers.get(flatFiles[0].path)!;
      const blob = new Blob(chunks as any);
      triggerDownload(blob, flatFiles[0].path.split("/").pop()!);
    } else {
      // Zip download
      const JSZip = (await import("jszip")).default;
      const zip = new JSZip();
      for (const [path, chunks] of buffers) {
        zip.file(path, new Blob(chunks as any));
      }
      const blob = await zip.generateAsync({ type: "blob" });
      const zipName =
        offeredFiles.length === 1
          ? `${offeredFiles[0].name}.zip`
          : "fsend-files.zip";
      triggerDownload(blob, zipName);
    }

    callbacks.onComplete();
  } catch (err: any) {
    if (!abort.aborted) {
      callbacks.onError(err.message ?? String(err));
    }
  } finally {
    relay?.close();
    pc?.close();
  }
}

function triggerDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}
