import { For } from 'solid-js';
import { FiFolder, FiFile } from 'solid-icons/fi';
import { formatBytes } from '../lib/format';
import { totalSize, entrySize } from '../lib/fileTree';
import type { FilesAvailable } from '../lib/types';
import { Button } from './Button';

interface FileOfferProps {
  files: FilesAvailable[];
  onAccept: () => void;
  onReject: () => void;
}

export function FileOffer(props: FileOfferProps) {
  return (
    <div>
      <h3 class="text-lg font-semibold mb-3 text-gray-800 dark:text-gray-100">
        Incoming Files
      </h3>
      <p class="text-sm text-gray-500 dark:text-neutral-400 mb-4">
        The sender wants to share the following files with you:
      </p>

      <div class="border border-gray-200 dark:border-neutral-700 rounded-lg divide-y divide-gray-200 dark:divide-neutral-700 max-h-60 overflow-y-auto mb-4">
        <For each={props.files}>
          {(entry) => (
            <div class="flex items-center justify-between py-3 px-4">
              <div class="flex items-center gap-3">
                {entry.type === 'Dir' ? (
                  <FiFolder class="w-6 h-6 text-gray-500 dark:text-gray-400" />
                ) : (
                  <FiFile class="w-6 h-6 text-gray-500 dark:text-gray-400" />
                )}
                <span class="font-medium text-gray-800 dark:text-gray-100">
                  {entry.name}
                </span>
              </div>
              <span class="text-sm text-gray-500 dark:text-gray-400">
                {formatBytes(entrySize(entry))}
              </span>
            </div>
          )}
        </For>
      </div>

      <div class="flex justify-between items-center">
        <span class="text-sm font-medium text-gray-700 dark:text-gray-300">
          Total: {formatBytes(totalSize(props.files))}
        </span>
        <div class="flex gap-3">
          <Button variant="red" onClick={props.onReject}>
            Reject
          </Button>
          <Button variant="green" onClick={props.onAccept}>
            Accept
          </Button>
        </div>
      </div>
    </div>
  );
}
