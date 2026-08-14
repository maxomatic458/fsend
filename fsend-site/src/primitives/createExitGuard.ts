import { createSignal, onCleanup, onMount } from "solid-js";
import { useBeforeLeave } from "@solidjs/router";

export interface ExitGuard {
  isPrompting: () => boolean;
  confirm: () => void;
  cancel: () => void;
  withoutPrompt: (leave: () => void) => void;
}

export function createExitGuard(isBlocking: () => boolean) {
  const [pendingRetry, setPendingRetry] = createSignal<(() => void) | null>(
    null,
  );
  let bypass = false;

  useBeforeLeave((e) => {
    if (bypass) {
      bypass = false;
      return;
    }
    if (!isBlocking()) return;
    e.preventDefault();
    setPendingRetry(() => () => e.retry(true));
  });

  const onBeforeUnload = (event: BeforeUnloadEvent) => {
    if (!isBlocking()) return;
    event.preventDefault();
    event.returnValue = "";
  };

  onMount(() => window.addEventListener("beforeunload", onBeforeUnload));
  onCleanup(() => window.removeEventListener("beforeunload", onBeforeUnload));

  return {
    isPrompting: () => pendingRetry() !== null,
    confirm: () => {
      const retry = pendingRetry();
      setPendingRetry(null);
      retry?.();
    },
    cancel: () => setPendingRetry(null),
    withoutPrompt: (leave: () => void) => {
      bypass = true;
      leave();
    },
  } satisfies ExitGuard;
}
