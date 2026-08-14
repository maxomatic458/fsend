import { Show, createEffect, onCleanup } from "solid-js";
import { Button } from "./Button";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog(props: ConfirmDialogProps) {
  let cancelButton: HTMLButtonElement | undefined;

  createEffect(() => {
    if (!props.open) return;

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onCancel();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));

    // Move focus into the dialog so Escape and Tab behave as expected.
    cancelButton?.focus();
  });

  return (
    <Show when={props.open}>
      <div
        class="fixed inset-0 z-[60] flex items-center justify-center p-4 bg-black/50"
        onClick={props.onCancel}
      >
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="confirm-title"
          class="bg-surface border border-line rounded-xl p-6 max-w-md w-full shadow-xl"
          onClick={(e) => e.stopPropagation()}
        >
          <h2 id="confirm-title" class="text-lg font-bold text-ink mb-2">
            {props.title}
          </h2>
          <p class="text-ink-muted text-[15px] leading-relaxed mb-6">
            {props.message}
          </p>
          <div class="flex justify-end gap-3">
            <Button
              ref={(el) => (cancelButton = el)}
              tone="ghost"
              size="sm"
              onClick={props.onCancel}
            >
              {props.cancelLabel}
            </Button>
            <Button tone="danger" size="sm" onClick={props.onConfirm}>
              {props.confirmLabel}
            </Button>
          </div>
        </div>
      </div>
    </Show>
  );
}
