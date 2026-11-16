import { For, Show } from "solid-js";
import { FiX } from "solid-icons/fi";
import { formatBytes, plural } from "../lib/format";
import type { SelectedItem } from "../primitives/createSendSession";
import { EntryName } from "./EntryName";

interface FileListProps {
  items: SelectedItem[];
  onRemove: (index: number) => void;
  totalSizeBytes: number;
}

export function FileList(props: FileListProps) {
  return (
    <Show when={props.items.length > 0}>
      <div class="flex flex-col border-t border-line max-h-72 overflow-y-auto">
        <For each={props.items}>
          {(item, i) => (
            <div class="flex items-center justify-between gap-3 py-3.5 px-1 border-b border-line">
              <EntryName
                name={item.entry.name}
                dir={item.entry.kind === "directory"}
              />
              <div class="flex items-center gap-2 shrink-0">
                <Show
                  when={!item.error}
                  fallback={
                    <span
                      class="font-mono text-[13px] font-bold text-danger"
                      title={item.error}
                      aria-label={`Could not read ${item.entry.name}: ${item.error}`}
                    >
                      !
                    </span>
                  }
                >
                  <span class="font-mono text-[13px] text-ink-dim">
                    {item.sizeBytes === null
                      ? "..."
                      : formatBytes(item.sizeBytes)}
                  </span>
                </Show>
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
          <span>{plural(props.items.length, "item", "items")}</span>
          <span>total {formatBytes(props.totalSizeBytes)}</span>
        </div>
      </div>
    </Show>
  );
}
