import { For, Show, type JSX } from "solid-js";
import { FiFolder, FiFile } from "solid-icons/fi";
import { formatBytes, formatSpeed, formatTime } from "../lib/format";
import { summarize } from "../lib/stats";
import type { ProgressStore } from "../primitives/createProgressTracker";
import type { ConnectionKind } from "../lib/transfer/events";

interface TransferProgressProps {
  progress: ProgressStore;
  /// "received" / "sent" — used in the byte counter beneath the bar
  verb: string;
  connection?: ConnectionKind;
  /// Sits opposite the actions, as a fact about the transfer
  hint?: string;
  actions?: JSX.Element;
}

export function TransferProgress(props: TransferProgressProps) {
  /** Once the clock has stopped, the live speed and ETA give way to totals. */
  const finished = () => props.progress.endTimeMs > 0;
  const summary = () => summarize(props.progress);

  const pct = () =>
    props.progress.totalSizeBytes === 0
      ? 0
      : Math.min(
          100,
          (props.progress.totalTransferredBytes /
            props.progress.totalSizeBytes) *
            100,
        );

  return (
    <div class="w-full max-w-[620px] mx-auto flex flex-col gap-4 pt-5">
      <div class="flex items-baseline justify-between gap-4">
        <div class="text-[34px] font-bold tracking-[-0.02em] leading-none">
          {Math.round(pct())}
          <span class="text-[19px] text-ink-dim">%</span>
        </div>
        <div class="text-sm text-ink-dim text-right leading-normal">
          <Show
            when={finished()}
            fallback={
              <>
                <Show when={props.progress.speedBytesPerSec > 0}>
                  <div>
                    {formatSpeed(props.progress.speedBytesPerSec)}
                    <Show
                      when={props.connection && props.connection !== "unknown"}
                    >
                      {" "}
                      · {props.connection}
                    </Show>
                  </div>
                </Show>
                <Show
                  when={
                    props.progress.etaSecs > 0 &&
                    props.progress.etaSecs < Infinity
                  }
                >
                  <div>{formatTime(props.progress.etaSecs)} remaining</div>
                </Show>
              </>
            }
          >
            <Show when={summary().averageBytesPerSec > 0}>
              <div>
                {formatSpeed(summary().averageBytesPerSec)} average
                <Show when={props.connection && props.connection !== "unknown"}>
                  {" "}
                  · {props.connection}
                </Show>
              </div>
            </Show>
            <div>{formatTime(summary().elapsedSecs)} total</div>
          </Show>
        </div>
      </div>

      <div class="h-2 rounded bg-track overflow-hidden">
        <div
          class="h-full bg-gradient-to-r from-azure/40 to-azure transition-[width] duration-200"
          style={{ width: `${pct()}%` }}
        />
      </div>

      <div class="flex justify-between font-mono text-xs text-ink-faint">
        <span>
          {formatBytes(props.progress.totalTransferredBytes)} {props.verb}
        </span>
        <span>{formatBytes(props.progress.totalSizeBytes)} total</span>
      </div>

      <div class="flex flex-col gap-2.5 border-t border-line pt-4 mt-1.5 max-h-64 overflow-y-auto">
        <For each={props.progress.entries}>
          {(entry) => {
            const entryPct = () =>
              entry.sizeBytes === 0
                ? 100
                : Math.min(
                    100,
                    (entry.transferredBytes / entry.sizeBytes) * 100,
                  );
            return (
              <div class="flex flex-col gap-2">
                <div class="flex items-center justify-between gap-4">
                  <div class="flex items-center gap-3 min-w-0">
                    {entry.isDir ? (
                      <FiFolder class="w-4 h-4 text-ink-faint shrink-0" />
                    ) : (
                      <FiFile class="w-4 h-4 text-ink-faint shrink-0" />
                    )}
                    <span class="text-[15px] truncate">{entry.name}</span>
                  </div>
                  <span class="font-mono text-[12.5px] text-ink-dim shrink-0">
                    {formatBytes(entry.transferredBytes)} /{" "}
                    {formatBytes(entry.sizeBytes)}
                  </span>
                </div>
                <div class="h-[3px] rounded-sm bg-track overflow-hidden">
                  <div
                    class={`h-full ${
                      entry.transferredBytes >= entry.sizeBytes
                        ? "bg-ok-bar"
                        : "bg-azure"
                    }`}
                    style={{ width: `${entryPct()}%` }}
                  />
                </div>
              </div>
            );
          }}
        </For>
      </div>

      <Show when={props.hint || props.actions}>
        <div class="flex items-center justify-between gap-4 pt-1.5">
          <p class="text-[13px] text-ink-faint">{props.hint}</p>
          <div class="flex gap-2.5 shrink-0">{props.actions}</div>
        </div>
      </Show>
    </div>
  );
}
