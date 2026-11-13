import { createSignal, onMount } from "solid-js";
import { runReceive } from "../lib/transfer/receive";
import {
  BrowserStorage,
  folderKind,
  chooseFolder as pickFolder,
  type FolderKind,
  type StorageMode,
} from "../lib/files/storage";
import { createFolderSink, createZipSink } from "../lib/transfer/sinks";
import { createTransferRun, stepOf } from "./createTransferRun";
import type { TransferEvent } from "../lib/transfer/events";
import { CODE_LENGTH, type ConflictPolicy, type TopEntry } from "../lib/proto";

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

  /// The folder the fs mode writes into; without one, zip is the only option.
  const [kind, setKind] = createSignal<FolderKind | null>(folderKind());
  const [mode, setMode] = createSignal<StorageMode>(kind() ? "fs" : "zip");
  const [storage, setStorage] = createSignal<BrowserStorage | null>(null);
  /// Bytes kept in browser storage, once known.
  const [storageUsed, setStorageUsed] = createSignal<number | null>(null);

  const refreshUsage = async () => {
    const opened = storage();
    if (opened) setStorageUsed(await opened.usage().catch(() => null));
  };

  onMount(async () => {
    if (kind() !== "browser") return;
    const opened = await BrowserStorage.open();
    if (!opened) {
      // Refused, as in private windows.
      setKind(null);
      setMode("zip");
      return;
    }
    setStorage(opened);
    await refreshUsage();
  });

  /// False when another tab is using the storage.
  const clearStorage = async () => {
    const opened = storage();
    if (!opened) return true;
    const cleared = await opened.exclusive(() => opened.clear());
    await refreshUsage();
    return cleared;
  };

  const [code, setCodeRaw] = createSignal("");
  const [offered, setOffered] = createSignal<TopEntry[]>([]);
  const [folder, setFolder] = createSignal<FileSystemDirectoryHandle | null>(
    null,
  );
  const [policy, setPolicy] = createSignal<ConflictPolicy>("skip");
  /// Existing files that differed from the offer, once the transfer started.
  const [conflicts, setConflicts] = createSignal({ replaced: 0, skipped: 0 });
  /// 0-100 while the sink packs a zip, null when there is nothing to pack.
  const [packing, setPacking] = createSignal<number | null>(null);

  let accept: (() => void) | null = null;
  let reject: (() => void) | null = null;

  const chooseFolder = async () => {
    if (mode() !== "fs" || kind() !== "picked") return;
    try {
      setFolder(await pickFolder());
    } catch {
      // The picker was dismissed; nothing to do.
    }
  };

  /// Writing to a folder needs one before it can start.
  const isReady = () =>
    code().length === CODE_LENGTH &&
    (mode() === "zip" ||
      (kind() === "picked" ? folder() !== null : storage() !== null));

  const start = () => {
    if (!isReady()) return;
    const signal = run.beginAttempt();
    setState("connecting");
    setConflicts({ replaced: 0, skipped: 0 });

    const staging = mode() === "fs" && kind() === "browser" ? storage() : null;
    const sink =
      mode() === "zip"
        ? createZipSink()
        : staging
          ? createFolderSink(staging.incoming, staging)
          : createFolderSink(folder()!);

    const receive = () =>
      runReceive(
        code(),
        sink,
        // Browser storage only holds earlier transfers, so anything that differs is stale.
        staging ? "replace" : policy(),
        onEvent,
        signal,
      );
    if (!staging) {
      void receive();
      return;
    }
    void staging.exclusive(receive).then((ran) => {
      if (ran) return;
      run.setError(
        "Another tab is receiving into browser storage. Finish that transfer first, or choose Download as Zip.",
      );
      setState("error");
    });
  };

  const onEvent = (event: TransferEvent) => {
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
      case "conflicts":
        setConflicts({ replaced: event.replaced, skipped: event.skipped });
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
        void refreshUsage();
        break;
      case "error":
        run.setError(event.message);
        setState("error");
        void refreshUsage();
        break;
    }
  };

  const acceptOffer = async () => {
    // A picked folder may still be missing if the code was deep-linked.
    if (mode() === "fs" && kind() === "picked" && !folder()) {
      await chooseFolder();
      if (!folder()) return;
    }
    accept?.();
  };

  return {
    /// The folder fs mode writes into, null when only zip is possible.
    kind,
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
    policy,
    setPolicy,
    conflicts,
    storageUsed,
    clearStorage,
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
