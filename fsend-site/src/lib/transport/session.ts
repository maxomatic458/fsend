import { Relay } from "./relay";
import {
    createOfferer,
    createAnswerer,
    applyAnswer,
    waitConnected,
    watchDisconnect,
} from "./webrtc";
import { loadProto, parseCode, type ConnectionInfo } from "../proto";

/// A live connection to the peer.
export interface Link {
    controlChannel: RTCDataChannel;
    dataChannel: RTCDataChannel;
    pc: RTCPeerConnection;
    /// Rejects once the peer is gone.
    disconnected: Promise<never>;
    close(): void;
}

function webRtcInfo(sdp: string): ConnectionInfo {
    return { type: "web_rtc", sdp };
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
    const watcher = watchDisconnect(pc, [controlChannel, dataChannel]);
    return {
        controlChannel,
        dataChannel,
        pc,
        disconnected: watcher.promise,
        close() {
            watcher.stop();
            pc.close();
        },
    };
}

export interface SenderSessionHooks {
    onCode(code: string, expiresInSecs: number): void;
    onWaitingPeer(): void;
    onHandshaking(): void;
}

/** Runs `connect`, closing `pc` if it fails or is cancelled; once connected, the transfer tells the peer instead. */
async function settingUp<T>(
    pc: RTCPeerConnection,
    abort: AbortSignal,
    connect: () => Promise<T>,
): Promise<T> {
    const closeOnAbort = () => pc.close();
    abort.addEventListener("abort", closeOnAbort);
    try {
        return await connect();
    } catch (err) {
        pc.close();
        throw err;
    } finally {
        abort.removeEventListener("abort", closeOnAbort);
    }
}

export async function openSenderSession(
    abort: AbortSignal,
    hooks: SenderSessionHooks,
): Promise<Link> {
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
        return await settingUp(pc, abort, async () => {
            relay.sendConnection(webRtcInfo(offerSdp));
            await applyAnswer(pc, sdpOf(await relay.connection()));
            await waitConnected(pc, [controlChannel, dataChannel], abort);
            return assemble(pc, controlChannel, dataChannel);
        });
    } finally {
        relay.close();
    }
}

export async function openReceiverSession(
    code: string,
    abort: AbortSignal,
    hooks: { onHandshaking(): void },
): Promise<Link> {
    await loadProto();
    const parsed = parseCode(code);
    if ("error" in parsed) throw new Error(`Invalid code: ${parsed.message}`);

    const relay = await Relay.connect(abort);
    try {
        await relay.pairAsReceiver(parsed.code);

        const answerer = await createAnswerer(sdpOf(await relay.connection()));
        const { pc } = answerer;
        return await settingUp(pc, abort, async () => {
            relay.sendConnection(webRtcInfo(answerer.answerSdp));
            // The channels only arrive once the peers connect, which may never happen.
            const channels = answerer.channelsReady;
            await waitConnected(
                pc,
                channels.then((c) => [c.controlChannel, c.dataChannel]),
                abort,
            );
            const { controlChannel, dataChannel } = await channels;
            hooks.onHandshaking();
            return assemble(pc, controlChannel, dataChannel);
        });
    } finally {
        relay.close();
    }
}
