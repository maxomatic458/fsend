import { Show } from "solid-js";
import { Button } from "./Button";

/** Spinner + label for every "waiting on the other side" state. */
export function Busy(props: { label: string; onCancel?: () => void }) {
  return (
    <div class="flex flex-col items-center gap-4 pt-10 text-center">
      <div class="animate-spin w-10 h-10 border-2 border-line border-t-azure rounded-full" />
      <p class="text-ink-muted">{props.label}</p>
      <Show when={props.onCancel}>
        <Button tone="ghost" size="sm" onClick={props.onCancel}>
          Cancel
        </Button>
      </Show>
    </div>
  );
}
