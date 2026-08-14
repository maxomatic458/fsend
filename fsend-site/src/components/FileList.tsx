import { For, Show } from "solid-js";
import { FiFolder, FiFile, FiX } from "solid-icons/fi";
import { formatBytes } from "../lib/format";
import type { SelectedItem } from "../primitives/createSendSession";

interface FileListProps {
  items: SelectedItem[];
  onRemove: (index: number) => void;
  totalSizeBytes: number;
}

/**
 * The chosen files, on hairlines to match the offer and progress screens.
 *
 * Each item carries its own size, so a row can never be labelled with another
 * row's number.
 */
export function FileList(props: FileListProps) {
  return (
    <Show when={props.items.length > 0}>
      <div class="flex flex-col border-t border-line max-h-72 overflow-y-auto">
        <For each={props.items}>
          {(item, i) => (
            <div class="flex items-center justify-between gap-3 py-3.5 px-1 border-b border-line">
              <div class="flex items-center gap-3 min-w-0">
                {item.entry.kind === "directory" ? (
                  <FiFolder class="w-4 h-4 text-ink-faint shrink-0" />
                ) : (
                  <FiFile class="w-4 h-4 text-ink-faint shrink-0" />
                )}
                <span class="text-[15.5px] truncate">{item.entry.name}</span>
              </div>
              <div class="flex items-center gap-2 shrink-0">
                <span class="font-mono text-[13px] text-ink-dim">
                  {item.sizeBytes === null ? "…" : formatBytes(item.sizeBytes)}
                </span>
                <button
                  onClick={() => props.onRemove(i())}
                  class="text-ink-faint hover:text-danger transition-colors p-1 cursor-pointer"
                  aria-label={`Remove ${item.entry.name}`}
                >
                  <FiX class="w-4 h-4" />
                </button>
              </div>
            </div>
          )}
        </For>
        <div class="flex items-center justify-between py-3 px-1 font-mono text-xs text-ink-faint">
          <span>
            {props.items.length} item{props.items.length === 1 ? "" : "s"}
          </span>
          <span>total {formatBytes(props.totalSizeBytes)}</span>
        </div>
      </div>
    </Show>
  );
}
