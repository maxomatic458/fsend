import { codecState, serve } from "../.build/app.mjs";

/**
 * The codec worker, run in this process. Messages cross by structured clone, so
 * the buffers a request hands over are emptied on the sending side, as in a browser.
 */
class FakeWorker {
    onmessage = null;
    onerror = null;
    #state = codecState();
    #closed = false;

    constructor() {
        // Ready for requests.
        this.#reply(() => null);
    }

    postMessage(data, transfer = []) {
        const request = structuredClone(data, { transfer });
        this.#reply(() => serve(request, this.#state));
    }

    terminate() {
        this.#closed = true;
    }

    #reply(respond) {
        setImmediate(() => {
            if (!this.#closed) this.onmessage?.({ data: structuredClone(respond()) });
        });
    }
}

export function installWorker(globals) {
    globals.Worker = FakeWorker;
}
