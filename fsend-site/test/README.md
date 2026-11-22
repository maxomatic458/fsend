# WebRTC Transfer test suite

> This test suite is pretty much completely ai generated atm.
>
> TODO: properly review and rewrite tests.

Runs the real `runSend` / `runReceive` against an in-process fake browser. No headless browser, no network, no relay server;
the whole suite finishes in about two seconds.

```bash
bun run test                     # or: node test/run.mjs
node test/run.mjs --test-reporter=spec     # per-test output
node test/run.mjs --test-name-pattern=resume
```

## How it works

`test/run.mjs` builds the protocol wasm (`scripts/build-wasm.mjs`), bundles
`test/entry.ts` (a barrel over `src/lib`) into plain ESM with esbuild, then runs
Node's built-in test runner. The harness loads the wasm from disk. Bundling rather than
mocking means the code under test is the code that ships, with no reimplementation
to drift out of sync.

`test/harness/` provides the five browser APIs the transfer code touches:

| Module          | Stands in for                         | Notes                                                                                                                                                                                                                                                       |
| --------------- | ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `webrtc.mjs`    | `RTCPeerConnection`, `RTCDataChannel` | Loopback pair. Messages are copied and delivered asynchronously in order; `bufferedAmount` rises and falls so backpressure is real. Exposes `simulateAbruptDisconnect()`: the peer vanishes with no graceful close, which a browser will not do on command. |
| `relay.mjs`     | `WebSocket` + fsend-server            | Speaks relay protocol 2: create, join, paired (with the receiver's stats token), and verbatim forwarding of binary signal frames. Pairing runs in the app's real wasm.                                                                                      |
| `fs-access.mjs` | File System Access API                | In-memory directories and files. Writables commit as they go, so an abandoned one leaves partial data behind, which is what resume reads. `snapshot()` flattens a tree to `{ path: bytes }`.                                                                |
| `dom.mjs`       | Object URLs, anchor download          | Captures what would have been downloaded and the stats beacons; `unzip` in `index.mjs` reads the zips back.                                                                                                                                                 |
| `worker.mjs`    | `Worker` (the codec worker)           | Runs the real codec in-process. Messages cross by structured clone, so buffers handed to the worker are emptied on the page's side, as in a browser.                                                                                                        |

## Coverage

- **Complete transfers (native FS):** single file, several files with a nested
  folder, picker-handle sources, a file past `MAX_BUFFERED_BYTES` (backpressure), an
  empty file, an empty folder, progress per entry; a selection that breaks the
  offer rules is refused before pairing.
- **The offer:** it arrives through the relay before the peers connect, sorted
  by name at every level; declining it connects nothing; one larger than a
  relay frame arrives whole.
- **Complete transfers (fallback):** single-file direct download, folder as a
  zip, multiple loose files as a zip. Same `runReceive`, driven by a download
  sink instead of a disk sink.
- **Interrupted:** receiver's tab disappears mid-send, the same with a send
  buffer that never fills (which isolates the per-chunk disconnect check),
  sender's tab disappears mid-receive, offer rejected, a cancelled receive
  reaches the sender as a cancel, sender aborted.
- **Resume:** a cut-off download continues and ends byte-perfect, sending only
  the remainder; complete files are skipped; resuming with nothing on disk
  transfers everything.
- **Checksums:** they come with every file and match SHA-256; a file changed
  on the way fails the receiver, which the sender only sees leave; what is
  already on disk is checked before resuming; a complete file that differs
  fails before anything is accepted; all of it also works without a worker.
- **Stats:** a receiver reports the bytes it got and over which connection
  with its token, also after a cancel, and nothing when nothing arrived.
- **Peer protocol building blocks:** the wasm frame codec round-trips messages
  split anywhere, compresses large ones, encodes and decodes only the messages
  of the given side and rejects unknown or malformed messages and frames; the
  offer rules and ids, the accept, per-file progress and SHA-256 match the CLI. The rules
  themselves are tested in `fsend-proto`, hostile peers in the CLI transport
  suites.
- **Protocol errors:** a sender that gets a sender-only, unknown or malformed
  message fails with a protocol error, and the receiver learns why.
- **Relay:** unknown and malformed codes surface errors, a wrong password fails
  on both sides, a share link works as the code.
- **`watchDisconnect` unit tests:** graceful close, `failed` on either
  transport, a blip that recovers, a blip that does not, and `stop()`.

## Adding a scenario

`makeEntries(spec, mode)` builds the sender's input from a plain object:
`Uint8Array` values are files, nested objects are folders. `mode: "file"` gives
plain `File` entries (drag-and-drop or `<input>`); `mode: "handle"` gives File
System Access handles (the Chromium picker path). `expectedSnapshot(spec)`
produces the matching `{ path: bytes }` to assert against
`dirHandle.snapshot()`.

Use `bytes(n, seed)` for payloads. It is deterministic, so a failure
reproduces exactly.

## Bugs this suite has already caught

- Both receivers deadlocked on a zero-byte file: completion was only ever
  checked after a chunk arrived, and an empty file never produces one.
- The sender kept streaming after the receiver's tab closed, because a
  disconnect was only detected via the data channel's `close` event, which an
  abrupt departure never sends.
- A rejected offer reached the sender as "peer disconnected", because the
  receiver tore the connection down before the message drained.
- `watchDisconnect` treated the peer as healthy whenever _either_ transport
  still read `connected`, so a stale value masked a real drop.
- Mutation testing found the interruption test was passing only via the
  backpressure path: removing the sender's per-chunk disconnect check left the
  suite green. `instantDrain` on the fake channel closed that hole.
