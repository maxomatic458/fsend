# Transfer test suite (this is completely ai generated)

Runs the real `runSend` / `runReceive` against an in-process fake browser. No headless browser, no network, no relay server —
the whole suite finishes in about two seconds.

```bash
bun run test                     # or: node test/run.mjs
node test/run.mjs --test-reporter=spec     # per-test output
node test/run.mjs --test-name-pattern=resume
```

## How it works

`test/run.mjs` bundles `test/entry.ts` (a barrel over `src/lib`) into plain ESM
with esbuild, then runs Node's built-in test runner. Bundling rather than
mocking means the code under test is the code that ships — no reimplementation
to drift out of sync.

`test/harness/` provides the four browser APIs the transfer code touches:

| Module          | Stands in for                              | Notes                                                                                                                                                                                                                                                        |
| --------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `webrtc.mjs`    | `RTCPeerConnection`, `RTCDataChannel`      | Loopback pair. Messages are copied and delivered asynchronously in order; `bufferedAmount` rises and falls so backpressure is real. Exposes `simulateAbruptDisconnect()` — the peer vanishes with no graceful close, which a browser will not do on command. |
| `relay.mjs`     | `WebSocket` + fsend-relay                  | Speaks the same JSON protocol as `src/lib/types.ts`: session creation, join, peer-joined, SDP exchange.                                                                                                                                                      |
| `fs-access.mjs` | File System Access API                     | In-memory directories and files. Writables commit as they go, so an abandoned one leaves partial data behind — which is what resume reads. `snapshot()` flattens a tree to `{ path: bytes }`.                                                                |
| `dom.mjs`       | Object URLs, anchor download, `FileReader` | Captures what would have been downloaded. `FileReader` is needed because jszip reads Blob input through it.                                                                                                                                                  |

## Coverage

- **Complete transfers (native FS):** single file, several files with a nested
  folder, picker-handle sources, a file past `MAX_BUFFERED_BYTES` (backpressure), an
  empty file.
- **Complete transfers (fallback):** single-file direct download, folder as a
  zip, multiple loose files as a zip. Same `runReceive`, driven by a download
  sink instead of a disk sink.
- **Interrupted:** receiver's tab disappears mid-send, the same with a send
  buffer that never fills (which isolates the per-chunk disconnect check),
  sender's tab disappears mid-receive, offer rejected, sender aborted.
- **Resume:** a cut-off download continues and ends byte-perfect, sending only
  the remainder; resuming with nothing on disk transfers everything.
- **Relay:** unknown share code surfaces an error.
- **`watchDisconnect` unit tests:** graceful close, `failed` on either
  transport, a blip that recovers, a blip that does not, and `stop()`.

## Adding a scenario

`makeEntries(spec, mode)` builds the sender's input from a plain object —
`Uint8Array` values are files, nested objects are folders. `mode: "file"` gives
plain `File` entries (drag-and-drop or `<input>`); `mode: "handle"` gives File
System Access handles (the Chromium picker path). `expectedSnapshot(spec)`
produces the matching `{ path: bytes }` to assert against
`dirHandle.snapshot()`.

Use `bytes(n, seed)` for payloads — it is deterministic, so a failure
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
