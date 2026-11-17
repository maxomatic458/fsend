import type { SelectedEntry } from "./types";

/** A drop on the home page, handed to /send across the navigation. */
let pending: SelectedEntry[] | null = null;

export function setPendingDrop(entries: SelectedEntry[]): void {
    pending = entries;
}

/** Returns the pending drop, if any, and clears it. */
export function takePendingDrop(): SelectedEntry[] | null {
    const entries = pending;
    pending = null;
    return entries;
}
