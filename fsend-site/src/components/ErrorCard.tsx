import type { JSX } from "solid-js";
import { Button } from "./Button";

export function ErrorCard(props: {
  message?: string;
  onRetry?: () => void;
  class?: string;
  children?: JSX.Element;
}) {
  return (
    <div
      class={`bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg p-6 transition-colors ${props.class ?? ""}`}
    >
      {props.children ?? (
        <p class="text-red-600 dark:text-red-400 font-semibold mb-4">
          {props.message}
        </p>
      )}
      {props.onRetry && !props.children && (
        <Button variant="red" onClick={props.onRetry}>
          Try Again
        </Button>
      )}
    </div>
  );
}
