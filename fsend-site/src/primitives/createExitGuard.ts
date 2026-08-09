import { onCleanup, onMount } from "solid-js";
import { useBeforeLeave } from "@solidjs/router";

/**
 * Asks for confirmation before leaving while `isBlocking()` is true.
 *
 * Covers both ways out: `beforeunload` for closing/reloading the tab, and the
 * router's beforeLeave for in-app navigation — the header logo and footer
 * links are reachable from every page, and leaving a transfer route unmounts
 * the page, which aborts the transfer with no way back.
 *
 * Returns a wrapper for exits the user asked for explicitly (a Cancel button),
 * which should not prompt a second time.
 */
export function createExitGuard(isBlocking: () => boolean, message: string) {
  let bypass = false;

  useBeforeLeave((e) => {
    if (bypass) {
      bypass = false;
      return;
    }
    if (!isBlocking()) return;
    e.preventDefault();
    if (window.confirm(message)) e.retry(true);
  });

  const onBeforeUnload = (e: BeforeUnloadEvent) => {
    if (!isBlocking()) return;
    // Browsers show their own wording; both calls are needed for coverage.
    e.preventDefault();
    e.returnValue = "";
  };

  onMount(() => window.addEventListener("beforeunload", onBeforeUnload));
  onCleanup(() => window.removeEventListener("beforeunload", onBeforeUnload));

  return function withoutPrompt(leave: () => void) {
    bypass = true;
    leave();
  };
}
