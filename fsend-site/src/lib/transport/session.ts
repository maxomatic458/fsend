import { Relay } from "./relay";
import {
  createOfferer,
  createAnswerer,
  applyAnswer,
  waitConnected,
  watchDisconnect,
  type DisconnectWatcher,
} from "./webrtc";
import { loadProto, parseCode, type ConnectionInfo } from "../proto";

/// Live peer to peer connection
export interface Link {
  controlChannel: RTCDataChannel;
  dataChannel: RTCDataChannel;
  pc: RTCPeerConnection;
  peer: DisconnectWatcher;
  /// Rejects the moment the peer disconnects
  disconnected: Promise<never>;
  close(): void;
}

function webRtcInfo(sdp: string): ConnectionInfo {
  return { type: "web_rtc", sdp, ice_candidates: [] };
}

function sdpOf(info: ConnectionInfo): string {
  if (info.type !== "web_rtc") {
    throw new Error("The peer sent connection details for another transport");
  }
  return info.sdp;
}

function assemble(
  pc: RTCPeerConnection,
  controlChannel: RTCDataChannel,
  dataChannel: RTCDataChannel,
): Link {
  const peer = watchDisconnect(pc, [controlChannel, dataChannel]);
  return {
    controlChannel,
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
): Promise<Link | null> {
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
    const closeOnAbort = () => pc.close();
    abort.addEventListener("abort", closeOnAbort);

    relay.sendConnection(webRtcInfo(offerSdp));
    await applyAnswer(pc, sdpOf(await relay.connection()));
    await waitConnected(pc, [controlChannel, dataChannel]);
    // Once connected, the transfer tells the peer it was cancelled.
    abort.removeEventListener("abort", closeOnAbort);
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
): Promise<Link | null> {
  await loadProto();
  const parsed = parseCode(code);
  if ("error" in parsed) throw new Error(`Invalid code: ${parsed.message}`);

  const relay = await Relay.connect(abort);
  try {
    await relay.pairAsReceiver(parsed.code);

    const answerer = await createAnswerer(sdpOf(await relay.connection()));
    const pc = answerer.pc;
    const closeOnAbort = () => pc.close();
    abort.addEventListener("abort", closeOnAbort);

    relay.sendConnection(webRtcInfo(answerer.answerSdp));

    const { controlChannel, dataChannel } = await answerer.channelsReady;
    await waitConnected(pc, [controlChannel, dataChannel]);
    // Once connected, the transfer tells the peer it was cancelled.
    abort.removeEventListener("abort", closeOnAbort);
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
