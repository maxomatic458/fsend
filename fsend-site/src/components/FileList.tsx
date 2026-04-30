import { For, Show } from 'solid-js';
import { FiFolder, FiFile, FiX } from 'solid-icons/fi';
import { formatBytes } from '../lib/format';
import type { SelectedEntry } from '../lib/types';

interface FileListProps {
  entries: SelectedEntry[];
  onRemove: (index: number) => void;
  totalSize: number;
}

export function FileList(props: FileListProps) {
  return (
    <Show when={props.entries.length > 0}>
      <div class="mb-6">
        <div class="border border-gray-200 dark:border-neutral-700 rounded-lg divide-y divide-gray-200 dark:divide-neutral-700 max-h-60 overflow-y-auto">
          <For each={props.entries}>
            {(entry, i) => (
              <div class="flex items-center justify-between py-3 px-4 hover:bg-gray-50 dark:hover:bg-neutral-700/50 transition-colors">
                <div class="flex items-center gap-3">
                  {entry.kind === 'directory' ? (
                    <FiFolder class="w-6 h-6 text-gray-500 dark:text-gray-400" />
                  ) : (
                    <FiFile class="w-6 h-6 text-gray-500 dark:text-gray-400" />
                  )}
                  <div>
                    <div class="font-medium text-gray-800 dark:text-gray-100">{entry.name}</div>
                  </div>
                </div>
                <button
                  onClick={() => props.onRemove(i())}
                  class="text-red-500 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300 p-2 transition-colors"
                >
                  <FiX class="w-5 h-5" />
                </button>
              </div>
            )}
          </For>
        </div>
        <div class="text-right text-gray-600 dark:text-gray-400 mt-2">
          Total: {formatBytes(props.totalSize)}
        </div>
      </div>
    </Show>
  );
}
