import { test } from "node:test";
import assert from "node:assert/strict";

const { formatBytes, formatTime } = await import("./.build/app.mjs");

test("sizes move to the next unit once they round up to it", () => {
    assert.equal(formatBytes(0), "0 B");
    assert.equal(formatBytes(1023), "1023 B");
    assert.equal(formatBytes(1024), "1.0 KB");
    assert.equal(formatBytes(1048575), "1.0 MB");
    assert.equal(formatBytes(1048576 * 1.5), "1.5 MB");
    assert.equal(formatBytes(2 ** 30 - 1), "1.00 GB");
    assert.equal(formatBytes(5 * 2 ** 40), "5120.00 GB");
});

test("times carry over once they round up", () => {
    assert.equal(formatTime(0.4), "< 1s");
    assert.equal(formatTime(59.4), "59s");
    assert.equal(formatTime(59.6), "1m 0s");
    assert.equal(formatTime(119.6), "2m 0s");
    assert.equal(formatTime(3599.7), "1h 0m");
    assert.equal(formatTime(7199), "2h 0m");
    assert.equal(formatTime(3725), "1h 2m");
});
