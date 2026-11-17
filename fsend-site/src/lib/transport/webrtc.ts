import {
    STUN_SERVERS,
    ICE_GATHERING_TIMEOUT_MS,
    CONNECT_TIMEOUT_MS,
    MAX_DATA_MESSAGE_BYTES,
    MAX_MESSAGE_BYTES,
} from "../../config";

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
    if (pc.iceGatheringState === "complete") return Promise.resolve();
    return new Promise((resolve) => {
        const done = () => {
            clearTimeout(timer);
            pc.removeEventListener("icegatheringstatechange", onChange);
            resolve();
        };
        const onChange = () => {
            if (pc.iceGatheringState === "complete") done();
        };
        pc.addEventListener("icegatheringstatechange", onChange);
        const timer = setTimeout(done, ICE_GATHERING_TIMEOUT_MS);
    });
}

export async function createOfferer(): Promise<{
    connection: WebRtcConnection;
    offerSdp: string;
}> {
    const pc = createPeerConnection();
    const controlChannel = pc.createDataChannel("control", { ordered: true });
    const dataChannel = pc.createDataChannel("data", { ordered: true });
    controlChannel.binaryType = "arraybuffer";
    dataChannel.binaryType = "arraybuffer";

    try {
        await pc.setLocalDescription(await pc.createOffer());
        await waitIceGathering(pc);
    } catch (err) {
        pc.close();
        throw err;
    }
    return {
        connection: { pc, controlChannel, dataChannel },
        offerSdp: pc.localDescription!.sdp,
    };
}

export async function createAnswerer(offerSdp: string): Promise<{
    pc: RTCPeerConnection;
    channelsReady: Promise<{
        controlChannel: RTCDataChannel;
        dataChannel: RTCDataChannel;
    }>;
    answerSdp: string;
}> {
    const pc = createPeerConnection();

    const channels = new Map<string, RTCDataChannel>();
    const channelsReady = new Promise<{
        controlChannel: RTCDataChannel;
        dataChannel: RTCDataChannel;
    }>((resolve) => {
        pc.ondatachannel = (ev) => {
            ev.channel.binaryType = "arraybuffer";
            channels.set(ev.channel.label, ev.channel);
            if (channels.size === 2) {
                resolve({
                    controlChannel: channels.get("control")!,
                    dataChannel: channels.get("data")!,
                });
            }
        };
    });

    try {
        await pc.setRemoteDescription(new RTCSessionDescription({ type: "offer", sdp: offerSdp }));
        await pc.setLocalDescription(await pc.createAnswer());
        await waitIceGathering(pc);
    } catch (err) {
        pc.close();
        throw err;
    }

    return {
        pc,
        channelsReady,
        answerSdp: pc.localDescription!.sdp,
    };
}

/** Rejects if the answer is unusable, rather than failing silently. */
export function applyAnswer(pc: RTCPeerConnection, answerSdp: string): Promise<void> {
    return pc.setRemoteDescription(new RTCSessionDescription({ type: "answer", sdp: answerSdp }));
}

/**
 * Resolves once the connection is up and `channels` are open. `channels` may
 * still be on their way, as the answerer's are until the peers connect.
 * Rejects if the connection fails, `timeoutMs` passes, or `abort` fires.
 */
export function waitConnected(
    pc: RTCPeerConnection,
    channels: RTCDataChannel[] | Promise<RTCDataChannel[]>,
    abort: AbortSignal,
    timeoutMs = CONNECT_TIMEOUT_MS,
): Promise<void> {
    return new Promise((resolve, reject) => {
        let opened: RTCDataChannel[] = [];
        let settled = false;
        const cleanup = () => {
            clearTimeout(timer);
            pc.removeEventListener("connectionstatechange", onState);
            abort.removeEventListener("abort", onAbort);
            for (const ch of opened) ch.removeEventListener("open", check);
        };
        const settle = (err?: Error) => {
            if (settled) return;
            settled = true;
            cleanup();
            if (err) reject(err);
            else resolve();
        };

        function check() {
            if (
                opened.length > 0 &&
                pc.connectionState === "connected" &&
                opened.every((ch) => ch.readyState === "open")
            ) {
                settle();
            }
        }

        function onState() {
            if (pc.connectionState === "failed") {
                settle(new Error("WebRTC connection failed"));
                return;
            }
            check();
        }

        function onAbort() {
            settle(new Error("Cancelled"));
        }

        const timer = setTimeout(
            () => settle(new Error("Timed out establishing the connection")),
            timeoutMs,
        );
        if (abort.aborted) return onAbort();
        if (pc.connectionState === "failed") {
            settle(new Error("WebRTC connection failed"));
            return;
        }
        pc.addEventListener("connectionstatechange", onState);
        abort.addEventListener("abort", onAbort);

        Promise.resolve(channels).then((chs) => {
            if (settled) return;
            opened = chs;
            for (const ch of opened) ch.addEventListener("open", check);
            check();
        }, settle);
    });
}

export interface DisconnectWatcher {
    /// Rejects once the peer is gone.
    promise: Promise<never>;
    isDown: () => boolean;
    /// Stop watching
    stop: () => void;
}

const DISCONNECT_GRACE_MS = 3000;

export function watchDisconnect(
    pc: RTCPeerConnection,
    channels: RTCDataChannel[],
): DisconnectWatcher {
    let down = false;
    let stopped = false;
    let graceTimer: ReturnType<typeof setTimeout> | undefined;
    let fail!: () => void;

    const promise = new Promise<never>((_, reject) => {
        fail = () => {
            if (down || stopped) return;
            down = true;
            reject(new Error("Peer disconnected"));
        };
    });
    // Nothing may be awaiting this yet
    promise.catch(() => {});

    const isFatal = (s: string) => s === "failed" || s === "closed";
    // Either transport being down is enough
    const isDown = () =>
        isFatal(pc.connectionState) ||
        isFatal(pc.iceConnectionState) ||
        pc.connectionState === "disconnected" ||
        pc.iceConnectionState === "disconnected";

    const onState = () => {
        if (stopped) return;
        if (isFatal(pc.connectionState) || isFatal(pc.iceConnectionState)) {
            clearTimeout(graceTimer);
            fail();
        } else if (isDown()) {
            clearTimeout(graceTimer);
            graceTimer = setTimeout(() => {
                if (isDown()) fail();
            }, DISCONNECT_GRACE_MS);
        } else {
            clearTimeout(graceTimer);
            graceTimer = undefined;
        }
    };

    const onChannelClose = () => fail();

    pc.addEventListener("connectionstatechange", onState);
    pc.addEventListener("iceconnectionstatechange", onState);
    for (const ch of channels) ch.addEventListener("close", onChannelClose);

    return {
        promise,
        isDown: () => down,
        stop: () => {
            stopped = true;
            clearTimeout(graceTimer);
            pc.removeEventListener("connectionstatechange", onState);
            pc.removeEventListener("iceconnectionstatechange", onState);
            for (const ch of channels) ch.removeEventListener("close", onChannelClose);
        },
    };
}

/** The largest data stream message both ends take, once connected. */
export const dataMessageBytes = (pc: RTCPeerConnection): number =>
    Math.min(pc.sctp?.maxMessageSize || MAX_MESSAGE_BYTES, MAX_DATA_MESSAGE_BYTES);

export async function getConnectionType(
    pc: RTCPeerConnection,
): Promise<"direct" | "relay" | "unknown"> {
    const stats = await pc.getStats();
    for (const report of stats.values()) {
        if (report.type === "candidate-pair" && report.nominated) {
            const localId = report.localCandidateId;
            const local = stats.get(localId);
            if (local?.candidateType === "relay") return "relay";
            const remoteId = report.remoteCandidateId;
            const remote = stats.get(remoteId);
            if (remote?.candidateType === "relay") return "relay";
            return "direct";
        }
    }
    return "unknown";
}
