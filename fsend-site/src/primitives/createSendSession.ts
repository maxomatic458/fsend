import { createMemo, createSignal } from "solid-js";
import { runSend } from "../lib/transfer/send";
import { collectFiles } from "../lib/files/source";
import { buildFileTree, entrySizeBytes } from "../lib/files/tree";
import { SESSION_EXPIRY_SECS } from "../config";
import { createTransferRun, stepOf } from "./createTransferRun";
import type { SelectedEntry } from "../lib/types";

/// Selected item with its size
export interface SelectedItem {
  entry: SelectedEntry;
  /// null when the size is measured
  sizeBytes: number | null;
}

export type SendState =
  | "selecting"
  | "connecting"
  | "waiting"
  | "handshaking"
  | "waitingAccept"
  | "transferring"
  | "completed"
  | "error";

export function createSendSession() {
  const [state, setState] = createSignal<SendState>("selecting");
  const run = createTransferRun();

  const [items, setItems] = createSignal<SelectedItem[]>([]);
  const [shareCode, setShareCode] = createSignal("");
  const [expiresAtMs, setExpiresAtMs] = createSignal(0);

  const entries = createMemo(() => items().map((item) => item.entry));
  const selectionSizeBytes = createMemo(() =>
    items().reduce((total, item) => total + (item.sizeBytes ?? 0), 0),
  );

  // Measure a single item and update its size in the list.
  const measure = async (item: SelectedItem) => {
    const [tree] = await buildFileTree([item.entry]);
    const sizeBytes = tree ? entrySizeBytes(tree) : 0;
    setItems((current) =>
      current.includes(item)
        ? current.map((each) => (each === item ? { ...each, sizeBytes } : each))
        : current,
    );
  };

  const add = (added: SelectedEntry[]) => {
    // Appended to list
    const pending: SelectedItem[] = added.map((entry) => ({
      entry,
      sizeBytes: null,
    }));
    setItems((current) => [...current, ...pending]);
    for (const item of pending) void measure(item);
  };

  const remove = (index: number) => {
    setItems((current) => current.filter((_, i) => i !== index));
  };

  const start = () => {
    if (entries().length === 0) return;
    const signal = run.beginAttempt();
    setState("connecting");

    runSend(
      entries(),
      (event) => {
        switch (event.type) {
          case "code":
            setShareCode(event.code);
            setExpiresAtMs(Date.now() + SESSION_EXPIRY_SECS * 1000);
            setState("waiting");
            break;
          case "handshaking":
            setState("handshaking");
            break;
          case "waitingAccept":
            setState("waitingAccept");
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

  const reset = () => {
    setState("selecting");
    setItems([]);
    setShareCode("");
    run.setError("");
  };

  return {
    state,
    error: run.error,
    connection: run.connection,
    progress: run.progress,
    isTransferring: () => state() === "transferring",
    /** Which of Choose / Share code / Transfer is in progress. */
    step: () => stepOf(state(), "selecting"),
    entries,
    shareCode,
    expiresAtMs,
    items,
    selectionSizeBytes,
    add,
    remove,
    start,
    cancel: () => {
      run.abort();
      reset();
    },
    reset,
    /// Also collects the files, so a caller can verify a selection is readable.
    collect: () => collectFiles(entries()),
  };
}
