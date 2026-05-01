export type FilesAvailable =
  | { type: "File"; name: string; size: number }
  | { type: "Dir"; name: string; files: FilesAvailable[] };

export type FilesToSkip =
  | { type: "File"; name: string; skip: number }
  | { type: "Dir"; name: string; files: FilesToSkip[] };

export type FileSendRecvTree =
  | { type: "File"; name: string; skip: number; size: number }
  | { type: "Dir"; name: string; files: FileSendRecvTree[] };

// Control channel messages
export type SenderToReceiver =
  | { type: "ConnRequest"; version: string }
  | { type: "FileInfo"; files: FilesAvailable[] };

export type ReceiverToSender =
  | { type: "Ok" }
  | { type: "WrongVersion"; expected: string }
  | { type: "AcceptFilesSkip"; files: (FilesToSkip | null)[] }
  | { type: "RejectFiles" };

// Relay protocol messages
export type ClientMessage =
  | { type: "create_session"; capabilities: string[] }
  | { type: "join_session"; code: string; capabilities: string[] }
  | { type: "exchange"; connection_info: ConnectionInfo };

export type ServerMessage =
  | { type: "create_session"; code: string }
  | { type: "join_session"; protocol: string }
  | { type: "peer_joined"; protocol: string }
  | { type: "exchange"; connection_info: ConnectionInfo }
  | { type: "error"; message: string };

export interface ConnectionInfo {
  type: "web_rtc";
  sdp: string;
  ice_candidates: string[];
}

export interface SelectedEntry {
  kind: "file" | "directory";
  name: string;
  handle?: FileSystemDirectoryHandle | FileSystemFileHandle;
  file?: File;
  files?: { relativePath: string; file: File }[];
}

export interface EntryProgress {
  name: string;
  kind: "file" | "directory";
  size: number;
  transferred: number;
}
