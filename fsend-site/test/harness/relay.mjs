/**
 * In-process stand-in for fsend-relay, speaking the same JSON protocol as
 * src/lib/types.ts, plus a WebSocket shim so RelayClient connects to it
 * unmodified.
 */

const sessions = new Map(); // code -> { sender, receiver, capabilities }
let codeCounter = 0;

function nextCode() {
  // Same shape as the real relay: 8 chars, A-Z0-9.
  const n = (++codeCounter).toString(36).toUpperCase().padStart(4, "0");
  return `TEST${n}`.slice(0, 8);
}

function negotiate(a, b) {
  if (a.includes("iroh") && b.includes("iroh")) return "iroh";
  if (a.includes("web_rtc") && b.includes("web_rtc")) return "web_rtc";
  return null;
}

class FakeWebSocket {
  constructor(url) {
    this.url = url;
    this.readyState = 0;
    this.onopen = null;
    this.onmessage = null;
    this.onerror = null;
    this.onclose = null;
    this._session = null;
    this._role = null;
    setTimeout(() => {
      if (this.readyState !== 0) return;
      this.readyState = 1;
      this.onopen?.({});
    }, 0);
  }

  _deliver(obj) {
    setTimeout(() => {
      if (this.readyState === 1) this.onmessage?.({ data: JSON.stringify(obj) });
    }, 0);
  }

  send(raw) {
    const msg = JSON.parse(raw);

    if (msg.type === "create_session") {
      const code = nextCode();
      sessions.set(code, {
        sender: this,
        receiver: null,
        capabilities: msg.capabilities,
      });
      this._session = code;
      this._role = "sender";
      this._deliver({ type: "create_session", code });
      return;
    }

    if (msg.type === "join_session") {
      const session = sessions.get(msg.code);
      if (!session) {
        this._deliver({ type: "error", message: "unknown session" });
        return;
      }
      const protocol = negotiate(session.capabilities, msg.capabilities);
      if (!protocol) {
        this._deliver({ type: "error", message: "no common protocol" });
        return;
      }
      session.receiver = this;
      this._session = msg.code;
      this._role = "receiver";
      this._deliver({ type: "join_session", protocol });
      session.sender._deliver({ type: "peer_joined", protocol });
      return;
    }

    if (msg.type === "exchange") {
      const session = sessions.get(this._session);
      if (!session) return;
      const peer = this._role === "sender" ? session.receiver : session.sender;
      peer?._deliver({ type: "exchange", connection_info: msg.connection_info });
      return;
    }
  }

  close() {
    this.readyState = 3;
    this.onclose?.({});
  }
}

export function installRelay(globals) {
  globals.WebSocket = FakeWebSocket;
}

export function resetRelay() {
  sessions.clear();
  codeCounter = 0;
}
