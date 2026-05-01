import { RELAY_URL, PROTO_VERSION } from "../config";
import { RelayClient } from "./relay";
import { createAnswerer, waitConnected } from "./webrtc";
import { sendControlMessage, ControlDecoder } from "./protocol";
import { flattenTree, applySkip, treeSize, treeSkip } from "./fileTree";
import { getExistingFileSizes } from "./fsAccess";
import type {
  FilesAvailable,
  FileSendRecvTree,
  SenderToReceiver,
  ReceiverToSender,
} from "./types";

export interface ReceiverCallbacks {
  onConnecting: () => void;
  onHandshaking: () => void;
  onFilesOffered: (
    files: FilesAvailable[],
    accept: () => void,
    reject: () => void,
  ) => void;
  onTransferring: (
    entries: Array<{
      name: string;
      size: number;
      skip: number;
      isDir: boolean;
    }>,
  ) => void;
  onProgress: (bytes: number) => void;
  onComplete: () => void;
  onError: (error: string) => void;
  onConnectionType: (type: "direct" | "relay" | "unknown") => void;
}

export async function runReceiver(
  code: string,
  dirHandle: FileSystemDirectoryHandle,
  resume: boolean,
  callbacks: ReceiverCallbacks,
  abort: AbortSignal,
): Promise<void> {
  let relay: RelayClient | undefined;
  let pc: RTCPeerConnection | undefined;
  let currentWritable: FileSystemWritableFileStream | null = null;

  try {
    callbacks.onConnecting();

    relay = await RelayClient.connect(RELAY_URL);
    if (abort.aborted) return;

    await relay.joinSession(code.toUpperCase());

    // Get sender's offer
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

    // Version handshake
    const connReq = await waitControl();
    if (connReq.type !== "ConnRequest") throw new Error("Expected ConnRequest");
    if (connReq.version !== PROTO_VERSION) {
      await sendControlMessage(controlChannel, {
        type: "WrongVersion",
        expected: PROTO_VERSION,
      } as ReceiverToSender);
      throw new Error(
        `Version mismatch: sender has ${connReq.version}, we need ${PROTO_VERSION}`,
      );
    }
    await sendControlMessage(controlChannel, {
      type: "Ok",
    } as ReceiverToSender);

    // Receive file info
    const fileInfoMsg = await waitControl();
    if (fileInfoMsg.type !== "FileInfo") throw new Error("Expected FileInfo");
    const offeredFiles = fileInfoMsg.files;

    // Let UI show the files and wait for accept/reject (races disconnect/abort)
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

    // Compute skip info
    let skipInfo: (import("./types").FilesToSkip | null)[];
    if (resume) {
      skipInfo = await getExistingFileSizes(dirHandle, offeredFiles);
    } else {
      skipInfo = offeredFiles.map(() => null);
    }

    await sendControlMessage(controlChannel, {
      type: "AcceptFilesSkip",
      files: skipInfo,
    } as ReceiverToSender);

    // Build receive tree
    const recvTrees: FileSendRecvTree[] = [];
    for (let i = 0; i < offeredFiles.length; i++) {
      const tree = applySkip(offeredFiles[i], skipInfo[i]);
      if (tree) recvTrees.push(tree);
    }

    const flatFiles = flattenTree(recvTrees);
    callbacks.onTransferring(
      recvTrees.map((t) => ({
        name: t.name,
        size: treeSize(t),
        skip: treeSkip(t),
        isDir: t.type === "Dir",
      })),
    );

    if (flatFiles.length === 0) {
      callbacks.onComplete();
      return;
    }

    let fileIdx = 0;
    let fileWritten = flatFiles[0].skip;
    let totalBytesReceived = flatFiles.reduce((s, f) => s + f.skip, 0);

    const openFile = async (path: string, skip: number) => {
      const parts = path.split("/");
      let dir = dirHandle;
      for (let i = 0; i < parts.length - 1; i++) {
        dir = await dir.getDirectoryHandle(parts[i], { create: true });
      }
      const fh = await dir.getFileHandle(parts[parts.length - 1], {
        create: true,
      });
      const writable = await fh.createWritable({ keepExistingData: skip > 0 });
      if (skip > 0) await writable.seek(skip);
      return writable;
    };

    // Register onmessage BEFORE openFile so chunks that arrive during the
    // async seek are buffered rather than lost.
    const chunkQueue: Uint8Array[] = [];
    dataChannel.onmessage = (ev) => {
      if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0) return;
      chunkQueue.push(new Uint8Array(ev.data));
    };

    currentWritable = await openFile(flatFiles[0].path, flatFiles[0].skip);

    const dataPromise = new Promise<void>((resolve, reject) => {
      let processing = false;

      const processQueue = async () => {
        if (processing) return;
        processing = true;
        try {
          while (chunkQueue.length > 0) {
            const chunk = chunkQueue.shift()!;
            if (fileIdx >= flatFiles.length) break;

            await currentWritable!.write(chunk as any);
            fileWritten += chunk.length;
            totalBytesReceived += chunk.length;
            callbacks.onProgress(chunk.length);

            while (
              fileIdx < flatFiles.length &&
              fileWritten >= flatFiles[fileIdx].size
            ) {
              await currentWritable!.close();
              currentWritable = null;
              fileIdx++;
              if (fileIdx < flatFiles.length) {
                fileWritten = flatFiles[fileIdx].skip;
                currentWritable = await openFile(
                  flatFiles[fileIdx].path,
                  flatFiles[fileIdx].skip,
                );
              }
            }

            if (fileIdx >= flatFiles.length) {
              resolve();
              return;
            }
          }
        } catch (err) {
          reject(err);
        } finally {
          processing = false;
        }
      };

      // Re-wire onmessage now that writable is ready; drain any buffered chunks.
      dataChannel.onmessage = (ev) => {
        if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0)
          return;
        chunkQueue.push(new Uint8Array(ev.data));
        processQueue();
      };

      processQueue();
    });

    await Promise.race([dataPromise, disconnectPromise]);
    transferDone = true;
    pc?.close();
    callbacks.onComplete();
  } catch (err: any) {
    if (!abort.aborted) {
      callbacks.onError(err.message ?? String(err));
    }
  } finally {
    // Close writable to flush partial data to disk, enabling resume.
    await currentWritable?.close().catch(() => {});
    relay?.close();
    pc?.close();
  }
}
