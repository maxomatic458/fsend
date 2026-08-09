import type { FilesAvailable } from "../types";

export type ConnectionKind = "direct" | "relay" | "unknown";

export interface TransferEntry {
  name: string;
  size: number;
  skip: number;
  isDir: boolean;
}


export type TransferEvent =
  /// Sender only
  | { type: "code"; code: string }
  | { type: "waitingPeer" }
  | { type: "connecting" }
  | { type: "handshaking" }
  /// Sender only: files offered.
  | { type: "waitingAccept" }
  /// Receiver only
  | {
      type: "offered";
      files: FilesAvailable[];
      accept: () => void;
      reject: () => void;
    }
  | { type: "transferring"; entries: TransferEntry[] }
  | { type: "progress"; bytes: number }
  | { type: "connectionType"; kind: ConnectionKind }
  | { type: "complete" }
  | { type: "error"; message: string };

export type TransferListener = (event: TransferEvent) => void;
