import { createSignal, onCleanup } from "solid-js";
import { createProgressTracker } from "./createProgressTracker";
import type { ConnectionKind } from "../lib/transfer/events";

/**
 * Shared parts of a transfer attempt: progress tracker, connection kind, error
 * and abort controller. Holds no state signal — each side owns its own.
 */
export function createTransferRun() {
  const tracker = createProgressTracker();

  const [error, setError] = createSignal("");
  const [connection, setConnection] = createSignal<ConnectionKind>("unknown");

  // Recreated per attempt.
  let abortController = new AbortController();

  onCleanup(() => {
    abortController.abort();
    tracker.cleanup();
  });

  return {
    /** initialize / recordBytes / complete, driven from the event handler. */
    tracker,
    progress: tracker.progress,
    error,
    setError,
    connection,
    setConnection,
    /** Fresh signal for a new attempt, reusing the current one until it aborts. */
    beginAttempt: (): AbortSignal => {
      if (abortController.signal.aborted) {
        abortController = new AbortController();
      }
      return abortController.signal;
    },
    /** Cancels the attempt and stops the clock it left running. */
    abort: () => {
      abortController.abort();
      tracker.cleanup();
    },
  };
}

/**
 * Which of the side's three steps is in progress, zero-based. Both sides end
 * on transfer, so only the name of the first step differs.
 */
export function stepOf(state: string, first: string): number {
  if (state === first) return 0;
  if (state === "transferring" || state === "completed") return 2;
  return 1;
}
