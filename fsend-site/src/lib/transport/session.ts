import { Relay } from "./relay";
import {
    createOfferer,
    createAnswerer,
    applyAnswer,
    waitConnected,
    watchDisconnect,
} from "./webrtc";
import { loadProto, parseCode, type Bytes, type ConnectionInfo, type Offer } from "../proto";

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
    /** The receiver joined and has the offer; its user decides next. */
    onOffered(): void;
    /** The receiver accepted; the peers connect next. */
    onAccepted(): void;
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

/** Offers `plaintext`, from `encodeOffer`, and connects once the receiver accepts. */
export async function openSenderSession(
    plaintext: Bytes,
    abort: AbortSignal,
    hooks: SenderSessionHooks,
): Promise<Link> {
    await loadProto();
    const relay = await Relay.connect(abort);
    try {
        await relay.pairAsSender(plaintext, hooks.onCode);
        hooks.onOffered();

        // Ready to connect once accepted; none of it reaches the receiver before.
        const offerer = createOfferer();
        let prepared;
        try {
            const answer = await relay.answer();
            if (answer.type === "reject") {
                throw new Error("The receiver rejected the files");
            }
            hooks.onAccepted();
            prepared = await offerer;
        } catch (err) {
            offerer.then(
                ({ connection }) => connection.pc.close(),
                () => {},
            );
            throw err;
        }

        const { connection, offerSdp } = prepared;
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

/** Joins the session of `code`; returns the paired relay and the sender's offer. */
export async function pairReceiver(
    code: string,
    abort: AbortSignal,
): Promise<{ relay: Relay; offer: Offer }> {
    await loadProto();
    const parsed = parseCode(code);
    if ("error" in parsed) throw new Error(`Invalid code: ${parsed.message}`);

    const relay = await Relay.connect(abort);
    try {
        return { relay, offer: await relay.pairAsReceiver(parsed.code) };
    } catch (err) {
        relay.close();
        throw err;
    }
}

/** Accepts the offer over `relay`, which is closed afterwards, and connects to the sender. */
export async function connectReceiver(relay: Relay, abort: AbortSignal): Promise<Link> {
    try {
        relay.accept();
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
            return assemble(pc, controlChannel, dataChannel);
        });
    } finally {
        relay.close();
    }
}
