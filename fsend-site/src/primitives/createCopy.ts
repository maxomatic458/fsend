import { createSignal, onCleanup } from "solid-js";

/** Copies text to the clipboard; `copied` stays true for `ms` after each copy. */
export function createCopy(ms: number) {
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Refused, or no clipboard outside a secure context.
      return;
    }
    setCopied(true);
    clearTimeout(timer);
    timer = setTimeout(() => setCopied(false), ms);
  };
  return { copied, copy };
}
