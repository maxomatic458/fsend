import { RELAY_URL, GOODBYE_TIMEOUT_MS } from "../../config";
import { abortable } from "../abort";
import { RelayClient } from "./relay";
import {
  createOfferer,
  createAnswerer,
  applyAnswer,
  waitConnected,
  watchDisconnect,
  type DisconnectWatcher,
} from "./webrtc";
import { createControlChannel, type ControlChannel } from "./control";
import type {
  SenderToReceiver,
  ReceiverToSender,
  ConnectionInfo,
} from "../types";

/// Live peer to peer session
export interface Session<In, Out> {
  control: ControlChannel<In, Out>;
  dataChannel: RTCDataChannel;
  pc: RTCPeerConnection;
  peer: DisconnectWatcher;
  /// Rejects the moment the peer disconnects or the session is aborted
  disconnected: Promise<never>;
  /// Waits for the peer to hang up, so our last message reaches it.
  goodbye(): Promise<void>;
  close(): void;
}

export type SenderSession = Session<ReceiverToSender, SenderToReceiver>;
export type ReceiverSession = Session<SenderToReceiver, ReceiverToSender>;

function exchange(sdp: string): ConnectionInfo {
  return { type: "web_rtc", sdp, ice_candidates: [] };
}

function assemble<In, Out>(
  relay: RelayClient,
  pc: RTCPeerConnection,
  controlChannel: RTCDataChannel,
  dataChannel: RTCDataChannel,
  abort: AbortSignal,
): Session<In, Out> {
  const peer = watchDisconnect(pc, [controlChannel, dataChannel]);
  const disconnected = abortable(peer.promise, abort);
  disconnected.catch(() => {});
  return {
    control: createControlChannel<In, Out>(controlChannel, disconnected),
    dataChannel,
    pc,
    peer,
    disconnected,
    goodbye: () =>
      new Promise((resolve) => {
        const timer = setTimeout(resolve, GOODBYE_TIMEOUT_MS);
        disconnected.catch(() => {
          clearTimeout(timer);
          resolve();
        });
      }),
    close() {
      peer.stop();
      relay.close();
      pc.close();
    },
  };
}

export interface SenderSessionHooks {
  onCode(code: string): void;
  onWaitingPeer(): void;
  onHandshaking(): void;
}

/** Rejects if the session cannot be set up, including when `abort` fires. */
export async function openSenderSession(
  abort: AbortSignal,
  hooks: SenderSessionHooks,
): Promise<SenderSession> {
  const relay = await RelayClient.connect(RELAY_URL, abort);
  let pc: RTCPeerConnection | undefined;
  try {
    hooks.onCode(await relay.createSession());
    hooks.onWaitingPeer();
    await relay.waitForPeer();
    hooks.onHandshaking();

    const { connection, offerSdp } = await createOfferer();
    const { controlChannel, dataChannel } = connection;
    pc = connection.pc;
    relay.sendExchange(exchange(offerSdp));
    await applyAnswer(pc, (await relay.recvExchange()).sdp);
    await waitConnected(pc, [controlChannel, dataChannel], abort);
    return assemble(relay, pc, controlChannel, dataChannel, abort);
  } catch (err) {
    relay.close();
    pc?.close();
    throw err;
  }
}

/** Rejects if the session cannot be set up, including when `abort` fires. */
export async function openReceiverSession(
  code: string,
  abort: AbortSignal,
  hooks: { onHandshaking(): void },
): Promise<ReceiverSession> {
  const relay = await RelayClient.connect(RELAY_URL, abort);
  let pc: RTCPeerConnection | undefined;
  try {
    await relay.joinSession(code.toUpperCase());
    const answerer = await createAnswerer((await relay.recvExchange()).sdp);
    pc = answerer.pc;
    relay.sendExchange(exchange(answerer.answerSdp));

    const { controlChannel, dataChannel } = await abortable(
      answerer.channelsReady,
      abort,
    );
    await waitConnected(pc, [controlChannel, dataChannel], abort);
    hooks.onHandshaking();
    return assemble(relay, pc, controlChannel, dataChannel, abort);
  } catch (err) {
    relay.close();
    pc?.close();
    throw err;
  }
}
