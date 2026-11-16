import { createMemo, createSignal } from "solid-js";
import { runSend } from "../lib/transfer/send";
import { buildOffer, offerSizeBytes } from "../lib/files/tree";
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
    const sizeBytes = offerSizeBytes((await buildOffer([item.entry])).entries);
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

  // Events of an earlier attempt that are still on their way are ignored.
  let attempt = 0;

  const start = () => {
    if (entries().length === 0) return;
    const signal = run.beginAttempt();
    const current = ++attempt;
    setState("connecting");

    runSend(
      entries(),
      (event) => {
        if (current !== attempt) return;
        switch (event.type) {
          case "code":
            setShareCode(event.code);
            setExpiresAtMs(Date.now() + event.expiresInSecs * 1000);
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
            run.tracker.recordBytes(event.entry, event.bytes);
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

  /// Back to the selection, which is kept.
  const reset = () => {
    attempt++;
    setState("selecting");
    setShareCode("");
    run.setError("");
  };

  return {
    state,
    error: run.error,
    connection: run.connection,
    progress: run.progress,
    isTransferring: () => state() === "transferring",
    /** Which of Choose / Share Code / Transfer is in progress. */
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
  };
}
