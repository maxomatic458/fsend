import { For, Show } from "solid-js";
import { FiFolder, FiFile, FiX } from "solid-icons/fi";
import { formatBytes } from "../lib/format";
import type { SelectedEntry } from "../lib/types";

interface FileListProps {
  entries: SelectedEntry[];
  // Aligned with `entries`; may lag by a tick while a tree is being built.
  sizes: number[];
  onRemove: (index: number) => void;
  totalSize: number;
}

/// The chosen files
export function FileList(props: FileListProps) {
  return (
    <Show when={props.entries.length > 0}>
      <div class="flex flex-col border-t border-line max-h-72 overflow-y-auto">
        <For each={props.entries}>
          {(entry, i) => (
            <div class="flex items-center justify-between gap-3 py-3.5 px-1 border-b border-line">
              <div class="flex items-center gap-3 min-w-0">
                {entry.kind === "directory" ? (
                  <FiFolder class="w-4 h-4 text-ink-faint shrink-0" />
                ) : (
                  <FiFile class="w-4 h-4 text-ink-faint shrink-0" />
                )}
                <span class="text-[15.5px] truncate">{entry.name}</span>
              </div>
              <div class="flex items-center gap-2 shrink-0">
                <Show when={props.sizes[i()] !== undefined}>
                  <span class="font-mono text-[13px] text-ink-dim">
                    {formatBytes(props.sizes[i()])}
                  </span>
                </Show>
                <button
                  onClick={() => props.onRemove(i())}
                  class="text-ink-faint hover:text-red-500 transition-colors p-1 cursor-pointer"
                  aria-label={`Remove ${entry.name}`}
                >
                  <FiX class="w-4 h-4" />
                </button>
              </div>
            </div>
          )}
        </For>
        <div class="flex items-center justify-between py-3 px-1 font-mono text-xs text-ink-faint">
          <span>
            {props.entries.length} item{props.entries.length === 1 ? "" : "s"}
          </span>
          <span>total {formatBytes(props.totalSize)}</span>
        </div>
      </div>
    </Show>
  );
}
