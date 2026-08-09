import { createSignal, onCleanup } from "solid-js";
import { runSend } from "../lib/transfer/send";
import { collectFiles } from "../lib/files/source";
import { buildFileTree, totalSize } from "../lib/files/tree";
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

  // Recreated per attempt: an AbortController stays aborted forever, so one
  // cancelled session would otherwise poison every retry after it.
  let abortController = new AbortController();

  onCleanup(() => {
    abortController.abort();
    tracker.cleanup();
  });

  const refreshSize = async (items: SelectedEntry[]) => {
    setSelectionSize(totalSize(await buildFileTree(items)));
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

    runSend(entries(), (event) => {
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
    }, abortController.signal);
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
  };

  return {
    state,
    entries,
    shareCode,
    expiresAt,
    error,
    connection,
    selectionSize,
    progress: tracker.progress,
    isTransferring: () => state() === "transferring",
    add,
    remove,
    start,
    cancel,
    reset,
    /// Also collects the files, so a caller can verify a selection is readable.
    collect: () => collectFiles(entries()),
  };
}
