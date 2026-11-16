import { For, Show } from "solid-js";
import { FiFolder, FiFile } from "solid-icons/fi";
import { formatBytes } from "../lib/format";
import { Button } from "./Button";
import type { TopEntry } from "../lib/proto";

interface FileOfferProps {
  files: TopEntry[];
  /// True when the browser has no File System Access API
  inMemory?: boolean;
  /// Accepted, so the choice is made.
  busy?: boolean;
  onAccept: () => void;
  onReject: () => void;
}

export function FileOffer(props: FileOfferProps) {
  const count = () => props.files.length;
  const dirs = () => props.files.filter((f) => f.dir).length;
  const totalBytes = () => props.files.reduce((sum, f) => sum + f.size, 0);

  const headline = () => {
    const n = count();
    if (dirs() === n && n > 0) {
      return `The sender wants to share ${n} folder${n === 1 ? "" : "s"}`;
    }
    if (dirs() > 0) return `The sender wants to share ${n} items`;
    return `The sender wants to share ${n} file${n === 1 ? "" : "s"}`;
  };

  const summary = () => {
    const n = count();
    if (dirs() === n && n > 0) return `${n} folder${n === 1 ? "" : "s"}`;
    if (dirs() > 0) return `${n} items`;
    return `${n} file${n === 1 ? "" : "s"}`;
  };

  return (
    <div class="w-full max-w-[620px] mx-auto flex flex-col gap-4.5 pt-5">
      <div class="flex flex-col items-center gap-1.5 text-center">
        <h2 class="text-xl font-bold">{headline()}</h2>
        <p class="text-sm text-ink-dim">
          Accepting starts the transfer straight away.
        </p>
      </div>

      <div class="flex flex-col border-t border-line max-h-80 overflow-y-auto">
        <For each={props.files}>
          {(entry) => (
            <div class="flex items-center justify-between gap-4 py-3.5 px-1 border-b border-line">
              <div class="flex items-center gap-3 min-w-0">
                {entry.dir ? (
                  <FiFolder class="w-4 h-4 text-ink-faint shrink-0" />
                ) : (
                  <FiFile class="w-4 h-4 text-ink-faint shrink-0" />
                )}
                <span class="text-[15.5px] truncate">{entry.name}</span>
              </div>
              <span class="font-mono text-[13px] text-ink-dim shrink-0">
                {formatBytes(entry.size)}
              </span>
            </div>
          )}
        </For>
        <div class="flex items-center justify-between py-3 px-1 font-mono text-xs text-ink-faint">
          <span>{summary()}</span>
          <span>total {formatBytes(totalBytes())}</span>
        </div>
      </div>

      <Show when={props.inMemory}>
        <div class="bg-warn-bg text-warn-ink border border-warn-line rounded-lg px-4 py-3 text-sm leading-relaxed">
          Ensure you have {formatBytes(totalBytes())} of free system memory —
          this browser holds the whole transfer in memory until it is saved.
        </div>
      </Show>

      <div class="flex gap-2.5">
        <Button
          tone="accent"
          size="lg"
          class="flex-1"
          disabled={props.busy}
          onClick={props.onAccept}
        >
          Accept &amp; Receive
        </Button>
        <Button
          tone="ghost"
          size="lg"
          disabled={props.busy}
          onClick={props.onReject}
        >
          Reject
        </Button>
      </div>
    </div>
  );
}
