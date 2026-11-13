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

export interface SelectedEntry {
  kind: "file" | "directory";
  name: string;
  handle?: FileSystemDirectoryHandle | FileSystemFileHandle;
  file?: File;
  files?: { relativePath: string; file: File }[];
}
