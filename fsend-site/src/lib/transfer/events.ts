import type { Offer } from "../proto";

export type ConnectionKind = "direct" | "relay" | "unknown";

export interface TransferEntry {
    name: string;
    sizeBytes: number;
    // Bytes that were already present on receiver and skipped.
    skipBytes: number;
    isDir: boolean;
}

export type TransferEvent =
    /// Sender only: reading the files for their checksums, before pairing.
    | { type: "hashing"; done: number; total: number }
    /// Sender only
    | { type: "code"; code: string; expiresInSecs: number }
    /// Sender only: the receiver has the offer, and its user decides.
    | { type: "waitingAccept" }
    /// Sender only: the receiver accepted, and the peers connect.
    | { type: "handshaking" }
    /// Receiver only: joining the sender's session.
    | { type: "connecting" }
    /// Receiver only: nothing about this side has reached the sender yet.
    | {
          type: "offered";
          offer: Offer;
          accept: () => void;
          reject: () => void;
      }
    /// Receiver only: checking what is already here against the checksums.
    | { type: "verifying"; done: number; total: number }
    /// Receiver only: the browser asks the user to let fsend store more.
    | { type: "askingStorage" }
    /// Receiver only: there was no room where the files were going, so they are held in memory.
    | { type: "heldInMemory" }
    | { type: "transferring"; entries: TransferEntry[] }
    /// Receiver only: existing entries that differ from the offer, replaced or left alone.
    | { type: "conflicts"; replaced: number; skipped: number }
    /// `bytes` more of the entry at `entry` in `transferring`.
    | { type: "progress"; entry: number; bytes: number }
    /// Receiver only: bytes are all in, but the sink is still packing them.
    | { type: "packing"; percent: number }
    | { type: "connectionType"; kind: ConnectionKind }
    | { type: "complete" }
    | { type: "error"; message: string };

export type TransferListener = (event: TransferEvent) => void;
