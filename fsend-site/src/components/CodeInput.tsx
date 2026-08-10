import { createSignal, For } from "solid-js";

const LENGTH = 8;

export function CodeInput(props: {
  value: string;
  onChange: (value: string) => void;
  // Fired when the code reaches full length
  onComplete?: () => void;
}) {
  const [focused, setFocused] = createSignal(false);
  const chars = () => props.value.padEnd(LENGTH, " ").slice(0, LENGTH).split("");
  const caret = () => Math.min(props.value.length, LENGTH - 1);

  const set = (raw: string) => {
    // Accepts a bare code or a full share link — pasting either works.
    const fromLink = raw.match(/\/receive\/([A-Za-z0-9]{1,8})/);
    const cleaned = (fromLink ? fromLink[1] : raw)
      .toUpperCase()
      .replace(/[^A-Z0-9]/g, "")
      .slice(0, LENGTH);
    props.onChange(cleaned);
    if (cleaned.length === LENGTH) props.onComplete?.();
  };

  return (
    <div class="relative w-full">
      <input
        type="text"
        inputMode="text"
        autocomplete="one-time-code"
        autocapitalize="characters"
        spellcheck={false}
        aria-label="Share code"
        value={props.value}
        onInput={(e) => set(e.currentTarget.value)}
        onPaste={(e) => {
          e.preventDefault();
          set(e.clipboardData?.getData("text") ?? "");
        }}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        class="absolute inset-0 w-full h-full opacity-0 cursor-text"
      />
      <div class="flex justify-center gap-2 pointer-events-none">
        <For each={chars()}>
          {(char, i) => {
            const active = () =>
              focused() && i() === caret() && props.value.length < LENGTH;
            return (
              <div
                class={`w-[46px] h-[58px] sm:w-[52px] sm:h-16 rounded-[9px] border grid place-items-center font-mono text-2xl sm:text-[26px] transition-colors ${
                  active()
                    ? "border-azure text-ink"
                    : char.trim()
                      ? "border-line text-ink"
                      : "border-line text-transparent"
                } bg-surface-2`}
              >
                {char.trim() || " "}
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}
