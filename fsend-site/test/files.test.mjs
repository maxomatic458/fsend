import { test, describe } from "node:test";
import assert from "node:assert/strict";

const { isSafeName, candidateFiles, retainFiles, prefixHash } =
  await import("./.build/app.mjs");

const file = (name, skip) => ({ type: "File", name, skip });
const dir = (name, files) => ({ type: "Dir", name, files });

describe("offered names", () => {
  test("only plain file names are safe", () => {
    for (const name of ["a.txt", ".hidden", "with space", "ü — ✓", "..."]) {
      assert.ok(isSafeName(name), name);
    }
    for (const name of [
      "",
      ".",
      "..",
      "../x",
      "a/b",
      "a\\b",
      "/etc/passwd",
      "line\nbreak",
      "\u001b[2Jclear",
      "invoice‮fdp.exe",
      "⁦isolate",
    ]) {
      assert.ok(!isSafeName(name), JSON.stringify(name));
    }
  });
});

describe("resume candidates", () => {
  const candidates = [
    file("top.bin", 4),
    null,
    dir("d", [file("a", 1), dir("e", [file("b", 2)]), file("..", 3)]),
  ];

  test("files are walked in tree order", () => {
    assert.deepEqual(
      candidateFiles(candidates, (_, name) => name),
      [
        { path: "top.bin", skip: 4 },
        { path: "d/a", skip: 1 },
        { path: "d/e/b", skip: 2 },
        { path: null, skip: 3 },
      ],
    );
    assert.deepEqual(
      candidateFiles(candidates, (i) => (i === 0 ? "top.bin" : null)),
      [
        { path: "top.bin", skip: 4 },
        { path: null, skip: 1 },
        { path: null, skip: 2 },
        { path: null, skip: 3 },
      ],
    );
  });

  test("files are kept or dropped by their flag", () => {
    assert.deepEqual(retainFiles(candidates, [false, true, false, true]), [
      null,
      null,
      dir("d", [file("a", 1), file("..", 3)]),
    ]);
    assert.deepEqual(retainFiles(candidates, []), [null, null, null]);
  });
});

describe("prefix hashes", () => {
  // Shared with the CLI's test suite, which must produce the same hashes.
  const vectors = [
    [0, "2d06800538d394c2"],
    [1000, "33ef703fb2b20ed1"],
    [300_000, "5d12941f4cf541c5"],
  ];
  const input = Uint8Array.from({ length: 300_000 }, (_, i) => i % 251);

  test("match the CLI's", async () => {
    const blob = new Blob([input]);
    for (const [length, expected] of vectors) {
      assert.equal(await prefixHash(blob, length), expected, `${length}`);
    }
  });

  test("need the whole prefix", async () => {
    assert.equal(await prefixHash(new Blob([input]), 300_001), null);
  });
});
