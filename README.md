# fsend
fsend is a peer-to-peer file transfer webapp and CLI tool.

> [!NOTE]
> `fsend-site` (the webapp) uses WebRTC for the connection, without a fallback relay. So depending on your network/browser, it may not work. `fsend-cli` makes use of [iroh](https://github.com/n0-computer/iroh) which provides a fallback relay by default.

## Features
- **P2P File Transfer**: Directly send files between devices, a relay is only used if a direct connection cannot be established.
- **Web and CLI Interface**: fsend is available as a web application and a command-line tool.
- **Resumable Transfers**: If the connection is lost, the transfer can be resumed from where it left off.
- **Transfer files & Folders**

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
        2. Share the generated code with the receiver.
    * As a receiver:
        1. Enter the code provided by the sender (If you wish to continue a interrupted transfer, choose "Resume interrupted transfer").
        2. The file(s)/folder(s) will start downloading.

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