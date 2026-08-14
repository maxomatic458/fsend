import { createSignal } from "solid-js";
import { runReceive } from "../lib/transfer/receive";
import { detectStorage, type Storage } from "../lib/files/storage";
import { createTransferRun, stepOf } from "./createTransferRun";
import type { FilesAvailable } from "../lib/types";

export type ReceiveState =
  | "input"
  | "connecting"
  | "handshaking"
  | "offered"
  | "transferring"
  | "completed"
  | "error";

export function createReceiveSession(initialCode = "") {
  const [state, setState] = createSignal<ReceiveState>("input");
  const run = createTransferRun();
  const storage: Storage = detectStorage();

  const [code, setCodeRaw] = createSignal(normalizeCode(initialCode));
  const [offered, setOffered] = createSignal<FilesAvailable[]>([]);
  const [folder, setFolder] = createSignal<FileSystemDirectoryHandle | null>(
    null,
  );
  const [resume, setResume] = createSignal(false);

  let accept: (() => void) | null = null;
  let reject: (() => void) | null = null;

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
    const signal = run.beginAttempt();
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
            run.tracker.initialize(event.entries);
            setState("transferring");
            break;
          case "progress":
            run.tracker.recordBytes(event.bytes);
            break;
          case "connectionType":
            run.setConnection(event.kind);
            break;
          case "complete":
            run.tracker.complete();
            setState("completed");
            break;
          case "error":
            run.setError(event.message);
            setState("error");
            break;
        }
      },
      signal,
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
    error: run.error,
    connection: run.connection,
    progress: run.progress,
    isTransferring: () => state() === "transferring",
    /** Which of Enter code / Connect / Receive is in progress. */
    step: () => stepOf(state(), "input"),
    code,
    setCode: (value: string) => setCodeRaw(normalizeCode(value)),
    offered,
    folder,
    resume,
    setResume,
    isReady,
    chooseFolder,
    start,
    acceptOffer,
    rejectOffer: () => reject?.(),
    retry: () => {
      run.setError("");
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
