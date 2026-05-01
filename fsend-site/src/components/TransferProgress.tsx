import { For, Show } from "solid-js";
import { FiFolder, FiFile } from "solid-icons/fi";
import { formatBytes, formatSpeed, formatTime } from "../lib/format";
import type { ProgressStore } from "../primitives/createProgressTracker";

interface TransferProgressProps {
  progress: ProgressStore;
  status?: string;
  speedLabel?: string;
}

export function TransferProgress(props: TransferProgressProps) {
  const pct = () => {
    if (props.progress.totalSize === 0) return 0;
    return Math.min(
      100,
      (props.progress.totalTransferred / props.progress.totalSize) * 100,
    );
  };

  return (
    <div class="w-full">
      <div class="mb-6">
        <div class="flex justify-between items-center mb-2">
          <span class="text-lg font-semibold text-gray-800 dark:text-gray-100">
            Overall Progress
          </span>
          <span class="text-sm text-gray-600 dark:text-gray-400">
            {formatBytes(props.progress.totalTransferred)} /{" "}
            {formatBytes(props.progress.totalSize)}
          </span>
        </div>
        <div class="w-full bg-gray-200 dark:bg-neutral-700 rounded-full h-4">
          <div
            class="bg-blue-600 h-4 rounded-full transition-all duration-300"
            style={{ width: `${pct()}%` }}
          />
        </div>
        <div class="flex justify-between items-center mt-2">
          <div class="text-2xl font-bold text-blue-600 dark:text-blue-400">
            {Math.round(pct())}%
          </div>
          <Show when={props.progress.speed > 0}>
            <div class="text-right">
              <div class="text-sm font-medium text-gray-700 dark:text-gray-300">
                {props.speedLabel ?? "Speed"}:{" "}
                {formatSpeed(props.progress.speed)}
              </div>
              <Show
                when={props.progress.eta > 0 && props.progress.eta < Infinity}
              >
                <div class="text-sm text-gray-500 dark:text-gray-400">
                  ETA: {formatTime(props.progress.eta)}
                </div>
              </Show>
            </div>
          </Show>
        </div>
      </div>

      <Show when={props.status}>
        <div class="text-center mb-4 text-gray-600 dark:text-gray-400 capitalize">
          {props.status}
        </div>
      </Show>

      <div class="border dark:border-neutral-700 rounded-lg p-4 max-h-80 overflow-y-auto space-y-4">
        <For each={props.progress.entries}>
          {(entry) => {
            const entryPct = () =>
              entry.size === 0
                ? 100
                : Math.min(100, (entry.transferred / entry.size) * 100);
            const isComplete = () => entry.transferred >= entry.size;

            return (
              <div class="space-y-1">
                <div class="flex items-center justify-between">
                  <div class="flex items-center gap-2">
                    {entry.isDir ? (
                      <FiFolder class="w-5 h-5 text-gray-500 dark:text-gray-400" />
                    ) : (
                      <FiFile class="w-5 h-5 text-gray-500 dark:text-gray-400" />
                    )}
                    <span class="font-medium truncate max-w-xs text-gray-800 dark:text-gray-100">
                      {entry.name}
                    </span>
                  </div>
                  <span class="text-sm text-gray-600 dark:text-gray-400">
                    {formatBytes(entry.transferred)} / {formatBytes(entry.size)}
                  </span>
                </div>
                <div class="w-full bg-gray-200 dark:bg-neutral-700 rounded-full h-2">
                  <div
                    class={`h-2 rounded-full transition-all duration-300 ${
                      isComplete() ? "bg-green-500" : "bg-blue-400"
                    }`}
                    style={{ width: `${entryPct()}%` }}
                  />
                </div>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}
