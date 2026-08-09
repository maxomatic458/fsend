import { For, Show } from "solid-js";
import { FiFolder, FiFile, FiX } from "solid-icons/fi";
import { formatBytes } from "../lib/format";
import type { SelectedEntry } from "../lib/types";

interface FileListProps {
  entries: SelectedEntry[];
  onRemove: (index: number) => void;
  totalSize: number;
}

export function FileList(props: FileListProps) {
  return (
    <Show when={props.entries.length > 0}>
      <div class="mb-6">
        <div class="border border-line rounded-lg divide-y divide-line max-h-60 overflow-y-auto">
          <For each={props.entries}>
            {(entry, i) => (
              <div class="flex items-center justify-between py-3 px-4 hover:bg-surface-2">
                <div class="flex items-center gap-3">
                  {entry.kind === "directory" ? (
                    <FiFolder class="w-6 h-6 text-ink-dim" />
                  ) : (
                    <FiFile class="w-6 h-6 text-ink-dim" />
                  )}
                  <div>
                    <div class="font-medium text-ink">
                      {entry.name}
                    </div>
                  </div>
                </div>
                <button
                  onClick={() => props.onRemove(i())}
                  class="text-red-500 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300 p-2"
                >
                  <FiX class="w-5 h-5" />
                </button>
              </div>
            )}
          </For>
        </div>
        <div class="text-right text-ink-muted mt-2">
          Total: {formatBytes(props.totalSize)}
        </div>
      </div>
    </Show>
  );
}
