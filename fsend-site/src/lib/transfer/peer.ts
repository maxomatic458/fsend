import { MAX_MESSAGE_BYTES } from "../../config";
import {
    GOODBYE_TIMEOUT_MS,
    FrameDecoder,
    Role,
    encodeFrame,
    type AcceptedFile,
    type Entry,
    type ErrorReport,
    type Offer,
    type PeerMessage,
    type ReceiverMessage,
    type SenderMessage,
    type Bytes,
} from "../proto";
import { nodeSize, presentBytes } from "../files/tree";
import type { Link } from "../transport/session";
import type { TransferEvent } from "./events";

/** A failed transfer, with the `error` the peer is told if it should know. */
export class TransferFailure extends Error {
    constructor(
        message: string,
        readonly report?: ErrorReport,
    ) {
        super(message);
    }
}

export const messageOf = (err: unknown): string =>
    err instanceof Error ? err.message : String(err);

export const protocolError = (detail: string): TransferFailure =>
    new TransferFailure(`Protocol error: ${detail}`, {
        code: "protocol",
        message: detail,
    });

/** What a message means where the flow expects another one. */
export function unexpected(message: PeerMessage): TransferFailure {
    switch (message.type) {
        case "error":
            return new TransferFailure(`The other side failed: ${message.message}`);
        case "cancel":
            return new TransferFailure("The other side cancelled the transfer");
        default:
            return protocolError(`unexpected ${message.type} message`);
    }
}

/** What both sides send. */
type Common = Extract<SenderMessage, ReceiverMessage>;

/** What the peer sent on either channel. */
type Incoming<In> = In | Uint8Array;

/** Items in arrival order; fails once the link stops. */
class Inbox<T> {
    private items: T[] = [];
    private waiting?: {
        resolve: (item: T) => void;
        reject: (error: Error) => void;
    };
    private error?: Error;

    push(item: T): void {
        // Nothing that arrives after a failure counts.
        if (this.error) return;
        const waiting = this.waiting;
        this.waiting = undefined;
        if (waiting) waiting.resolve(item);
        else this.items.push(item);
    }

    fail(error: Error): void {
        this.error ??= error;
        this.waiting?.reject(this.error);
        this.waiting = undefined;
    }

    /** The next item, if it has already arrived and `take` wants it. */
    pollIf(take: (item: T) => boolean): T | undefined {
        if (this.items.length > 0 && take(this.items[0])) return this.items.shift();
        return undefined;
    }

    /** An item that has already arrived, if any. */
    poll(): T | undefined {
        if (this.items.length > 0) return this.items.shift();
        if (this.error) throw this.error;
        return undefined;
    }

    /** The next item, failing after `timeoutMs` with a timeout that names `what`. */
    async next(timeoutMs?: number, what = ""): Promise<T> {
        const item = this.poll();
        if (item !== undefined) return item;
        return new Promise((resolve, reject) => {
            const timer =
                timeoutMs === undefined
                    ? undefined
                    : unref(
                          setTimeout(() => {
                              this.waiting = undefined;
                              reject(
                                  new TransferFailure(`Timed out: ${what}`, {
                                      code: "timeout",
                                      message: what,
                                  }),
                              );
                          }, timeoutMs),
                      );
            const settled =
                <T>(settle: (value: T) => void) =>
                (value: T) => {
                    clearTimeout(timer);
                    settle(value);
                };
            this.waiting = { resolve: settled(resolve), reject: settled(reject) };
        });
    }
}

/**
 * The peer protocol over a link: messages on the control channel, chunks on the data channel.
 * We send `Out`, the peer sends `In`.
 */
export class Peer<Out extends PeerMessage, In extends PeerMessage> {
    /** Rejects once the peer is gone or the user aborted. */
    readonly stopped: Promise<never>;
    private inbox = new Inbox<Incoming<In>>();
    private decoder: FrameDecoder<In>;
    private gone = false;

    static sender(link: Link, abort: AbortSignal): SenderPeer {
        return new Peer<SenderMessage, ReceiverMessage>(link, abort, Role.Sender);
    }

    static receiver(link: Link, abort: AbortSignal): ReceiverPeer {
        return new Peer<ReceiverMessage, SenderMessage>(link, abort, Role.Receiver);
    }

    private constructor(
        private link: Link,
        private abort: AbortSignal,
        private role: Role,
    ) {
        this.decoder = new FrameDecoder(role === Role.Sender ? Role.Receiver : Role.Sender);
        this.stopped = new Promise<never>((_, reject) => {
            link.disconnected.catch(reject);
            const aborted = () => reject(new DOMException("Aborted", "AbortError"));
            if (abort.aborted) aborted();
            else abort.addEventListener("abort", aborted, { once: true });
        });
        link.disconnected.catch(() => (this.gone = true));
        this.stopped.catch((error) => this.inbox.fail(error));

        link.controlChannel.onmessage = (ev: MessageEvent<ArrayBuffer>) => {
            this.decoder.push(new Uint8Array(ev.data));
            try {
                for (let m = this.decoder.next(); m; m = this.decoder.next()) {
                    this.inbox.push(m);
                }
            } catch (err) {
                this.inbox.fail(protocolError(messageOf(err)));
            }
        };
        link.dataChannel.onmessage = (ev: MessageEvent<ArrayBuffer>) => {
            if (ev.data.byteLength > 0) this.inbox.push(new Uint8Array(ev.data));
        };
    }

    send(message: Out | Common): void {
        this.sendFrame(encodeFrame(message, this.role));
    }

    /** Frames go out in message-sized slices; the peer reassembles them. */
    sendFrame(frame: Bytes): void {
        for (let offset = 0; offset < frame.length; offset += MAX_MESSAGE_BYTES) {
            this.link.controlChannel.send(frame.subarray(offset, offset + MAX_MESSAGE_BYTES));
        }
    }

    /** The next message or data chunk. */
    receive(timeoutMs?: number, what?: string): Promise<Incoming<In>> {
        return this.inbox.next(timeoutMs, what);
    }

    /** The next message; data is not allowed here. */
    async next(timeoutMs?: number, what?: string): Promise<In> {
        return message(await this.inbox.next(timeoutMs, what));
    }

    /** A data chunk that has already arrived, if it is next. */
    pollData(): Uint8Array | undefined {
        return this.inbox.pollIf((item) => item instanceof Uint8Array) as Uint8Array | undefined;
    }

    /** A message that has already arrived, if any. */
    poll(): In | undefined {
        const item = this.inbox.poll();
        return item === undefined ? undefined : message(item);
    }

    /** Gives the peer a moment to read our last message, until it hangs up or says its last word. */
    async goodbye(): Promise<void> {
        const lastWord = async () => {
            for (;;) {
                const item = await this.inbox.next();
                if (!(item instanceof Uint8Array) && FINAL.has(item.type)) return;
            }
        };
        const hungUp = this.link.disconnected;
        await Promise.race([
            // A broken or aborted inbox has nothing more to say; wait for the hang-up.
            lastWord().catch(() => hungUp),
            hungUp,
            new Promise((resolve) => unref(setTimeout(resolve, GOODBYE_TIMEOUT_MS))),
        ]).catch(() => {});
    }

    /** Runs `flow`; if it fails, tells the peer why, if it should know. */
    async explained<T>(flow: () => Promise<T>): Promise<T> {
        try {
            return await flow();
        } catch (err) {
            const last = this.lastWord(err);
            if (last && !this.gone) {
                try {
                    this.send(last);
                    await this.goodbye();
                } catch {
                    // The channel is gone too.
                }
            }
            throw err;
        }
    }

    private lastWord(err: unknown): Common | undefined {
        if (this.abort.aborted) return { type: "cancel" };
        if (err instanceof TransferFailure) {
            return err.report && { type: "error", ...err.report };
        }
        // A local failure, such as a file that cannot be read or written.
        return { type: "error", code: "io", message: messageOf(err) };
    }
}

export type SenderPeer = Peer<SenderMessage, ReceiverMessage>;
export type ReceiverPeer = Peer<ReceiverMessage, SenderMessage>;

const FINAL = new Set(["done", "cancel", "error"]);

function message<In>(item: Incoming<In>): In {
    if (item instanceof Uint8Array) {
        throw protocolError("data arrived outside of a transfer");
    }
    return item;
}

/** The `transferring` event for `offer`, once `files` are accepted. */
export function transferring(offer: Offer, entries: Entry[], files: AcceptedFile[]): TransferEvent {
    const present = presentBytes(offer, entries, files);
    return {
        type: "transferring",
        entries: offer.entries.map((node, i) => ({
            name: node.name,
            sizeBytes: nodeSize(node),
            skipBytes: present[i],
            isDir: node.kind === "dir",
        })),
    };
}

/** Browsers return a number; Node returns a Timeout that would otherwise hold the test runner open. */
export function unref<T>(timer: T): T {
    (timer as { unref?: () => void }).unref?.();
    return timer;
}
