import { BlockDecoder, encodeBlock, type Bytes } from "../proto";

/** What a batch of data stream bytes completed. */
export interface Decoded {
    blocks: Bytes[];
    /** File bytes of the block still arriving, in proportion to how much of it did. */
    partial: number;
    /** Whether no partial block is buffered. */
    empty: boolean;
}

/** The data stream's block codec: a worker where there is one, else this thread. */
export interface BlockCodec {
    /** The block frame of `bytes`, which are handed over and unusable afterwards. */
    encode(bytes: Uint8Array): Promise<Bytes>;
    /** Takes `chunks` of the data stream, in order, handing them over too; throws on a malformed block. */
    decode(chunks: Uint8Array[]): Promise<Decoded>;
    close(): void;
}

export type CodecRequest =
    | { id: number; type: "encode"; bytes: Uint8Array }
    | { id: number; type: "decode"; chunks: Uint8Array[] };

export type CodecResponse =
    { id: number; frame: Bytes } | ({ id: number } & Decoded) | { id: number; error: string };

/** Runs a request against `decoder`; the worker and this thread share it. */
export function serve(request: CodecRequest, decoder: BlockDecoder): CodecResponse {
    try {
        if (request.type === "encode") {
            return { id: request.id, frame: encodeBlock(request.bytes) };
        }
        for (const chunk of request.chunks) decoder.push(chunk);
        const blocks = [];
        for (let block = decoder.next(); block; block = decoder.next()) {
            blocks.push(block);
        }
        return {
            id: request.id,
            blocks,
            partial: decoder.partial(),
            empty: decoder.isEmpty(),
        };
    } catch (err) {
        return {
            id: request.id,
            error: err instanceof Error ? err.message : String(err),
        };
    }
}

/** The codec for one transfer, in a worker unless the browser has none or it fails to start. */
export async function createCodec(): Promise<BlockCodec> {
    if (typeof Worker !== "function") return inThread();
    let worker: Worker;
    try {
        worker = new Worker(new URL("./codec.worker.ts", import.meta.url), {
            type: "module",
        });
    } catch {
        return inThread();
    }
    const ready = await new Promise<boolean>((resolve) => {
        worker.onmessage = () => resolve(true);
        worker.onerror = () => resolve(false);
    });
    if (!ready) {
        worker.terminate();
        return inThread();
    }
    return inWorker(worker);
}

function inThread(): BlockCodec {
    const decoder = new BlockDecoder();
    let id = 0;
    const run = <T>(request: CodecRequest): Promise<T> => {
        const response = serve(request, decoder);
        return "error" in response
            ? Promise.reject(new Error(response.error))
            : Promise.resolve(response as T);
    };
    return {
        encode: (bytes) =>
            run<{ frame: Bytes }>({ id: ++id, type: "encode", bytes }).then((r) => r.frame),
        decode: (chunks) => run<Decoded>({ id: ++id, type: "decode", chunks }),
        close: () => {},
    };
}

function inWorker(worker: Worker): BlockCodec {
    const waiting = new Map<
        number,
        { resolve: (response: CodecResponse) => void; reject: (e: Error) => void }
    >();
    let id = 0;
    // Once set, the worker is gone and every request fails with it.
    let failed: Error | undefined;
    const fail = (error: Error) => {
        failed ??= error;
        for (const { reject } of waiting.values()) reject(failed);
        waiting.clear();
    };
    worker.onmessage = (event: MessageEvent<CodecResponse>) => {
        const response = event.data;
        const pending = waiting.get(response.id);
        waiting.delete(response.id);
        if (!pending) return;
        if ("error" in response) pending.reject(new Error(response.error));
        else pending.resolve(response);
    };
    worker.onerror = () => fail(new Error("The compression worker failed"));

    const run = <T>(request: CodecRequest, transfer: Transferable[]) =>
        new Promise<T>((resolve, reject) => {
            if (failed) return reject(failed);
            waiting.set(request.id, {
                resolve: (response) => resolve(response as T),
                reject,
            });
            worker.postMessage(request, transfer);
        });
    return {
        encode: (bytes) =>
            run<{ frame: Bytes }>({ id: ++id, type: "encode", bytes }, [bytes.buffer]).then(
                (r) => r.frame,
            ),
        decode: (chunks) =>
            run<Decoded>(
                { id: ++id, type: "decode", chunks },
                chunks.map((chunk) => chunk.buffer),
            ),
        close: () => {
            worker.terminate();
            fail(new Error("The compression worker was closed"));
        },
    };
}
