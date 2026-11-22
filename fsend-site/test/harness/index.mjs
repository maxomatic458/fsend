import { readFileSync } from "node:fs";
import { installWebRtc, resetWebRtc } from "./webrtc.mjs";
import { installRelay, resetRelay } from "./relay.mjs";
import { installDom, resetDom, getDownloads } from "./dom.mjs";
import { installWorker } from "./worker.mjs";
import { MemoryDirectoryHandle } from "./fs-access.mjs";
import { unzip } from "./zip.mjs";

export { MemoryDirectoryHandle, getDownloads, unzip };

// The app loads its wasm with fetch(); hand it the bytes up front instead.
const { initSync } = await import("../.build/app.mjs");
initSync({
    module: readFileSync(
        new URL("../../src/lib/proto/pkg/fsend_proto_wasm_bg.wasm", import.meta.url),
    ),
});

let peerConnections = [];

export function installBrowserEnv({ latencyMs = 0, instantDrain = false } = {}) {
    installDom(globalThis);
    installRelay(globalThis);
    installWorker(globalThis);
    peerConnections = installWebRtc(globalThis, { latencyMs, instantDrain });
    return peerConnections;
}

export function resetBrowserEnv() {
    resetWebRtc();
    resetRelay();
    resetDom();
    peerConnections.length = 0;
}

/** Deterministic pseudo-random bytes, so failures are reproducible. */
export function bytes(n, seed = 1) {
    const out = new Uint8Array(n);
    let x = seed >>> 0 || 1;
    for (let i = 0; i < n; i++) {
        x ^= x << 13;
        x ^= x >>> 17;
        x ^= x << 5;
        out[i] = x & 0xff;
    }
    return out;
}

/**
 * Build SelectedEntry[] from a spec.
 *
 *   { "a.txt": bytes(10), "dir": { "b.bin": bytes(20) } }
 *
 * `mode: "file"` produces plain File entries (what a drag-and-drop or an
 * <input> yields); `mode: "handle"` produces File System Access handles, which
 * is the path a Chromium picker takes.
 */
export function makeEntries(spec, mode = "file") {
    const entries = [];
    for (const [name, value] of Object.entries(spec)) {
        if (value instanceof Uint8Array) {
            const file = new File([value], name);
            entries.push(
                mode === "handle"
                    ? { kind: "file", name, handle: fileHandleFor(name, value) }
                    : { kind: "file", name, file },
            );
        } else {
            if (mode === "handle") {
                entries.push({
                    kind: "directory",
                    name,
                    handle: dirHandleFor(name, value),
                });
            } else {
                entries.push({
                    kind: "directory",
                    name,
                    files: flattenSpec(value).map(([relativePath, data]) => ({
                        relativePath,
                        file: new File([data], relativePath.split("/").pop()),
                    })),
                });
            }
        }
    }
    return entries;
}

function flattenSpec(spec, prefix = "") {
    const out = [];
    for (const [name, value] of Object.entries(spec)) {
        const path = prefix ? `${prefix}/${name}` : name;
        if (value instanceof Uint8Array) out.push([path, value]);
        else out.push(...flattenSpec(value, path));
    }
    return out;
}

function fileHandleFor(name, data) {
    return {
        kind: "file",
        name,
        async getFile() {
            return new File([data], name);
        },
    };
}

function dirHandleFor(name, spec) {
    const children = new Map();
    for (const [child, value] of Object.entries(spec)) {
        children.set(
            child,
            value instanceof Uint8Array ? fileHandleFor(child, value) : dirHandleFor(child, value),
        );
    }
    return {
        kind: "directory",
        name,
        async *entries() {
            for (const [n, h] of children) yield [n, h];
        },
    };
}

/** Flatten a spec to { path: bytes } for comparison against a snapshot. */
export function expectedSnapshot(spec, prefix = "") {
    const out = {};
    for (const [name, value] of Object.entries(spec)) {
        const path = prefix ? `${prefix}/${name}` : name;
        if (value instanceof Uint8Array) out[path] = value;
        else Object.assign(out, expectedSnapshot(value, path));
    }
    return out;
}

/**
 * Records every TransferEvent, and answers an offer so a scenario that does
 * not care about the decision just proceeds.
 */
export function makeRecorder({ onOffered } = {}) {
    const record = {
        code: null,
        states: [],
        /// When each kind of event first arrived.
        at: {},
        progress: 0,
        /// The largest progress step.
        largestStep: 0,
        complete: false,
        error: null,
        offered: null,
        entries: null,
        /// Bytes present per entry by the end.
        present: null,
        connectionType: null,
        conflicts: null,
        /// The last { done, total } of checksumming before sending, and of checking before accepting.
        hashing: null,
        verifying: null,
    };

    const emit = (event) => {
        record.states.push(event.type);
        record.at[event.type] ??= Date.now();
        switch (event.type) {
            case "code":
                record.code = event.code;
                break;
            case "offered":
                record.offered = event.offer;
                if (onOffered) onOffered(event);
                else event.accept();
                break;
            case "transferring":
                record.entries = event.entries;
                record.present = event.entries.map((entry) => entry.skipBytes);
                break;
            case "progress":
                record.progress += event.bytes;
                record.largestStep = Math.max(record.largestStep, event.bytes);
                record.present[event.entry] += event.bytes;
                break;
            case "hashing":
            case "verifying":
                record[event.type] = { done: event.done, total: event.total };
                break;
            case "connectionType":
                record.connectionType = event.kind;
                break;
            case "conflicts":
                record.conflicts = { replaced: event.replaced, skipped: event.skipped };
                break;
            case "complete":
                record.complete = true;
                break;
            case "error":
                record.error = event.message;
                break;
        }
    };

    return { record, emit };
}

export function waitFor(predicate, { timeout = 5000, label = "condition" } = {}) {
    return new Promise((resolve, reject) => {
        const started = Date.now();
        const tick = () => {
            let value;
            try {
                value = predicate();
            } catch (e) {
                return reject(e);
            }
            if (value) return resolve(value);
            if (Date.now() - started > timeout) {
                return reject(new Error(`timed out waiting for ${label}`));
            }
            setTimeout(tick, 2);
        };
        tick();
    });
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Runs `body` with what `make` returns, inside a reactive root disposed afterwards. */
export async function inRoot(createRoot, make, body) {
    let dispose;
    const value = createRoot((d) => {
        dispose = d;
        return make();
    });
    try {
        await body(value);
    } finally {
        dispose();
    }
}

/** A control frame of `json`, valid or not. */
export function jsonFrame(json) {
    const payload = new TextEncoder().encode(json);
    const frame = new Uint8Array(5 + payload.length);
    new DataView(frame.buffer).setUint32(0, payload.length + 1);
    frame.set(payload, 5);
    return frame;
}
