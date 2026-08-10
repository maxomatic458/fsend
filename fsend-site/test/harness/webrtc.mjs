/**
 * Loopback RTCPeerConnection / RTCDataChannel.
 *
 * Close enough to the real thing for the transfer code to be unable to tell:
 * messages are copied (never shared memory), delivered asynchronously and in
 * order, `bufferedAmount` rises and falls with the queue, and the connection
 * exposes the same state machine. It also lets a test do what a browser makes
 * hard on demand — drop a peer abruptly, or close one gracefully.
 */

/** Offers waiting to be answered, keyed by the token embedded in the SDP. */
const pendingOffers = new Map();
let nextId = 0;

class FakeEventTarget {
  constructor() {
    this._listeners = new Map();
  }
  addEventListener(type, fn) {
    if (!this._listeners.has(type)) this._listeners.set(type, new Set());
    this._listeners.get(type).add(fn);
  }
  removeEventListener(type, fn) {
    this._listeners.get(type)?.delete(fn);
  }
  _emit(type, event = {}) {
    for (const fn of this._listeners.get(type) ?? []) fn(event);
    const prop = this[`on${type}`];
    if (typeof prop === "function") prop.call(this, event);
  }
}

function toArrayBuffer(data) {
  // Real transport serialises; never hand the peer a view of our memory.
  if (data instanceof ArrayBuffer) return data.slice(0);
  if (ArrayBuffer.isView(data)) {
    return data.buffer.slice(
      data.byteOffset,
      data.byteOffset + data.byteLength,
    );
  }
  return data;
}

export class FakeDataChannel extends FakeEventTarget {
  constructor(label, latencyMs, instantDrain = false) {
    super();
    // When true, bufferedAmount stays at 0: a link fast enough that the
    // sender never waits on backpressure.
    this._instantDrain = instantDrain;
    this.label = label;
    this.readyState = "connecting";
    this.binaryType = "blob";
    this.bufferedAmount = 0;
    this.bufferedAmountLowThreshold = 0;
    this._peer = null;
    this._latency = latencyMs;
    this._queue = Promise.resolve();
  }

  send(data) {
    if (this.readyState !== "open") {
      // Matches what a real channel throws once it has closed.
      const err = new Error("InvalidStateError");
      err.name = "InvalidStateError";
      throw err;
    }
    const buf = toArrayBuffer(data);
    const size = this._instantDrain ? 0 : (buf.byteLength ?? 0);
    this.bufferedAmount += size;

    // Serialise delivery so ordering matches a real ordered channel.
    this._queue = this._queue.then(
      () =>
        new Promise((resolve) => {
          setTimeout(() => {
            this.bufferedAmount -= size;
            if (
              this.bufferedAmount <= this.bufferedAmountLowThreshold &&
              this.readyState === "open"
            ) {
              this._emit("bufferedamountlow", {});
            }
            const peer = this._peer;
            if (peer && peer.readyState === "open") {
              peer._emit("message", { data: buf });
            }
            resolve();
          }, this._latency);
        }),
    );
  }

  close() {
    if (this.readyState === "closed") return;
    this.readyState = "closed";
    this._emit("close", {});
    const peer = this._peer;
    if (peer && peer.readyState !== "closed") {
      peer.readyState = "closed";
      peer._emit("close", {});
    }
  }

  /** Close only this end, as if the tab holding it went away mid-flight. */
  _closeSilently() {
    this.readyState = "closed";
  }
}

export class FakePeerConnection extends FakeEventTarget {
  constructor(config = {}, latencyMs = 0, instantDrain = false) {
    super();
    this.config = config;
    this._instantDrain = instantDrain;
    this.connectionState = "new";
    this.iceConnectionState = "new";
    this.iceGatheringState = "new";
    this.signalingState = "stable";
    this.localDescription = null;
    this.remoteDescription = null;
    this._channels = new Map();
    this._remote = null;
    this._latency = latencyMs;
    this._id = `pc${nextId++}`;
  }

  createDataChannel(label) {
    const ch = new FakeDataChannel(label, this._latency, this._instantDrain);
    this._channels.set(label, ch);
    return ch;
  }

  async createOffer() {
    return { type: "offer", sdp: `fake-offer:${this._id}` };
  }

  async createAnswer() {
    return { type: "answer", sdp: `fake-answer:${this._id}` };
  }

  async setLocalDescription(desc) {
    this.localDescription = desc;
    if (desc.type === "offer") pendingOffers.set(this._id, this);
    // Gathering finishes a tick later, as it would with a real ICE agent.
    this.iceGatheringState = "gathering";
    setTimeout(() => {
      this.iceGatheringState = "complete";
      this._emit("icegatheringstatechange", {});
    }, 0);
  }

  async setRemoteDescription(desc) {
    this.remoteDescription = desc;
    const [kind, id] = String(desc.sdp).split(":");
    if (kind === "fake-offer") {
      const offerer = pendingOffers.get(id);
      if (!offerer) throw new Error(`no pending offer ${id}`);
      this._link(offerer);
    }
  }

  /** Wire the two ends together and mirror the offerer's channels. */
  _link(offerer) {
    this._remote = offerer;
    offerer._remote = this;

    for (const [label, ch] of offerer._channels) {
      const mirror = new FakeDataChannel(
        label,
        this._latency,
        this._instantDrain,
      );
      mirror._peer = ch;
      ch._peer = mirror;
      this._channels.set(label, mirror);
    }

    setTimeout(() => {
      for (const pc of [offerer, this]) {
        pc.connectionState = "connected";
        pc.iceConnectionState = "connected";
      }
      // The answerer learns about the channels via ondatachannel.
      for (const [, mirror] of this._channels) {
        mirror.readyState = "open";
        this._emit("datachannel", { channel: mirror });
      }
      for (const [, ch] of offerer._channels) {
        ch.readyState = "open";
        ch._emit("open", {});
      }
      for (const [, mirror] of this._channels) mirror._emit("open", {});
      for (const pc of [offerer, this]) pc._emit("connectionstatechange", {});
      for (const pc of [offerer, this])
        pc._emit("iceconnectionstatechange", {});
    }, this._latency);
  }

  async getStats() {
    // Enough for getConnectionType() to report a direct connection.
    const stats = new Map();
    stats.set("pair", {
      type: "candidate-pair",
      nominated: true,
      localCandidateId: "local",
      remoteCandidateId: "remote",
    });
    stats.set("local", { type: "local-candidate", candidateType: "host" });
    stats.set("remote", { type: "remote-candidate", candidateType: "host" });
    return stats;
  }

  close() {
    if (this.connectionState === "closed") return;
    this.connectionState = "closed";
    this.iceConnectionState = "closed";
    for (const [, ch] of this._channels) ch.close();
    this._emit("connectionstatechange", {});
  }

  /**
   * The peer's tab vanished: no graceful channel close reaches us, only the
   * connection state changes. This is the case that used to let a sender keep
   * streaming into nothing.
   */
  simulateAbruptDisconnect() {
    for (const [, ch] of this._channels) ch._closeSilently();
    const remote = this._remote;
    if (remote) {
      remote.connectionState = "failed";
      remote.iceConnectionState = "failed";
      remote._emit("connectionstatechange", {});
      remote._emit("iceconnectionstatechange", {});
    }
  }
}

export class FakeRTCSessionDescription {
  constructor(init) {
    this.type = init.type;
    this.sdp = init.sdp;
  }
}

export function installWebRtc(
  globals,
  { latencyMs = 0, instantDrain = false } = {},
) {
  globals.RTCPeerConnection = class extends FakePeerConnection {
    constructor(config) {
      super(config, latencyMs, instantDrain);
      created.push(this);
    }
  };
  globals.RTCSessionDescription = FakeRTCSessionDescription;
  const created = [];
  globals.__peerConnections = created;
  return created;
}

export function resetWebRtc() {
  pendingOffers.clear();
}
