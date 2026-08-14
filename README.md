# fsend
fsend is a peer-to-peer file transfer webapp and CLI tool.

> [!NOTE]
> `fsend-site` (the webapp) uses WebRTC for the connection, without a fallback relay. So depending on your network/browser, it may not work. `fsend-cli` makes use of [iroh](https://github.com/n0-computer/iroh) which provides a fallback relay by default.

## Features
- **P2P File Transfer**: Directly send files between devices. The relay server only introduces the two peers — file data never passes through it.
- **Web and CLI Interface**: fsend is available as a web application and a command-line tool, and the two interoperate.
- **Resumable Transfers**: If the connection is lost, the transfer can be resumed from where it left off (see the browser support table below).
- **Files and Folders**: Send individual files or whole folders — the directory structure is preserved on the receiving side.

## Browser support

The webapp needs WebRTC, which every current browser has. What differs is
where received files are written.

| | Chromium (Chrome, Edge, Opera) | Firefox, Safari |
| --- | --- | --- |
| Receives into | a folder you pick, streamed to disk | memory, saved at the end |
| Folders | kept as folders | delivered as a zip |
| Limited by | free disk space | free RAM |
| Resume an interrupted transfer | yes | no |

The difference is the [File System Access API](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API); the webapp detects it and says which mode you are in.

## Installation

The cli version can be installed via cargo.

```bash
cargo install fsend-cli
```
## Usage

### Webapp
1. Open [fsend](https://fsend.sh) in your browser.
2. Either send or receive a file
    * As a sender:
        1. Drag and Drop or select the file(s)/folder(s) you want to send.
        2. Share the generated code — or the link, or the QR code — with the receiver.
    * As a receiver:
        1. Enter the code provided by the sender. Pasting the sender's link works too.
        2. On Chromium, pick the folder to save into (tick "Resume an interrupted transfer" to continue a previous attempt).
        3. Accept the offer and the file(s)/folder(s) will start transferring.

Both tabs have to stay open for the whole transfer; nothing is stored in
between.

### CLI

#### Sending files
```bash
$ fsend-cli send <file_path>
Session code: 123456

On the other peer, run:

  fsend-cli receive 123456
```

#### Receiving files
```bash
$ fsend-cli receive 123456
```

## Development

```bash
cd fsend-site
bun install
bun run dev      # http://localhost:3000
bun run build
bun run test     # transfer + protocol suite, no browser needed
```

The webapp test suite runs the real transfer modules against an in-process
fake browser (WebRTC, WebSocket, File System Access), and also checks the
wire format still matches `fsend-cli` and `fsend-relay`. See
[`fsend-site/test/README.md`](fsend-site/test/README.md).
