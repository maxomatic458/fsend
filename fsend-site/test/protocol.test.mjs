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
  filesToCheck,
  fingerprint,
  fingerprintRanges,
  parseServerMessage,
  planReceive,
  presentBytes,
  topEntries,
  validateOffer,
} = await import("./.build/app.mjs");

const offer = {
  type: "offer",
  entries: Array.from({ length: 500 }, (_, i) => ({
    kind: "file",
    path: [`file-${i}.bin`],
    size: i,
  })),
};

describe("control frames", () => {
  test("messages round-trip through frames split anywhere", () => {
    const frames = [
      encodeFrame({ type: "cancel" }, Role.Sender),
      encodeFrame(offer, Role.Sender),
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
    assert.deepEqual(got, [{ type: "cancel" }, offer]);
  });

  test("each side sends only its own messages", () => {
    assert.throws(() => encodeFrame(offer, Role.Receiver), /unknown variant/);
    assert.throws(
      () => encodeFrame({ type: "done" }, Role.Sender),
      /unknown variant/,
    );
    assert.ok(encodeFrame({ type: "done" }, Role.Receiver));
  });

  for (const [name, frame] of [
    ["a message the other side sends", encodeFrame(offer, Role.Sender)],
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
  const noise = Uint8Array.from(
    { length: 50_000 },
    (_, i) => ((i * 7919) % 251) ^ (i >> 3),
  );

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
    assert.throws(
      () => encodeBlock(new Uint8Array(1024 * 1024 + 1)),
      /a block holds/,
    );
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
    assert.deepEqual(
      parseServerMessage('{"type":"paired","transport":"webrtc","extra":1}'),
      { type: "paired", transport: "webrtc" },
    );
  });

  for (const [name, text] of [
    ["an unknown message", '{"type":"motd","text":"hi"}'],
    ["an unknown error code", '{"type":"error","code":"teapot","message":""}'],
    ["an unknown transport", '{"type":"paired","transport":"quic"}'],
    [
      "a client message",
      '{"type":"join","version":1,"nameplate":"K7Q2","transports":[]}',
    ],
    ["malformed JSON", "{"],
  ]) {
    test(`${name} is an error`, () => {
      assert.throws(() => parseServerMessage(text));
    });
  }
});

describe("offer helpers", () => {
  const entries = [
    { kind: "file", path: ["a.txt"], size: 10 },
    { kind: "dir", path: ["d"] },
    { kind: "file", path: ["d", "b"], size: 5 },
    { kind: "file", path: ["d", "c"], size: 7 },
  ];

  test("offers are checked against the rules", () => {
    validateOffer(entries);
    assert.throws(
      () => validateOffer([{ kind: "file", path: [".."], size: 1 }]),
      /not a valid name/,
    );
    assert.throws(
      () => validateOffer([{ kind: "file", path: ["d", "a"], size: 1 }]),
      /parent/,
    );
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
    assert.throws(
      () => checkAccept(entries, [{ id: 1, offset: 0 }]),
      /directory/,
    );
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

  test("progress is counted per top-level entry", () => {
    const [tops, topOf] = topEntries(entries);
    assert.deepEqual(tops, [
      { name: "a.txt", dir: false, size: 10 },
      { name: "d", dir: true, size: 12 },
    ]);
    assert.deepEqual(topOf, [0, 1, 1, 1]);
    assert.deepEqual(
      presentBytes(entries, [
        { id: 0, offset: 4 },
        { id: 3, offset: 0 },
      ]),
      [4, 5],
    );
  });
});
