import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

/**
 * Browser <-> fsend-cli wire compatibility.
 *
 * The two implementations share no code, so nothing stops one side from
 * drifting. These tests read the Rust definitions straight from `fsend-cli`
 * and `fsend-relay` and assert the browser speaks the same protocol: same
 * version string, same message tags, same field names, same framing.
 *
 * They are static — no Rust toolchain, no running relay. A rename on either
 * side fails the build.
 */

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../..");
const rust = (p) => readFileSync(path.join(repo, p), "utf8");

const CLI_TRANSFER = rust("fsend-cli/src/transfer.rs");
const CLI_WEBRTC = rust("fsend-cli/src/webrtc.rs");
const CLI_RELAY = rust("fsend-cli/src/relay.rs");
const RELAY_MAIN = rust("fsend-relay/src/main.rs");

const TS_CONFIG = readFileSync(path.join(here, "../src/config.ts"), "utf8");
const TS_TYPES = readFileSync(path.join(here, "../src/lib/types.ts"), "utf8");
const TS_COMPRESSION = readFileSync(
  path.join(here, "../src/lib/transport/compression.ts"),
  "utf8",
);

/** Variant names of a `#[serde(tag = "type")]` enum, in declaration order. */
function rustVariants(source, enumName) {
  const start = source.indexOf(`enum ${enumName} {`);
  assert.notEqual(start, -1, `Rust enum ${enumName} not found`);
  const body = source.slice(
    start + `enum ${enumName} {`.length,
    source.indexOf("\n}", start),
  );
  return [...body.matchAll(/^\s{4}(\w+)\s*[{,(]/gm)].map((m) => m[1]);
}

/** Field names declared on one variant of a Rust enum. */
function rustVariantFields(source, enumName, variant) {
  const start = source.indexOf(`enum ${enumName} {`);
  const body = source.slice(start, source.indexOf("\n}", start));
  const at = body.indexOf(`${variant} {`);
  if (at === -1) return [];
  const braces = body.slice(at + variant.length + 1);
  return [...braces.slice(0, braces.indexOf("}")).matchAll(/(\w+)\s*:/g)].map(
    (m) => m[1],
  );
}

/**
 * `type: "X"` literals from a TypeScript discriminated union.
 *
 * Reads whole lines rather than scanning for the next `;` — union members
 * contain semicolons between their own fields, which truncates the match.
 */
function tsTags(source, typeName) {
  const lines = source.split("\n");
  const start = lines.findIndex((l) =>
    l.startsWith(`export type ${typeName} =`),
  );
  assert.notEqual(start, -1, `TS type ${typeName} not found`);
  const tags = [];
  for (let i = start; i < lines.length; i++) {
    const found = /type:\s*"([^"]+)"/.exec(lines[i]);
    if (found) tags.push(found[1]);
    if (/;\s*$/.test(lines[i]) && i > start) break;
  }
  return tags;
}

function rustConst(source, name) {
  const m = new RegExp(`const ${name}[^=]*=\\s*([^;]+);`).exec(source);
  assert.ok(m, `Rust const ${name} not found`);
  return m[1].trim().replace(/"/g, "");
}

function tsConst(source, name) {
  const m = new RegExp(`export const ${name}\\s*=\\s*([^;]+);`).exec(source);
  assert.ok(m, `TS const ${name} not found`);
  return m[1].trim().replace(/"/g, "");
}

describe("protocol compatibility with fsend-cli", () => {
  test("protocol version strings match", () => {
    assert.equal(
      tsConst(TS_CONFIG, "PROTO_VERSION"),
      rustConst(CLI_TRANSFER, "PROTO_VERSION"),
      "a version mismatch makes every browser<->CLI transfer fail the handshake",
    );
  });

  test("control message tags match in both directions", () => {
    for (const name of ["SenderToReceiver", "ReceiverToSender"]) {
      assert.deepEqual(
        tsTags(TS_TYPES, name).sort(),
        rustVariants(CLI_TRANSFER, name).sort(),
        `${name} variants differ between the browser and the CLI`,
      );
    }
  });

  test("control message field names match", () => {
    const cases = [
      ["SenderToReceiver", "ConnRequest", ["version"]],
      ["SenderToReceiver", "FileInfo", ["files"]],
      ["ReceiverToSender", "WrongVersion", ["expected"]],
      ["ReceiverToSender", "AcceptFilesSkip", ["files"]],
    ];
    for (const [enumName, variant, expected] of cases) {
      assert.deepEqual(
        rustVariantFields(CLI_TRANSFER, enumName, variant),
        expected,
        `${enumName}::${variant} fields changed on the Rust side`,
      );
      for (const field of expected) {
        assert.match(
          TS_TYPES,
          new RegExp(`type: "${variant}"[^}]*${field}`),
          `TS ${variant} is missing the "${field}" field`,
        );
      }
    }
  });

  test("file tree shapes match", () => {
    for (const [enumName, fields] of [
      ["FilesAvailable", ["name", "size"]],
      ["FilesToSkip", ["name", "skip"]],
    ]) {
      assert.deepEqual(
        rustVariants(CLI_TRANSFER, enumName).sort(),
        ["Dir", "File"],
        `${enumName} variants changed`,
      );
      assert.deepEqual(
        rustVariantFields(CLI_TRANSFER, enumName, "File"),
        fields,
        `${enumName}::File fields changed`,
      );
      assert.deepEqual(
        rustVariantFields(CLI_TRANSFER, enumName, "Dir"),
        ["name", "files"],
        `${enumName}::Dir fields changed`,
      );
      assert.match(TS_TYPES, new RegExp(`${enumName}`), `TS lacks ${enumName}`);
    }
  });

  test("data channel framing matches", () => {
    for (const name of [
      "CHUNK_SIZE",
      "MAX_DC_PAYLOAD",
      "FRAG_MORE",
      "FRAG_LAST",
    ]) {
      const rustValue = rustConst(CLI_WEBRTC, name)
        .replace(
          "CHUNK_SIZE - 1",
          String(Number(rustConst(CLI_WEBRTC, "CHUNK_SIZE")) - 1),
        )
        .replace(/^0x00$/, "0")
        .replace(/^0x01$/, "1");
      const tsValue = tsConst(TS_CONFIG, name)
        .replace(
          "CHUNK_SIZE - 1",
          String(Number(tsConst(TS_CONFIG, "CHUNK_SIZE")) - 1),
        )
        .replace(/^0x00$/, "0")
        .replace(/^0x01$/, "1");
      assert.equal(
        Number(tsValue),
        Number(rustValue),
        `${name} differs — fragments would be framed incompatibly`,
      );
    }
  });

  test("control messages are gzipped on both sides", () => {
    assert.match(TS_COMPRESSION, /CompressionStream\("gzip"\)/);
    assert.match(CLI_TRANSFER, /GzipEncoder/);
    assert.match(CLI_TRANSFER, /GzipDecoder/);
  });

  test("relay message tags match", () => {
    for (const name of ["ClientMessage", "ServerMessage"]) {
      assert.deepEqual(
        rustVariants(CLI_RELAY, name).sort(),
        rustVariants(RELAY_MAIN, name).sort(),
        `${name} differs between the CLI and the relay server`,
      );
    }
    // The relay wire uses snake_case; the browser spells the same tags out.
    const toSnake = (s) => s.replace(/([a-z])([A-Z])/g, "$1_$2").toLowerCase();
    for (const [rustName, tsName] of [
      ["ClientMessage", "ClientMessage"],
      ["ServerMessage", "ServerMessage"],
    ]) {
      const expected = rustVariants(RELAY_MAIN, rustName).map(toSnake).sort();
      assert.deepEqual(
        tsTags(TS_TYPES, tsName).sort(),
        expected,
        `${tsName} tags differ from the relay's`,
      );
    }
  });

  test("the browser advertises a capability the relay knows", () => {
    // The relay negotiates a protocol from what both peers advertise; an
    // unknown string means it can never pair a browser with a CLI.
    assert.match(RELAY_MAIN, /WebRtc|web_rtc/);
    assert.match(
      readFileSync(path.join(here, "../src/lib/transport/relay.ts"), "utf8"),
      /capabilities: \["web_rtc"\]/,
    );
  });
});
