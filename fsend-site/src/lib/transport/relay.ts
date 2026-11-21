import { RELAY_URL } from "../../config";
import {
    NAMEPLATE_LENGTH,
    Pake,
    RELAY_VERSION,
    Role,
    newCode,
    parseServerMessage,
    validateOffer,
    type Bytes,
    type Channel,
    type ClientMessage,
    type ConnectionInfo,
    type Offer,
    type RelayError,
    type Sealed,
    type ServerMessage,
    type Transport,
} from "../proto";

const APP = "fsend-web";
const TRANSPORTS: Transport[] = ["webrtc"];

type Reply = Exclude<ServerMessage, { type: "error" }>;

/** The receiver's answer to the offer. */
export type Answer = Extract<Sealed, { type: "accept" | "reject" }>;

/** A WebSocket to the relay; pair it first, then swap connection info. */
export class Relay {
    private frames: (string | Uint8Array)[] = [];
    private closed = false;
    private wake: (() => void) | null = null;
    private channel: Channel | null = null;
    private onClose: () => void = () => {};
    private whenClosed = new Promise<void>((resolve) => (this.onClose = resolve));

    private constructor(private ws: WebSocket) {
        ws.onmessage = (ev) => {
            const data = ev.data;
            this.frames.push(typeof data === "string" ? data : new Uint8Array(data));
            this.notify();
        };
        ws.onclose = () => {
            this.closed = true;
            this.notify();
            this.onClose();
        };
    }

    /** Connects; aborting closes the connection, failing whatever waits on it. */
    static connect(abort: AbortSignal): Promise<Relay> {
        abort.throwIfAborted();
        const ws = new WebSocket(`${RELAY_URL.replace(/\/+$/, "")}/relay`);
        ws.binaryType = "arraybuffer";
        abort.addEventListener("abort", () => ws.close());
        return new Promise((resolve, reject) => {
            ws.onopen = () => resolve(new Relay(ws));
            ws.onclose = () => reject(new Error("Could not reach the relay"));
        });
    }

    /** Creates a session, pairs with whoever joins it and offers them `plaintext`, from `encodeOffer`. */
    async pairAsSender(
        plaintext: Uint8Array,
        onCode: (code: string, expiresInSecs: number) => void,
    ): Promise<void> {
        this.send({
            type: "create",
            version: RELAY_VERSION,
            transports: TRANSPORTS,
        });
        const { nameplate, expires_in } = await this.expect("created");
        const code = newCode(nameplate);
        if (!code) throw pairingError("the relay assigned an invalid nameplate");
        onCode(code, expires_in);
        await this.expect("paired");
        await this.handshake(code, Role.Sender);
        this.sendFrames(this.paired().sealPlaintext(plaintext));
    }

    /** Joins the session of `code`; returns the sender's offer, checked. */
    async pairAsReceiver(code: string): Promise<Offer> {
        this.send({
            type: "join",
            version: RELAY_VERSION,
            nameplate: code.slice(0, NAMEPLATE_LENGTH),
            transports: TRANSPORTS,
        });
        await this.expect("paired");
        await this.handshake(code, Role.Receiver);
        const message = await this.sealed();
        if (message.type !== "offer") throw pairingError("expected an offer");
        const { type: _, ...offer } = message;
        try {
            validateOffer(offer);
        } catch (err) {
            const reason = err instanceof Error ? err.message : String(err);
            throw new Error(`The sender offered files that cannot be received: ${reason}`);
        }
        return offer;
    }

    /** SPAKE2, then each side's sealed confirm proves both used the same code. */
    private async handshake(code: string, role: Role): Promise<void> {
        const pake = Pake.start(code);
        this.ws.send(pake.message);
        this.channel = pake.finish(await this.signal(), role);
        this.sendSealed({ type: "confirm", app: APP });
        if ((await this.sealed()).type !== "confirm") {
            throw pairingError("expected a confirm message");
        }
    }

    /** Sender: waits for the receiver's user to answer the offer. */
    async answer(): Promise<Answer> {
        const message = await this.sealed();
        if (message.type !== "accept" && message.type !== "reject") {
            throw pairingError("expected an answer to the offer");
        }
        return message;
    }

    accept(): void {
        this.sendSealed({ type: "accept" });
    }

    /** Ends the session. */
    reject(): void {
        this.sendSealed({ type: "reject" });
        this.close();
    }

    /** Fails once the relay ends the session, as it does when the other side leaves. */
    async ended(): Promise<never> {
        await this.whenClosed;
        await this.next();
        throw pairingError("unexpected message");
    }

    sendConnection(info: ConnectionInfo): void {
        this.sendSealed({ type: "connection", info });
    }

    async connection(): Promise<ConnectionInfo> {
        const message = await this.sealed();
        if (message.type !== "connection") {
            throw pairingError("expected connection details");
        }
        return message.info;
    }

    close(): void {
        this.ws.close();
    }

    private paired(): Channel {
        if (!this.channel) throw pairingError("not paired yet");
        return this.channel;
    }

    private send(message: ClientMessage): void {
        this.ws.send(JSON.stringify(message));
    }

    private sendSealed(message: Sealed): void {
        this.sendFrames(this.paired().seal(message));
    }

    private sendFrames(frames: Bytes[]): void {
        for (const frame of frames) this.ws.send(frame);
    }

    /** The peer's next sealed message, which may span several frames. */
    private async sealed(): Promise<Sealed> {
        for (;;) {
            const message = this.paired().open(await this.signal());
            if (message) return message;
        }
    }

    private async expect<T extends Reply["type"]>(type: T): Promise<Extract<Reply, { type: T }>> {
        const frame = await this.next();
        if (frame instanceof Uint8Array || frame.type !== type) {
            throw pairingError(`expected ${type}`);
        }
        return frame as Extract<Reply, { type: T }>;
    }

    private async signal(): Promise<Uint8Array> {
        const frame = await this.next();
        if (!(frame instanceof Uint8Array)) {
            throw pairingError(`unexpected ${frame.type} message`);
        }
        return frame;
    }

    /** The next signal or relay message; a relay error throws. */
    private async next(): Promise<Uint8Array | Reply> {
        for (;;) {
            const frame = this.frames.shift();
            if (frame instanceof Uint8Array) return frame;
            if (frame === undefined) {
                if (this.closed) throw new Error("Lost the connection to the relay.");
                await new Promise<void>((resolve) => (this.wake = resolve));
                continue;
            }
            const message = parse(frame);
            if (message.type === "error") throw new Error(describe(message.code));
            return message;
        }
    }

    private notify(): void {
        this.wake?.();
        this.wake = null;
    }
}

function parse(frame: string): ServerMessage {
    try {
        return parseServerMessage(frame);
    } catch {
        throw pairingError("invalid relay message");
    }
}

function pairingError(message: string): Error {
    return new Error(`Pairing failed: ${message}.`);
}

function describe(error: RelayError): string {
    switch (error) {
        case "not_found":
            return "No transfer with this code. Check the code; a code works only once.";
        case "expired":
            return "The code has expired. Start a new transfer.";
        case "version":
            return "This page is out of date. Reload it and try again.";
        case "no_common_transport":
            return "The other side cannot connect to a browser.";
        case "peer_left":
            return "The other side left.";
        case "busy":
            return "The relay is busy. Try again in a moment.";
        case "protocol":
            return "The relay refused a message from this page. Reload it and try again.";
    }
}
