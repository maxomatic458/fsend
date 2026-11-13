import { createSignal } from "solid-js";
import { runReceive } from "../lib/transfer/receive";
import {
  hasFileSystemAccess,
  chooseFolder as pickFolder,
  type StorageMode,
} from "../lib/files/storage";
import { createDiskSink, createDownloadSink } from "../lib/transfer/sinks";
import { createTransferRun, stepOf } from "./createTransferRun";
import { CODE_LENGTH, type TopEntry } from "../lib/proto";

export type ReceiveState =
  | "input"
  | "connecting"
  | "handshaking"
  | "offered"
  | "transferring"
  | "completed"
  | "error";

export function createReceiveSession() {
  const [state, setState] = createSignal<ReceiveState>("input");
  const run = createTransferRun();

  // Writing to disk needs the API; without it "download" is the only option.
  const canUseDisk = hasFileSystemAccess();
  const [mode, setMode] = createSignal<StorageMode>(
    canUseDisk ? "disk" : "download",
  );

  const [code, setCodeRaw] = createSignal("");
  const [offered, setOffered] = createSignal<TopEntry[]>([]);
  const [folder, setFolder] = createSignal<FileSystemDirectoryHandle | null>(
    null,
  );
  const [resume, setResume] = createSignal(false);
  /// 0-100 while the sink packs a zip, null when there is nothing to pack.
  const [packing, setPacking] = createSignal<number | null>(null);

  let accept: (() => void) | null = null;
  let reject: (() => void) | null = null;

  const chooseFolder = async () => {
    if (mode() !== "disk") return;
    try {
      setFolder(await pickFolder());
    } catch {
      // The picker was dismissed; nothing to do.
    }
  };

  /// Writing to disk needs somewhere to write before it can start.
  const isReady = () =>
    code().length === CODE_LENGTH &&
    (mode() === "download" || folder() !== null);

  const start = () => {
    if (!isReady()) return;
    const signal = run.beginAttempt();
    setState("connecting");

    const sink =
      mode() === "disk" ? createDiskSink(folder()!) : createDownloadSink();

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
            run.tracker.recordBytes(event.entry, event.bytes);
            break;
          case "packing":
            setPacking(event.percent);
            break;
          case "connectionType":
            run.setConnection(event.kind);
            break;
          case "complete":
            run.tracker.complete();
            setPacking(null);
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
    // Writing to disk may still need a destination if the code was deep-linked
    if (mode() === "disk" && !folder()) {
      await chooseFolder();
      if (!folder()) return;
    }
    accept?.();
  };

  return {
    /// False when the browser has no File System Access API, so there is no choice to offer.
    canUseDisk,
    mode,
    setMode,
    state,
    error: run.error,
    connection: run.connection,
    progress: run.progress,
    isTransferring: () => state() === "transferring",
    /** Which of Enter code / Connect / Receive is in progress. */
    step: () => stepOf(state(), "input"),
    code,
    setCode: setCodeRaw,
    offered,
    folder,
    resume,
    setResume,
    packing,
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
