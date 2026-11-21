const ALPHABET = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const TRANSPORTS = ["iroh", "webrtc"];
const sessions = new Map(); // nameplate -> { sender, transports }
let counter = 0;

function nextNameplate() {
    // Four symbols of the code alphabet, like the real relay hands out.
    let n = ++counter;
    let nameplate = "";
    for (let i = 0; i < 4; i++, n = Math.floor(n / 32)) {
        nameplate = ALPHABET[n % ALPHABET.length] + nameplate;
    }
    return nameplate;
}

class FakeWebSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSED = 3;

    constructor(url) {
        this.url = url;
        this.readyState = FakeWebSocket.CONNECTING;
        this.onopen = null;
        this.onmessage = null;
        this.onerror = null;
        this.onclose = null;
        this._peer = null;
        this._nameplate = null;
        setTimeout(() => {
            if (this.readyState !== FakeWebSocket.CONNECTING) return;
            if (!url.endsWith("/relay")) return this.close();
            this.readyState = FakeWebSocket.OPEN;
            this.onopen?.({});
        }, 0);
    }

    _deliver(data) {
        setTimeout(() => {
            if (this.readyState === FakeWebSocket.OPEN) this.onmessage?.({ data });
        }, 0);
    }

    _json(message) {
        this._deliver(JSON.stringify(message));
    }

    _fail(code) {
        this._json({ type: "error", code });
        setTimeout(() => this.close(), 0);
    }

    send(frame) {
        if (typeof frame !== "string") {
            if (!this._peer) return this._fail("protocol");
            // A copy, as an ArrayBuffer for binaryType "arraybuffer".
            return this._peer._deliver(new Uint8Array(frame).buffer);
        }
        // Only the first message is a text frame.
        if (this._nameplate || this._peer) return this._fail("protocol");

        const msg = JSON.parse(frame);
        if (msg.version !== 2) return this._fail("version");
        if (!msg.transports?.every((t) => TRANSPORTS.includes(t))) {
            return this._fail("protocol");
        }

        if (msg.type === "create") {
            const nameplate = nextNameplate();
            sessions.set(nameplate, { sender: this, transports: msg.transports });
            this._nameplate = nameplate;
            this._json({ type: "created", nameplate, expires_in: 300 });
            return;
        }

        if (msg.type === "join") {
            const session = sessions.get(msg.nameplate);
            sessions.delete(msg.nameplate);
            if (!session) return this._fail("not_found");
            const transport = session.transports.find((t) => msg.transports.includes(t));
            if (!transport) {
                session.sender._fail("no_common_transport");
                return this._fail("no_common_transport");
            }
            this._peer = session.sender;
            session.sender._peer = this;
            for (const side of [this, session.sender]) {
                side._json({ type: "paired", transport });
            }
            return;
        }

        this._fail("protocol");
    }

    close() {
        if (this.readyState === FakeWebSocket.CLOSED) return;
        this.readyState = FakeWebSocket.CLOSED;
        if (this._nameplate) sessions.delete(this._nameplate);
        const peer = this._peer;
        this._peer = null;
        if (peer?._peer === this) {
            peer._peer = null;
            peer._fail("peer_left");
        }
        this.onclose?.({});
    }
}

export function installRelay(globals) {
    globals.WebSocket = FakeWebSocket;
}

export function resetRelay() {
    sessions.clear();
    counter = 0;
}
