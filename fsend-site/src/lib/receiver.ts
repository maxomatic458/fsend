import { RELAY_URL, PROTO_VERSION } from '../config';
import { RelayClient } from './relay';
import { createAnswerer, waitConnected } from './webrtc';
import { sendControlMessage, ControlDecoder } from './protocol';
import { flattenTree, applySkip, treeSize, treeSkip } from './fileTree';
import { getExistingFileSizes } from './fsAccess';
import type {
  FilesAvailable,
  FileSendRecvTree,
  SenderToReceiver,
  ReceiverToSender,
} from './types';

export interface ReceiverCallbacks {
  onConnecting: () => void;
  onHandshaking: () => void;
  onFilesOffered: (files: FilesAvailable[], accept: () => void, reject: () => void) => void;
  onTransferring: (entries: Array<{ name: string; size: number; skip: number; isDir: boolean }>) => void;
  onProgress: (bytes: number) => void;
  onComplete: () => void;
  onError: (error: string) => void;
  onConnectionType: (type: 'direct' | 'relay' | 'unknown') => void;
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

    // Close relay + pc immediately when aborted, unblocking all pending awaits
    abort.addEventListener('abort', () => { relay?.close(); pc?.close(); });

    relay.sendExchange({
      type: 'web_rtc',
      sdp: answerer.answerSdp,
      ice_candidates: [],
    });

    const { controlChannel, dataChannel } = await answerer.channelsReady;
    await waitConnected(pc, [controlChannel, dataChannel]);
    if (abort.aborted) return;

    // Disconnect detection: use data channel 'close' event to avoid false positives
    // from transient 'disconnected' pc state. Flag prevents self-trigger when we
    // intentionally close pc after transfer completes.
    let transferDone = false;
    const disconnectPromise = new Promise<never>((_, reject) => {
      dataChannel.addEventListener('close', () => {
        if (!transferDone) reject(new Error('Peer disconnected'));
      });
    });
    disconnectPromise.catch(() => {});

    callbacks.onHandshaking();

    // Set up control decoder
    const decoder = new ControlDecoder();
    const controlMessages: SenderToReceiver[] = [];
    let controlResolve: (() => void) | null = null;

    controlChannel.onmessage = async (ev) => {
      const data = ev.data instanceof ArrayBuffer ? ev.data : await ev.data.arrayBuffer();
      const msg = await decoder.onMessage(data);
      if (msg) {
        controlMessages.push(msg as SenderToReceiver);
        controlResolve?.();
      }
    };

    const waitControl = (): Promise<SenderToReceiver> => {
      const inner = controlMessages.length > 0
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
    if (connReq.type !== 'ConnRequest') throw new Error('Expected ConnRequest');
    if (connReq.version !== PROTO_VERSION) {
      await sendControlMessage(controlChannel, {
        type: 'WrongVersion',
        expected: PROTO_VERSION,
      } as ReceiverToSender);
      throw new Error(`Version mismatch: sender has ${connReq.version}, we need ${PROTO_VERSION}`);
    }
    await sendControlMessage(controlChannel, { type: 'Ok' } as ReceiverToSender);

    // Receive file info
    const fileInfoMsg = await waitControl();
    if (fileInfoMsg.type !== 'FileInfo') throw new Error('Expected FileInfo');
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
        type: 'RejectFiles',
      } as ReceiverToSender);
      throw new Error('Transfer rejected');
    }

    // Compute skip info
    let skipInfo: (import('./types').FilesToSkip | null)[];
    if (resume) {
      skipInfo = await getExistingFileSizes(dirHandle, offeredFiles);
      console.log('[receiver] resume skip info:', JSON.stringify(skipInfo));
    } else {
      skipInfo = offeredFiles.map(() => null);
    }

    await sendControlMessage(controlChannel, {
      type: 'AcceptFilesSkip',
      files: skipInfo,
    } as ReceiverToSender);

    // Build receive tree
    const recvTrees: FileSendRecvTree[] = [];
    for (let i = 0; i < offeredFiles.length; i++) {
      const tree = applySkip(offeredFiles[i], skipInfo[i]);
      if (tree) recvTrees.push(tree);
    }

    const flatFiles = flattenTree(recvTrees);
    console.log('[receiver] flatFiles:', flatFiles.map(f => `${f.path} skip=${f.skip} size=${f.size}`));
    callbacks.onTransferring(
      recvTrees.map((t) => ({ name: t.name, size: treeSize(t), skip: treeSkip(t), isDir: t.type === 'Dir' })),
    );

    // Handle empty transfer (all files fully skipped or no files)
    if (flatFiles.length === 0) {
      console.log('[receiver] all files already complete, skipping transfer');
      callbacks.onComplete();
      return;
    }

    // Receive file data using a sequential write queue
    let fileIdx = 0;
    let fileWritten = flatFiles[0].skip;
    let totalBytesReceived = flatFiles.reduce((s, f) => s + f.skip, 0);
    console.log(`[receiver] starting write queue: fileIdx=0 fileWritten=${fileWritten} totalBytesReceived=${totalBytesReceived}`);

    const openFile = async (path: string, skip: number) => {
      console.log(`[receiver] opening file: ${path} skip=${skip}`);
      const parts = path.split('/');
      let dir = dirHandle;
      for (let i = 0; i < parts.length - 1; i++) {
        dir = await dir.getDirectoryHandle(parts[i], { create: true });
      }
      const fh = await dir.getFileHandle(parts[parts.length - 1], { create: true });
      const writable = await fh.createWritable({ keepExistingData: skip > 0 });
      if (skip > 0) await writable.seek(skip);
      return writable;
    };

    // Register onmessage BEFORE openFile so chunks that arrive during the
    // async seek (slow for large resume offsets) are buffered rather than lost.
    const chunkQueue: Uint8Array[] = [];
    let chunksReceived = 0;
    dataChannel.onmessage = (ev) => {
      if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0) return;
      chunksReceived++;
      chunkQueue.push(new Uint8Array(ev.data));
    };

    currentWritable = await openFile(flatFiles[0].path, flatFiles[0].skip);

    const dataPromise = new Promise<void>((resolve, reject) => {
      // Queue for serializing async writes (chunkQueue already populated above)
      let processing = false;
      let chunksProcessed = 0;

      const logMB = (n: number) => (n / 1048576).toFixed(2) + ' MB';

      const processQueue = async () => {
        if (processing) return;
        processing = true;
        try {
          while (chunkQueue.length > 0) {
            const chunk = chunkQueue.shift()!;
            if (fileIdx >= flatFiles.length) break;

            await currentWritable!.write(chunk as any);
            chunksProcessed++;
            fileWritten += chunk.length;
            totalBytesReceived += chunk.length;
            callbacks.onProgress(chunk.length);

            // Log every ~10 MB
            if (chunksProcessed % 640 === 0) {
              console.log(`[receiver] progress: ${logMB(fileWritten)}/${logMB(flatFiles[fileIdx]?.size ?? 0)} | chunks recv=${chunksReceived} proc=${chunksProcessed} queue=${chunkQueue.length}`);
            }

            // Advance to next file(s) if current is complete
            while (fileIdx < flatFiles.length && fileWritten >= flatFiles[fileIdx].size) {
              console.log(`[receiver] file complete: fileWritten=${logMB(fileWritten)} size=${logMB(flatFiles[fileIdx].size)}`);
              await currentWritable!.close();
              currentWritable = null;
              fileIdx++;
              if (fileIdx < flatFiles.length) {
                fileWritten = flatFiles[fileIdx].skip;
                currentWritable = await openFile(flatFiles[fileIdx].path, flatFiles[fileIdx].skip);
              }
            }

            // All files received
            if (fileIdx >= flatFiles.length) {
              console.log(`[receiver] all files complete: ${logMB(totalBytesReceived)} total, chunks recv=${chunksReceived} proc=${chunksProcessed}`);
              resolve();
              return;
            }
          }
          // Queue drained — log so we can see if messages stop arriving
          console.log(`[receiver] queue drained: ${logMB(fileWritten)} written, chunks recv=${chunksReceived} proc=${chunksProcessed} | waiting for more data...`);
        } catch (err) {
          console.error('[receiver] write queue error:', err);
          reject(err);
        } finally {
          processing = false;
        }
      };

      // Wire onmessage to also trigger processQueue now that writable is ready.
      // Drain any chunks that arrived during openFile first.
      dataChannel.onmessage = (ev) => {
        if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength === 0) return;
        chunksReceived++;
        chunkQueue.push(new Uint8Array(ev.data));
        processQueue();
      };

      dataChannel.addEventListener('close', () => {
        console.log(`[receiver] data channel closed — fileIdx=${fileIdx}/${flatFiles.length} fileWritten=${logMB(fileWritten)} chunks recv=${chunksReceived} proc=${chunksProcessed} queue=${chunkQueue.length}`);
      });

      // Drain any chunks buffered before writable was ready
      processQueue();
    });

    await Promise.race([dataPromise, disconnectPromise]);
    transferDone = true;
    pc?.close();
    callbacks.onComplete();
  } catch (err: any) {
    console.log(`[receiver] caught error (aborted=${abort.aborted}):`, err);
    if (!abort.aborted) {
      callbacks.onError(err.message ?? String(err));
    }
  } finally {
    console.log('[receiver] finally: closing writable and connection');
    // Close the active writable to flush partial data to disk (enables resume)
    await currentWritable?.close().catch(() => {});
    relay?.close();
    pc?.close();
  }
}
