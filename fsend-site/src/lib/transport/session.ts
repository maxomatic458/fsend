import { RELAY_URL } from "../../config";
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
  /// Rejects the moment the peer disconnects
  disconnected: Promise<never>;
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
): Session<In, Out> {
  const peer = watchDisconnect(pc, [controlChannel, dataChannel]);
  return {
    control: createControlChannel<In, Out>(controlChannel, peer.promise),
    dataChannel,
    pc,
    peer,
    disconnected: peer.promise,
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

export async function openSenderSession(
  abort: AbortSignal,
  hooks: SenderSessionHooks,
): Promise<SenderSession | null> {
  const relay = await RelayClient.connect(RELAY_URL);
  if (abort.aborted) {
    relay.close();
    return null;
  }

  const code = await relay.createSession();
  hooks.onCode(code);
  hooks.onWaitingPeer();

  await relay.waitForPeer();
  if (abort.aborted) {
    relay.close();
    return null;
  }
  hooks.onHandshaking();

  const { connection, offerSdp } = await createOfferer();
  const { pc, controlChannel, dataChannel } = connection;
  abort.addEventListener("abort", () => {
    relay.close();
    pc.close();
  });

  relay.sendExchange(exchange(offerSdp));
  const peerInfo = await relay.recvExchange();
  await applyAnswer(pc, peerInfo.sdp);
  await waitConnected(pc, [controlChannel, dataChannel]);
  if (abort.aborted) {
    relay.close();
    pc.close();
    return null;
  }

  return assemble(relay, pc, controlChannel, dataChannel);
}

export async function openReceiverSession(
  code: string,
  abort: AbortSignal,
  hooks: { onHandshaking(): void },
): Promise<ReceiverSession | null> {
  const relay = await RelayClient.connect(RELAY_URL);
  if (abort.aborted) {
    relay.close();
    return null;
  }

  await relay.joinSession(code.toUpperCase());

  const senderInfo = await relay.recvExchange();
  if (abort.aborted) {
    relay.close();
    return null;
  }

  const answerer = await createAnswerer(senderInfo.sdp);
  const pc = answerer.pc;
  abort.addEventListener("abort", () => {
    relay.close();
    pc.close();
  });

  relay.sendExchange(exchange(answerer.answerSdp));

  const { controlChannel, dataChannel } = await answerer.channelsReady;
  await waitConnected(pc, [controlChannel, dataChannel]);
  if (abort.aborted) {
    relay.close();
    pc.close();
    return null;
  }
  hooks.onHandshaking();

  return assemble(relay, pc, controlChannel, dataChannel);
}
