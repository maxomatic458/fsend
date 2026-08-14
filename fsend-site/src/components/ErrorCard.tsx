import { Show } from "solid-js";
import { Button } from "./Button";

export function ErrorCard(props: {
  message: string;
  onRetry?: () => void;
  class?: string;
}) {
  return (
    <div
      class={`bg-danger-panel border border-danger-panel-line rounded-lg p-6 transition-colors ${props.class ?? ""}`}
    >
      <p class="text-danger font-semibold mb-4">{props.message}</p>
      <Show when={props.onRetry}>
        <Button tone="danger" onClick={props.onRetry}>
          Try Again
        </Button>
      </Show>
    </div>
  );
}
