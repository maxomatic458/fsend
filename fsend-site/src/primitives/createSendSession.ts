import { createSignal, onCleanup } from "solid-js";
import { runSend } from "../lib/transfer/send";
import { collectFiles } from "../lib/files/source";
import { buildFileTree, totalSize, entrySize } from "../lib/files/tree";
import { SESSION_EXPIRY_SEC } from "../config";
import { createProgressTracker } from "./createProgressTracker";
import type { SelectedEntry } from "../lib/types";
import type { ConnectionKind } from "../lib/transfer/events";

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
  const [entries, setEntries] = createSignal<SelectedEntry[]>([]);
  const [shareCode, setShareCode] = createSignal("");
  const [expiresAt, setExpiresAt] = createSignal(0);
  const [error, setError] = createSignal("");
  const [connection, setConnection] = createSignal<ConnectionKind>("unknown");
  const [selectionSize, setSelectionSize] = createSignal(0);
  // Aligned with entries(); derived from the same tree as the total so the
  // rows and the summary can never disagree.
  const [entrySizes, setEntrySizes] = createSignal<number[]>([]);

  // Recreated per attempt: an AbortController stays aborted forever, so one
  // cancelled session would otherwise poison every retry after it.
  let abortController = new AbortController();

  onCleanup(() => {
    abortController.abort();
    tracker.cleanup();
  });

  const refreshSize = async (items: SelectedEntry[]) => {
    const tree = await buildFileTree(items);
    setSelectionSize(totalSize(tree));
    setEntrySizes(tree.map(entrySize));
  };

  const add = (added: SelectedEntry[]) => {
    const next = [...entries(), ...added];
    setEntries(next);
    refreshSize(next);
  };

  const remove = (index: number) => {
    const next = entries().filter((_, i) => i !== index);
    setEntries(next);
    refreshSize(next);
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
    setEntries([]);
    setShareCode("");
    setError("");
    setSelectionSize(0);
    setEntrySizes([]);
  };

  return {
    state,
    entries,
    shareCode,
    expiresAt,
    error,
    connection,
    selectionSize,
    entrySizes,
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
