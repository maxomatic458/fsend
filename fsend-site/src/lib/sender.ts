import {
  RELAY_URL,
  PROTO_VERSION,
  DATA_CHUNK_SIZE,
  MAX_BUFFERED,
} from "../config";
import { RelayClient } from "./relay";
import { createOfferer, applyAnswer, waitConnected } from "./webrtc";
import { sendControlMessage, ControlDecoder } from "./protocol";
import {
  buildFileTree,
  flattenTree,
  applySkip,
  treeSize,
  treeSkip,
} from "./fileTree";
import type {
  SelectedEntry,
  FilesAvailable,
  FileSendRecvTree,
  ReceiverToSender,
  SenderToReceiver,
  ConnectionInfo,
} from "./types";

export interface SenderCallbacks {
  onCode: (code: string) => void;
  onWaitingPeer: () => void;
  onHandshaking: () => void;
  onWaitingAccept: () => void;
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

export async function runSender(
  entries: SelectedEntry[],
  callbacks: SenderCallbacks,
  abort: AbortSignal,
): Promise<void> {
  let relay: RelayClient | undefined;
  let pc: RTCPeerConnection | undefined;

  try {
    // Build file tree
    const fileTree = await buildFileTree(entries);

    // Build a map from path -> File for reading
    const fileMap = await buildFileMap(entries);

    // Connect to relay
    relay = await RelayClient.connect(RELAY_URL);
    if (abort.aborted) return;

    const code = await relay.createSession();
    callbacks.onCode(code);
    callbacks.onWaitingPeer();

    await relay.waitForPeer();
    if (abort.aborted) return;

    callbacks.onHandshaking();

    // WebRTC offer
    const { connection, offerSdp } = await createOfferer();
    pc = connection.pc;

    abort.addEventListener("abort", () => {
      relay?.close();
      pc?.close();
    });

    relay.sendExchange({
      type: "web_rtc",
      sdp: offerSdp,
      ice_candidates: [],
    });

    const peerInfo = await relay.recvExchange();
    applyAnswer(pc, peerInfo.sdp);
    await waitConnected(pc, [
      connection.controlChannel,
      connection.dataChannel,
    ]);
    if (abort.aborted) return;

    const disconnectPromise = new Promise<never>((_, reject) => {
      connection.dataChannel.addEventListener("close", () =>
        reject(new Error("Peer disconnected")),
      );
    });
    disconnectPromise.catch(() => {});

    const decoder = new ControlDecoder();
    const controlMessages: ReceiverToSender[] = [];
    let controlResolve: (() => void) | null = null;

    connection.controlChannel.onmessage = async (ev) => {
      const data =
        ev.data instanceof ArrayBuffer ? ev.data : await ev.data.arrayBuffer();
      const msg = await decoder.onMessage(data);
      if (msg) {
        controlMessages.push(msg as ReceiverToSender);
        controlResolve?.();
      }
    };

    const waitControl = (): Promise<ReceiverToSender> => {
      const inner =
        controlMessages.length > 0
          ? Promise.resolve(controlMessages.shift()!)
          : new Promise<ReceiverToSender>((resolve) => {
              controlResolve = () => {
                controlResolve = null;
                resolve(controlMessages.shift()!);
              };
            });
      return Promise.race([inner, disconnectPromise]);
    };

    await sendControlMessage(connection.controlChannel, {
      type: "ConnRequest",
      version: PROTO_VERSION,
    } as SenderToReceiver);

    const versionResp = await waitControl();
    if (versionResp.type === "WrongVersion") {
      throw new Error(`Version mismatch: peer expects ${versionResp.expected}`);
    }
    if (versionResp.type !== "Ok")
      throw new Error("Unexpected response to version handshake");

    await sendControlMessage(connection.controlChannel, {
      type: "FileInfo",
      files: fileTree,
    } as SenderToReceiver);

    callbacks.onWaitingAccept();

    const acceptResp = await waitControl();
    if (acceptResp.type === "RejectFiles")
      throw new Error("Receiver rejected the files");
    if (acceptResp.type !== "AcceptFilesSkip")
      throw new Error("Unexpected response");

    const sendTrees: FileSendRecvTree[] = [];
    for (let i = 0; i < fileTree.length; i++) {
      const skip = acceptResp.files[i] ?? null;
      const tree = applySkip(fileTree[i], skip);
      if (tree) sendTrees.push(tree);
    }

    const flatFiles = flattenTree(sendTrees);
    callbacks.onTransferring(
      sendTrees.map((t) => ({
        name: t.name,
        size: treeSize(t),
        skip: treeSkip(t),
        isDir: t.type === "Dir",
      })),
    );

    const dc = connection.dataChannel;
    dc.bufferedAmountLowThreshold = MAX_BUFFERED / 2;

    for (const { path, skip, size } of flatFiles) {
      const file = fileMap.get(path);
      if (!file) throw new Error(`File not found: ${path}`);

      let sent = skip;
      while (sent < size) {
        if (abort.aborted) return;

        if (dc.bufferedAmount > MAX_BUFFERED) {
          await Promise.race([
            new Promise<void>((resolve) => {
              dc.onbufferedamountlow = () => resolve();
            }),
            disconnectPromise,
          ]);
        }

        const end = Math.min(sent + DATA_CHUNK_SIZE, size);
        const chunk = await file.slice(sent, end).arrayBuffer();
        dc.send(chunk);
        const bytes = end - sent;
        sent = end;
        callbacks.onProgress(bytes);
      }
    }

    // Wait for receiver to close, confirming all data was consumed.
    try {
      await disconnectPromise;
    } catch {
      /* expected */
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

async function buildFileMap(
  entries: SelectedEntry[],
): Promise<Map<string, File>> {
  const map = new Map<string, File>();

  for (const entry of entries) {
    if (entry.kind === "file") {
      if (entry.handle) {
        const file = await (entry.handle as FileSystemFileHandle).getFile();
        map.set(entry.name, file);
      } else if (entry.file) {
        map.set(entry.name, entry.file);
      }
    } else {
      if (entry.handle) {
        await collectFilesFromHandle(
          entry.handle as FileSystemDirectoryHandle,
          entry.name,
          map,
        );
      } else if (entry.files) {
        for (const { relativePath, file } of entry.files) {
          map.set(`${entry.name}/${relativePath}`, file);
        }
      }
    }
  }

  return map;
}

async function collectFilesFromHandle(
  handle: FileSystemDirectoryHandle,
  prefix: string,
  map: Map<string, File>,
): Promise<void> {
  for await (const [, childHandle] of handle.entries()) {
    const path = `${prefix}/${childHandle.name}`;
    if (childHandle.kind === "file") {
      const file = await (childHandle as FileSystemFileHandle).getFile();
      map.set(path, file);
    } else {
      await collectFilesFromHandle(
        childHandle as FileSystemDirectoryHandle,
        path,
        map,
      );
    }
  }
}
