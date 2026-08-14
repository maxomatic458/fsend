import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { FakePeerConnection, FakeDataChannel } from "./harness/webrtc.mjs";
import { sleep } from "./harness/index.mjs";

const { watchDisconnect } = await import("./.build/app.mjs");

/**
 * Unit coverage for the disconnect detector. A browser makes these states
 * awkward to produce on demand, so they are driven directly here.
 */
describe("watchDisconnect", () => {
  function setup() {
    const pc = new FakePeerConnection();
    pc.connectionState = "connected";
    pc.iceConnectionState = "connected";
    const ch = new FakeDataChannel("data", 0);
    ch.readyState = "open";
    const peer = watchDisconnect(pc, [ch]);
    return { pc, ch, peer };
  }

  function setState(pc, { connection, ice }) {
    if (connection !== undefined) pc.connectionState = connection;
    if (ice !== undefined) pc.iceConnectionState = ice;
    pc._emit("connectionstatechange", {});
    pc._emit("iceconnectionstatechange", {});
  }

  test("a graceful channel close is reported", async () => {
    const { ch, peer } = setup();
    ch.close();
    await sleep(5);
    assert.equal(peer.isDown(), true);
  });

  test("connectionState=failed is reported immediately", async () => {
    const { pc, peer } = setup();
    setState(pc, { connection: "failed" });
    await sleep(5);
    assert.equal(peer.isDown(), true);
  });

  test("iceConnectionState=failed is reported immediately", async () => {
    const { pc, peer } = setup();
    setState(pc, { ice: "failed" });
    await sleep(5);
    assert.equal(peer.isDown(), true);
  });

  test("a blip that recovers inside the grace window is not reported", async () => {
    const { pc, peer } = setup();
    setState(pc, { connection: "disconnected" });
    await sleep(200);
    assert.equal(peer.isDown(), false, "must not fire during the grace period");
    setState(pc, { connection: "connected" });
    await sleep(200);
    assert.equal(peer.isDown(), false, "recovery must cancel the pending fail");
  });

  test("a stale 'connected' on the other transport does not mask a drop", async () => {
    // Regression: an earlier version treated the peer as healthy if *either*
    // state still read "connected", so a real drop went unnoticed.
    const { pc, peer } = setup();
    pc.iceConnectionState = "connected";
    setState(pc, { connection: "disconnected" });
    await sleep(3200);
    assert.equal(peer.isDown(), true, "should fire once the grace expires");
  });

  test("stop() silences the watcher for a normal teardown", async () => {
    const { pc, ch, peer } = setup();
    peer.stop();
    setState(pc, { connection: "failed" });
    ch.close();
    await sleep(20);
    assert.equal(peer.isDown(), false);
  });

  test("the promise rejects rather than resolving", async () => {
    const { pc, peer } = setup();
    setState(pc, { connection: "failed" });
    await assert.rejects(() => peer.promise, /Peer disconnected/);
  });
});
