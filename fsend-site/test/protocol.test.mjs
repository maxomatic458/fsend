import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { jsonFrame } from "./harness/index.mjs";

const {
    BlockDecoder,
    encodeBlock,
    encodeFrame,
    FrameDecoder,
    Role,
    acceptPlan,
    checkAccept,
    checkLengths,
    byName,
    encodeOffer,
    filesToCheck,
    fingerprint,
    fingerprintRanges,
    flattenOffer,
    Hasher,
    parseServerMessage,
    planReceive,
    presentBytes,
    Progress,
    validateOffer,
} = await import("./.build/app.mjs");

/** A sender's message big enough to be compressed. */
const hashes = {
    type: "hashes",
    hashes: Array.from({ length: 500 }, (_, i) => i.toString(16).padStart(32, "0")),
};

describe("control frames", () => {
    test("messages round-trip through frames split anywhere", () => {
        const frames = [
            encodeFrame({ type: "cancel" }, Role.Sender),
            encodeFrame(hashes, Role.Sender),
        ];
        // Big payloads are compressed (flags bit 0), small ones are not.
        assert.equal(frames[0][4], 0);
        assert.equal(frames[1][4], 1);

        const decoder = new FrameDecoder(Role.Sender);
        const got = [];
        for (const frame of frames) {
            for (let i = 0; i < frame.length; i += 7) {
                decoder.push(frame.subarray(i, i + 7));
                for (let m = decoder.next(); m; m = decoder.next()) got.push(m);
            }
        }
        assert.deepEqual(got, [{ type: "cancel" }, hashes]);
    });

    test("each side sends only its own messages", () => {
        assert.throws(() => encodeFrame(hashes, Role.Receiver), /unknown variant/);
        assert.throws(() => encodeFrame({ type: "done" }, Role.Sender), /unknown variant/);
        assert.ok(encodeFrame({ type: "done" }, Role.Receiver));
    });

    for (const [name, frame] of [
        ["a message the other side sends", encodeFrame(hashes, Role.Sender)],
        ["an unknown message", jsonFrame('{"type":"resume"}')],
        ["a malformed message", jsonFrame('{"type":"ack"}')],
        ["a malformed frame", new Uint8Array([0, 0, 0, 0])],
    ]) {
        test(`${name} is an error`, () => {
            const decoder = new FrameDecoder(Role.Receiver);
            decoder.push(frame);
            assert.throws(() => decoder.next());
        });
    }
});

describe("data blocks", () => {
    const text = new TextEncoder().encode("fsend ".repeat(100_000));
    const noise = Uint8Array.from({ length: 50_000 }, (_, i) => ((i * 7919) % 251) ^ (i >> 3));

    test("compressible blocks shrink, others go raw, and both round-trip", () => {
        const packed = encodeBlock(text);
        assert.equal(packed[4], 1);
        assert.ok(packed.length < text.length / 10);
        const random = crypto.getRandomValues(new Uint8Array(60_000));
        const stored = encodeBlock(random);
        assert.equal(stored[4], 0);
        assert.equal(stored.length, random.length + 5);

        const decoder = new BlockDecoder();
        const got = [];
        const bytes = new Uint8Array([...packed, ...stored, ...encodeBlock(noise)]);
        for (let i = 0; i < bytes.length; i += 1000) {
            decoder.push(bytes.subarray(i, i + 1000));
            for (let b = decoder.next(); b; b = decoder.next()) got.push(b);
        }
        assert.deepEqual(got, [text, random, noise]);
        assert.ok(decoder.isEmpty());
    });

    test("a block holds 1 to MAX_BLOCK_BYTES bytes", () => {
        assert.throws(() => encodeBlock(new Uint8Array(0)), /a block holds/);
        assert.throws(() => encodeBlock(new Uint8Array(1024 * 1024 + 1)), /a block holds/);
    });

    test("a malformed block is an error", () => {
        const block = encodeBlock(text);
        block[4] = 0x7f;
        const decoder = new BlockDecoder();
        decoder.push(block);
        assert.throws(() => decoder.next(), /flags/);
    });
});

describe("relay messages", () => {
    test("a valid message parses", () => {
        assert.deepEqual(parseServerMessage('{"type":"paired","transport":"webrtc","extra":1}'), {
            type: "paired",
            transport: "webrtc",
        });
    });

    for (const [name, text] of [
        ["an unknown message", '{"type":"motd","text":"hi"}'],
        ["an unknown error code", '{"type":"error","code":"teapot","message":""}'],
        ["an unknown transport", '{"type":"paired","transport":"quic"}'],
        ["a client message", '{"type":"join","version":1,"nameplate":"K7Q2","transports":[]}'],
        ["malformed JSON", "{"],
    ]) {
        test(`${name} is an error`, () => {
            assert.throws(() => parseServerMessage(text));
        });
    }
});

describe("offer helpers", () => {
    const offer = {
        entries: [
            { kind: "file", name: "a.txt", size: 10 },
            {
                kind: "dir",
                name: "d",
                entries: [
                    { kind: "file", name: "b", size: 5 },
                    { kind: "file", name: "c", size: 7 },
                ],
            },
        ],
    };
    const entries = [
        { kind: "file", path: ["a.txt"], size: 10 },
        { kind: "dir", path: ["d"] },
        { kind: "file", path: ["d", "b"], size: 5 },
        { kind: "file", path: ["d", "c"], size: 7 },
    ];

    test("an entry's id is its place in the tree, each folder before what it holds", () => {
        assert.deepEqual(flattenOffer(offer), entries);
    });

    test("offers are checked against the rules", () => {
        validateOffer(offer);
        encodeOffer(offer);
        const named = (name) => ({ entries: [{ kind: "file", name, size: 1 }] });
        for (const check of [validateOffer, encodeOffer]) {
            assert.throws(() => check(named("..")), /not a valid name/);
            assert.throws(
                () => check({ entries: [named("x").entries[0], named("x").entries[0]] }),
                /taken in its directory/,
            );
        }
    });

    test("names sort by code point, as the CLI sorts them", () => {
        const names = ["b", "B", "a", "\u{1F600}", "\uFF5E", "ä", ""];
        assert.deepEqual(names.sort(byName), ["", "B", "a", "b", "ä", "\uFF5E", "\u{1F600}"]);
    });

    test("verified prefixes resume and the rest conflict, like the CLI", () => {
        const local = [
            { kind: "file", size: 4 },
            { kind: "missing" },
            { kind: "file", size: 5 },
            { kind: "file", size: 3 },
        ];
        const checked = filesToCheck(entries, local);
        assert.deepEqual(checked, [
            { id: 0, len: 4 },
            { id: 2, len: 5 },
            { id: 3, len: 3 },
        ]);
        checkLengths(entries, checked);
        assert.throws(() => checkLengths(entries, [{ id: 2, len: 6 }]), /past/);

        // a.txt matches, d/c does not, d/b is complete.
        const plan = planReceive(entries, local, [0, 2]);
        assert.deepEqual(plan, {
            files: [{ id: 0, offset: 4 }],
            complete: [2],
            dirs: [1],
            conflicts: [{ id: 3, replaceable: true }],
        });
        assert.deepEqual(acceptPlan(plan, "skip"), plan.files);
        const replaced = acceptPlan(plan, "replace");
        assert.deepEqual(replaced, [
            { id: 0, offset: 4 },
            { id: 3, offset: 0 },
        ]);
        checkAccept(entries, replaced);
        assert.throws(() => checkAccept(entries, [{ id: 1, offset: 0 }]), /directory/);
    });

    test("fingerprints match the CLI's", () => {
        const mib = 1024 * 1024;
        assert.deepEqual(fingerprintRanges(2 * mib), [[0, 2 * mib]]);
        assert.deepEqual(fingerprintRanges(2 * mib + 1), [
            [0, mib],
            [mib + 1, 2 * mib + 1],
        ]);
        assert.equal(
            fingerprint(new TextEncoder().encode("fsend")),
            "e56767a1277b0357a6d715ea3ebead85",
        );
    });

    test("checksums are SHA-256, fed in any pieces", () => {
        const abc = new TextEncoder().encode("abc");
        const whole = new Hasher("sha256");
        whole.update(abc);
        const pieces = new Hasher("sha256");
        for (const byte of abc) pieces.update(Uint8Array.of(byte));
        const digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert.equal(whole.finish(), digest);
        assert.equal(pieces.finish(), digest);
    });

    test("progress is counted per file, and what is there per top-level entry", () => {
        const files = [
            { id: 0, offset: 4 },
            { id: 3, offset: 0 },
        ];
        assert.deepEqual(new Progress(entries, files).advance(10), [
            [0, 6],
            [3, 4],
        ]);
        assert.deepEqual(presentBytes(offer, entries, files), [4, 5]);
    });
});
