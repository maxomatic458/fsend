import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { jsonFrame } from "./harness/index.mjs";

const {
  encodeFrame,
  FrameDecoder,
  Role,
  acceptFiles,
  checkAccept,
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

  test("the accept resumes, skips and starts over like the CLI", () => {
    const files = acceptFiles(entries, [4, null, 5, 9]);
    assert.deepEqual(files, [
      { id: 0, offset: 4 },
      { id: 3, offset: 0 },
    ]);
    checkAccept(entries, files);
    assert.throws(
      () => checkAccept(entries, [{ id: 1, offset: 0 }]),
      /directory/,
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
