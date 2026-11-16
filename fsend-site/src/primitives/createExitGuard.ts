import { createSignal, onCleanup, onMount } from "solid-js";
import { useBeforeLeave } from "@solidjs/router";

interface ExitGuard {
    isPrompting: () => boolean;
    confirm: () => void;
    cancel: () => void;
    withoutPrompt: (leave: () => void) => void;
}

export function createExitGuard(isBlocking: () => boolean) {
    const [pendingRetry, setPendingRetry] = createSignal<(() => void) | null>(null);
    let bypass = false;

    useBeforeLeave((e) => {
        if (bypass) {
            bypass = false;
            return;
        }
        if (!isBlocking()) return;
        // Staying on this page with a new fragment, such as another share link,
        // is the page's own business. Back and forward give only a step count;
        // the browser is already at the new entry by then.
        const to =
            typeof e.to === "string"
                ? new URL(e.to, window.location.href).pathname
                : window.location.pathname;
        if (to === e.from.pathname) return;
        e.preventDefault();
        setPendingRetry(() => () => e.retry(true));
    });

    const onBeforeUnload = (event: BeforeUnloadEvent) => {
        if (!isBlocking()) return;
        event.preventDefault();
        event.returnValue = "";
    };

    // Paired inside onMount so neither runs during the build-time prerender.
    onMount(() => {
        window.addEventListener("beforeunload", onBeforeUnload);
        onCleanup(() => window.removeEventListener("beforeunload", onBeforeUnload));
    });

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
