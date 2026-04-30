import { STUN_SERVERS } from '../config';

export interface WebRtcConnection {
  pc: RTCPeerConnection;
  controlChannel: RTCDataChannel;
  dataChannel: RTCDataChannel;
}

function createPeerConnection(): RTCPeerConnection {
  return new RTCPeerConnection({
    iceServers: [{ urls: STUN_SERVERS }],
  });
}

function waitIceGathering(pc: RTCPeerConnection): Promise<void> {
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return new Promise((resolve) => {
    pc.onicegatheringstatechange = () => {
      if (pc.iceGatheringState === 'complete') resolve();
    };
  });
}

export async function createOfferer(): Promise<{
  connection: WebRtcConnection;
  offerSdp: string;
}> {
  const pc = createPeerConnection();
  const controlChannel = pc.createDataChannel('control', { ordered: true });
  const dataChannel = pc.createDataChannel('data', { ordered: true });
  controlChannel.binaryType = 'arraybuffer';
  dataChannel.binaryType = 'arraybuffer';

  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);
  await waitIceGathering(pc);

  return {
    connection: { pc, controlChannel, dataChannel },
    offerSdp: pc.localDescription!.sdp,
  };
}

export async function createAnswerer(offerSdp: string): Promise<{
  pc: RTCPeerConnection;
  channelsReady: Promise<{ controlChannel: RTCDataChannel; dataChannel: RTCDataChannel }>;
  answerSdp: string;
}> {
  const pc = createPeerConnection();

  const channels = new Map<string, RTCDataChannel>();
  const channelsReady = new Promise<{ controlChannel: RTCDataChannel; dataChannel: RTCDataChannel }>((resolve) => {
    pc.ondatachannel = (ev) => {
      ev.channel.binaryType = 'arraybuffer';
      channels.set(ev.channel.label, ev.channel);
      if (channels.size === 2) {
        resolve({
          controlChannel: channels.get('control')!,
          dataChannel: channels.get('data')!,
        });
      }
    };
  });

  await pc.setRemoteDescription(new RTCSessionDescription({ type: 'offer', sdp: offerSdp }));
  const answer = await pc.createAnswer();
  await pc.setLocalDescription(answer);
  await waitIceGathering(pc);

  return {
    pc,
    channelsReady,
    answerSdp: pc.localDescription!.sdp,
  };
}

export function applyAnswer(pc: RTCPeerConnection, answerSdp: string): void {
  pc.setRemoteDescription(new RTCSessionDescription({ type: 'answer', sdp: answerSdp }));
}

export function waitConnected(pc: RTCPeerConnection, channels: RTCDataChannel[]): Promise<void> {
  return new Promise((resolve, reject) => {
    const check = () => {
      if (
        (pc.connectionState === 'connected' || pc.connectionState === 'completed' as string) &&
        channels.every((ch) => ch.readyState === 'open')
      ) {
        resolve();
      }
    };

    if (pc.connectionState === 'failed') {
      reject(new Error('WebRTC connection failed'));
      return;
    }

    pc.onconnectionstatechange = () => {
      if (pc.connectionState === 'failed') {
        reject(new Error('WebRTC connection failed'));
      }
      check();
    };

    for (const ch of channels) {
      ch.onopen = check;
    }

    check();
  });
}

export async function getConnectionType(
  pc: RTCPeerConnection,
): Promise<'direct' | 'relay' | 'unknown'> {
  const stats = await pc.getStats();
  for (const report of stats.values()) {
    if (report.type === 'candidate-pair' && report.nominated) {
      const localId = report.localCandidateId;
      const local = stats.get(localId);
      if (local?.candidateType === 'relay') return 'relay';
      const remoteId = report.remoteCandidateId;
      const remote = stats.get(remoteId);
      if (remote?.candidateType === 'relay') return 'relay';
      return 'direct';
    }
  }
  return 'unknown';
}
