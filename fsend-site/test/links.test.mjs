import { test, describe, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";

import {
  installBrowserEnv,
  resetBrowserEnv,
  makeEntries,
  makeRecorder,
  bytes,
  waitFor,
} from "./harness/index.mjs";

const { createReceiveSession, createRoot, runSend } =
  await import("./.build/app.mjs");

beforeEach(() => installBrowserEnv({ latencyMs: 0 }));
afterEach(() => resetBrowserEnv());

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function withSession(body) {
  let dispose;
  const session = createRoot((d) => {
    dispose = d;
    return createReceiveSession();
  });
  try {
    await body(session);
  } finally {
    dispose();
  }
}

/** A sender of one small file; returns its code and recorder. */
async function sender() {
  const send = makeRecorder();
  const done = runSend(
    makeEntries({ "a.bin": bytes(5000, 201) }),
    send.emit,
    new AbortController().signal,
  );
  const code = await waitFor(() => send.record.code, { label: "share code" });
  return { code, record: send.record, done };
}

/** Opens `code` as a share link and connects, up to the offer. */
async function untilOffered(session, code) {
  session.openLink(code);
  session.start();
  await waitFor(() => session.state() === "offered", { label: "the offer" });
}

describe("share links on an open receive page", () => {
  test("a link fills in its code", async () => {
    await withSession(async (session) => {
      session.openLink("K7Q29XMV4P");
      assert.equal(session.code(), "K7Q29XMV4P");
      assert.equal(session.state(), "input");
    });
  });

  test("a link after a finished transfer starts over", async () => {
    await withSession(async (session) => {
      const first = await sender();
      await untilOffered(session, first.code);
      await session.acceptOffer();
      await waitFor(() => session.state() === "completed", {
        label: "the transfer",
      });
      await first.done;

      session.openLink("K7Q29XMV4P");
      assert.equal(session.state(), "input");
      assert.equal(session.code(), "K7Q29XMV4P");
      assert.equal(session.pendingLink(), null);
    });
  });

  test("a link after a failed transfer starts over", async () => {
    await withSession(async (session) => {
      session.openLink("ZZZZ000000");
      session.start();
      await waitFor(() => session.state() === "error", { label: "the error" });

      session.openLink("K7Q29XMV4P");
      assert.equal(session.state(), "input");
      assert.equal(session.error(), "");
      assert.equal(session.code(), "K7Q29XMV4P");
    });
  });

  test("a link during a transfer asks first, and can be ignored", async () => {
    await withSession(async (session) => {
      const first = await sender();
      await untilOffered(session, first.code);

      session.openLink("K7Q29XMV4P");
      assert.equal(session.pendingLink(), "K7Q29XMV4P");
      assert.equal(session.state(), "offered");
      assert.equal(session.code(), first.code);

      session.keepTransfer();
      assert.equal(session.pendingLink(), null);
      await session.acceptOffer();
      await waitFor(() => session.state() === "completed", {
        label: "the transfer",
      });
      await first.done;
      assert.equal(first.record.error, null);
    });
  });

  test("switching to the new link cancels the transfer", async () => {
    await withSession(async (session) => {
      const first = await sender();
      await untilOffered(session, first.code);

      session.openLink("K7Q29XMV4P");
      session.switchToLink();
      assert.equal(session.state(), "input");
      assert.equal(session.code(), "K7Q29XMV4P");

      await first.done;
      assert.match(first.record.error ?? "", /cancelled/);
      // The cancelled attempt says nothing more.
      await sleep(50);
      assert.equal(session.state(), "input");
      assert.equal(session.error(), "");
    });
  });
});
