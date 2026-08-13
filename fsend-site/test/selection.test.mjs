import { test, describe } from "node:test";
import assert from "node:assert/strict";

const { createSendSession, createRoot } = await import("./.build/app.mjs");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * A directory handle whose files resolve slowly, standing in for a real
 * File System Access walk: `buildFileTree` awaits `getFile()` once per file,
 * so a big folder takes far more ticks than a plain File entry, which takes
 * none at all.
 */
function slowFolder(name, fileCount, delayMs) {
  const children = new Map();
  for (let i = 0; i < fileCount; i++) {
    children.set(`f${i}.bin`, {
      kind: "file",
      name: `f${i}.bin`,
      async getFile() {
        await sleep(delayMs);
        return { size: 1000 };
      },
    });
  }
  return {
    kind: "directory",
    name,
    handle: {
      kind: "directory",
      name,
      async *entries() {
        for (const [n, h] of children) yield [n, h];
      },
    },
  };
}

/** A plain File entry — `buildFileTree` reads `.size` with no await at all. */
function fastFile(name, size) {
  return { kind: "file", name, file: { size } };
}

/** Every row's name paired with the size shown next to it. */
const labelled = (session) =>
  session.items().map((item) => [item.entry.name, item.size]);

/** Runs `body` inside a reactive root and disposes it afterwards. */
async function withSession(body) {
  let dispose;
  const session = createRoot((d) => {
    dispose = d;
    return createSendSession();
  });
  try {
    await body(session);
  } finally {
    dispose();
  }
}

describe("selection sizing", () => {
  test("a slow tree walk cannot mislabel a newer selection", async () => {
    await withSession(async (session) => {
      // A folder that takes many ticks to measure, then immediately a plain
      // file that takes none — the second measurement resolves first.
      session.add([slowFolder("big", 12, 4)]);
      session.add([fastFile("quick.txt", 500)]);

      await sleep(400);

      assert.deepEqual(
        labelled(session),
        [
          ["big", 12 * 1000],
          ["quick.txt", 500],
        ],
        "each row must carry its own size, whatever order they finished in",
      );
      assert.equal(
        session.selectionSize(),
        12 * 1000 + 500,
        "total must include both the folder and the file",
      );
    });
  });

  test("a removal is not undone by an in-flight measurement", async () => {
    await withSession(async (session) => {
      session.add([slowFolder("big", 12, 4), fastFile("quick.txt", 500)]);
      // Remove the folder while its walk is still running.
      session.add([fastFile("second.txt", 700)]);
      session.remove(0);

      await sleep(400);
      assert.deepEqual(
        labelled(session),
        [
          ["quick.txt", 500],
          ["second.txt", 700],
        ],
        "the removed folder must not come back when its walk lands",
      );
      assert.equal(
        session.selectionSize(),
        500 + 700,
        "total must reflect the removal, not the pre-removal selection",
      );
    });
  });

  test("reset is not repopulated by an in-flight measurement", async () => {
    await withSession(async (session) => {
      session.add([slowFolder("big", 12, 4)]);
      session.reset();

      await sleep(400);
      assert.deepEqual(session.items(), [], "the selection must stay cleared");
      assert.equal(session.selectionSize(), 0, "total must stay cleared");
    });
  });

  test("a size is shown as pending until its walk finishes", async () => {
    await withSession(async (session) => {
      session.add([slowFolder("big", 12, 20)]);
      assert.deepEqual(
        labelled(session),
        [["big", null]],
        "the row appears immediately, with no size yet",
      );
      assert.equal(
        session.selectionSize(),
        0,
        "an unmeasured item contributes nothing to the total",
      );

      await sleep(600);
      assert.deepEqual(labelled(session), [["big", 12 * 1000]]);
    });
  });
});
