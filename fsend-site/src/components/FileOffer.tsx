import { For } from "solid-js";
import { FiFolder, FiFile } from "solid-icons/fi";
import { formatBytes } from "../lib/format";
import { totalSize, entrySize } from "../lib/files/tree";
import type { FilesAvailable } from "../lib/types";
import { Button } from "./Button";

interface FileOfferProps {
  files: FilesAvailable[];
  onAccept: () => void;
  onReject: () => void;
}

export function FileOffer(props: FileOfferProps) {
  return (
    <div>
      <h3 class="text-lg font-semibold mb-3 text-ink">
        Incoming Files
      </h3>
      <p class="text-sm text-ink-dim mb-4">
        The sender wants to share the following files with you:
      </p>

      <div class="border border-line rounded-lg divide-y divide-line max-h-60 overflow-y-auto mb-4">
        <For each={props.files}>
          {(entry) => (
            <div class="flex items-center justify-between py-3 px-4">
              <div class="flex items-center gap-3">
                {entry.type === "Dir" ? (
                  <FiFolder class="w-6 h-6 text-ink-dim" />
                ) : (
                  <FiFile class="w-6 h-6 text-ink-dim" />
                )}
                <span class="font-medium text-ink">
                  {entry.name}
                </span>
              </div>
              <span class="text-sm text-ink-dim">
                {formatBytes(entrySize(entry))}
              </span>
            </div>
          )}
        </For>
      </div>

      <div class="flex justify-between items-center">
        <span class="text-sm font-medium text-ink-muted">
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
