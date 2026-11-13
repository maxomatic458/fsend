import { Relay } from "./relay";
import {
  createOfferer,
  createAnswerer,
  applyAnswer,
  waitConnected,
  watchDisconnect,
  type DisconnectWatcher,
} from "./webrtc";
import { createControlChannel, type ControlChannel } from "./control";
import { loadProto, parseCode, type ConnectionInfo } from "../proto";
import type { SenderToReceiver, ReceiverToSender } from "../types";

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

function webRtcInfo(sdp: string): ConnectionInfo {
  return { type: "web_rtc", sdp, ice_candidates: [] };
}

function sdpOf(info: ConnectionInfo): string {
  if (info.type !== "web_rtc") {
    throw new Error("The peer sent connection details for another transport");
  }
  return info.sdp;
}

function assemble<In, Out>(
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
      pc.close();
    },
  };
}

export interface SenderSessionHooks {
  onCode(code: string, expiresInSecs: number): void;
  onWaitingPeer(): void;
  onHandshaking(): void;
}

export async function openSenderSession(
  abort: AbortSignal,
  hooks: SenderSessionHooks,
): Promise<SenderSession | null> {
  await loadProto();
  const relay = await Relay.connect(abort);
  try {
    await relay.pairAsSender((code, expiresInSecs) => {
      hooks.onCode(code, expiresInSecs);
      hooks.onWaitingPeer();
    });
    hooks.onHandshaking();

    const { connection, offerSdp } = await createOfferer();
    const { pc, controlChannel, dataChannel } = connection;
    abort.addEventListener("abort", () => pc.close());

    relay.sendConnection(webRtcInfo(offerSdp));
    await applyAnswer(pc, sdpOf(await relay.connection()));
    await waitConnected(pc, [controlChannel, dataChannel]);
    if (abort.aborted) {
      pc.close();
      return null;
    }

    return assemble(pc, controlChannel, dataChannel);
  } finally {
    relay.close();
  }
}

export async function openReceiverSession(
  code: string,
  abort: AbortSignal,
  hooks: { onHandshaking(): void },
): Promise<ReceiverSession | null> {
  await loadProto();
  const parsed = parseCode(code);
  if ("error" in parsed) throw new Error(`Invalid code: ${parsed.message}`);

  const relay = await Relay.connect(abort);
  try {
    await relay.pairAsReceiver(parsed.code);

    const answerer = await createAnswerer(sdpOf(await relay.connection()));
    const pc = answerer.pc;
    abort.addEventListener("abort", () => pc.close());

    relay.sendConnection(webRtcInfo(answerer.answerSdp));

    const { controlChannel, dataChannel } = await answerer.channelsReady;
    await waitConnected(pc, [controlChannel, dataChannel]);
    if (abort.aborted) {
      pc.close();
      return null;
    }
    hooks.onHandshaking();

    return assemble(pc, controlChannel, dataChannel);
  } finally {
    relay.close();
  }
}
