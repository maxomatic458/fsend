import type { SelectedEntry } from "./types";

/**
 * A drop that landed on the home page is handed to /send across the navigation.
 * Module state rather than a property on `window`, so it stays typed and cannot
 * be read by anything that did not import this file.
 */
let pending: SelectedEntry[] | null = null;

export function setPendingDrop(entries: SelectedEntry[]): void {
  pending = entries;
}

/** Returns the pending drop, if any, and clears it — it is only ever used once. */
export function takePendingDrop(): SelectedEntry[] | null {
  const entries = pending;
  pending = null;
  return entries;
}
