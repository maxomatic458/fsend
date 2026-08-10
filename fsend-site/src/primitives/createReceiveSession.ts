import { createSignal, onCleanup } from "solid-js";
import { runReceive } from "../lib/transfer/receive";
import { detectStorage, type Storage } from "../lib/files/storage";
import { createProgressTracker } from "./createProgressTracker";
import type { FilesAvailable } from "../lib/types";
import type { ConnectionKind } from "../lib/transfer/events";

export type ReceiveState =
  | "input"
  | "connecting"
  | "handshaking"
  | "offered"
  | "transferring"
  | "completed"
  | "error";

export function createReceiveSession(initialCode = "") {
  const tracker = createProgressTracker();
  const storage: Storage = detectStorage();

  const [state, setState] = createSignal<ReceiveState>("input");
  const [code, setCodeRaw] = createSignal(normalizeCode(initialCode));
  const [error, setError] = createSignal("");
  const [offered, setOffered] = createSignal<FilesAvailable[]>([]);
  const [folder, setFolder] = createSignal<FileSystemDirectoryHandle | null>(
    null,
  );
  const [connection, setConnection] = createSignal<ConnectionKind>("unknown");
  const [resume, setResume] = createSignal(false);

  let accept: (() => void) | null = null;
  let reject: (() => void) | null = null;
  let abortController = new AbortController();

  onCleanup(() => {
    abortController.abort();
    tracker.cleanup();
  });

  const chooseFolder = async () => {
    if (storage.kind !== "disk") return;
    try {
      setFolder(await storage.chooseFolder());
    } catch {
      // The picker was dismissed; nothing to do.
    }
  };

  /// Disk storage needs somewhere to write before it can start.
  const isReady = () =>
    code().length === 8 && (storage.kind === "download" || folder() !== null);

  const start = () => {
    if (!isReady()) return;
    if (abortController.signal.aborted) abortController = new AbortController();
    setState("connecting");

    const sink =
      storage.kind === "disk"
        ? storage.createSink(folder()!)
        : storage.createSink();

    runReceive(
      code(),
      sink,
      resume(),
      (event) => {
        switch (event.type) {
          case "connecting":
            setState("connecting");
            break;
          case "handshaking":
            setState("handshaking");
            break;
          case "offered":
            setOffered(event.files);
            accept = event.accept;
            reject = event.reject;
            setState("offered");
            break;
          case "transferring":
            tracker.initialize(event.entries);
            setState("transferring");
            break;
          case "progress":
            tracker.recordBytes(event.bytes);
            break;
          case "connectionType":
            setConnection(event.kind);
            break;
          case "complete":
            setState("completed");
            break;
          case "error":
            setError(event.message);
            setState("error");
            break;
        }
      },
      abortController.signal,
    );
  };

  const acceptOffer = async () => {
    // Disk storage may still need a destination if the code was deep-linked
    if (storage.kind === "disk" && !folder()) {
      await chooseFolder();
      if (!folder()) return;
    }
    accept?.();
  };

  return {
    storage,
    state,
    code,
    setCode: (value: string) => setCodeRaw(normalizeCode(value)),
    error,
    offered,
    folder,
    connection,
    resume,
    setResume,
    progress: tracker.progress,
    isReady,
    isTransferring: () => state() === "transferring",
    /** Which of Enter code / Connect / Receive is in progress. */
    step: () => {
      const s = state();
      if (s === "input") return 0;
      if (s === "transferring" || s === "completed") return 2;
      return 1;
    },
    chooseFolder,
    start,
    acceptOffer,
    rejectOffer: () => reject?.(),
    retry: () => {
      setError("");
      setState("input");
    },
  };
}

function normalizeCode(input: string): string {
  return input
    .toUpperCase()
    .replace(/[^A-Z0-9]/g, "")
    .slice(0, 8);
}
