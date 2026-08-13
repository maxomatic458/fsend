import { createMemo, createSignal, onCleanup } from "solid-js";
import { runSend } from "../lib/transfer/send";
import { collectFiles } from "../lib/files/source";
import { buildFileTree, entrySize } from "../lib/files/tree";
import { SESSION_EXPIRY_SEC } from "../config";
import { createProgressTracker } from "./createProgressTracker";
import type { SelectedEntry } from "../lib/types";
import type { ConnectionKind } from "../lib/transfer/events";

/// Selected item with its size
export interface SelectedItem {
  entry: SelectedEntry;
  /// null when the size is measured
  size: number | null;
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
  const tracker = createProgressTracker();

  const [state, setState] = createSignal<SendState>("selecting");
  const [items, setItems] = createSignal<SelectedItem[]>([]);
  const [shareCode, setShareCode] = createSignal("");
  const [expiresAt, setExpiresAt] = createSignal(0);
  const [error, setError] = createSignal("");
  const [connection, setConnection] = createSignal<ConnectionKind>("unknown");

  const entries = createMemo(() => items().map((item) => item.entry));
  const selectionSize = createMemo(() =>
    items().reduce((total, item) => total + (item.size ?? 0), 0),
  );

  // Recreated per attempt.
  let abortController = new AbortController();

  onCleanup(() => {
    abortController.abort();
    tracker.cleanup();
  });

  // Measure a single item and update its size in the list.
  const measure = async (item: SelectedItem) => {
    const [tree] = await buildFileTree([item.entry]);
    const size = tree ? entrySize(tree) : 0;
    setItems((current) =>
      current.includes(item)
        ? current.map((each) => (each === item ? { ...each, size } : each))
        : current,
    );
  };

  const add = (added: SelectedEntry[]) => {
    // Appended to list
    const pending: SelectedItem[] = added.map((entry) => ({
      entry,
      size: null,
    }));
    setItems((current) => [...current, ...pending]);
    for (const item of pending) void measure(item);
  };

  const remove = (index: number) => {
    setItems((current) => current.filter((_, i) => i !== index));
  };

  const start = () => {
    if (entries().length === 0) return;
    if (abortController.signal.aborted) abortController = new AbortController();
    setState("connecting");

    runSend(
      entries(),
      (event) => {
        switch (event.type) {
          case "code":
            setShareCode(event.code);
            setExpiresAt(Date.now() + SESSION_EXPIRY_SEC * 1000);
            setState("waiting");
            break;
          case "handshaking":
            setState("handshaking");
            break;
          case "waitingAccept":
            setState("waitingAccept");
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

  const cancel = () => {
    abortController.abort();
    reset();
  };

  const reset = () => {
    setState("selecting");
    setItems([]);
    setShareCode("");
    setError("");
  };

  return {
    state,
    entries,
    shareCode,
    expiresAt,
    error,
    connection,
    items,
    selectionSize,
    progress: tracker.progress,
    isTransferring: () => state() === "transferring",
    /** Which of Choose / Share code / Transfer is in progress. */
    step: () => {
      const s = state();
      if (s === "selecting") return 0;
      if (s === "transferring" || s === "completed") return 2;
      return 1;
    },
    add,
    remove,
    start,
    cancel,
    reset,
    /// Also collects the files, so a caller can verify a selection is readable.
    collect: () => collectFiles(entries()),
  };
}
