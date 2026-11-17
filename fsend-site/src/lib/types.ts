/** Something the user picked or dropped to send: through a handle, or as `File`s. */
export type SelectedEntry =
    | { kind: "file"; name: string; handle: FileSystemFileHandle }
    | { kind: "file"; name: string; file: File }
    | { kind: "directory"; name: string; handle: FileSystemDirectoryHandle }
    | {
          kind: "directory";
          name: string;
          files: { relativePath: string; file: File }[];
      };
