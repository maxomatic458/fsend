import { createSignal, Index } from "solid-js";
import { CODE_LENGTH } from "../lib/proto";

export function CodeInput(props: {
  value: string;
  onInput: (code: string) => void;
  // Fired when the code reaches full length
  onComplete?: () => void;
}) {
  const [focused, setFocused] = createSignal(false);
  const chars = () => props.value.padEnd(CODE_LENGTH, " ").split("");
  const caret = () => Math.min(props.value.length, CODE_LENGTH - 1);

  const set = (code: string) => {
    props.onInput(code);
    if (code.length === CODE_LENGTH) props.onComplete?.();
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
        maxLength={CODE_LENGTH}
        value={props.value}
        onInput={(e) => set(e.currentTarget.value)}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        class="absolute inset-0 w-full h-full opacity-0 cursor-text"
      />
      <div class="flex justify-center gap-1.5 sm:gap-2 pointer-events-none">
        {/* Index, not For: each box stays put, so its colour can transition. */}
        <Index each={chars()}>
          {(char, i) => {
            const active = () =>
              focused() && i === caret() && props.value.length < CODE_LENGTH;
            return (
              <div
                class={`flex-1 min-w-0 max-w-[48px] h-[52px] sm:h-[60px] rounded-[9px] border grid place-items-center font-mono text-xl sm:text-2xl transition-colors ${
                  active()
                    ? "border-azure text-ink"
                    : char().trim()
                      ? "border-line text-ink"
                      : "border-line text-transparent"
                } bg-surface-2`}
              >
                {char().trim() || " "}
              </div>
            );
          }}
        </Index>
      </div>
    </div>
  );
}
