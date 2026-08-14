import type { JSX } from "solid-js";

/**
 * `accent` follows whichever .accent-* class the page container carries, so the
 * same tone renders flame on /send and azure on /receive.
 */
export type ButtonTone = "accent" | "neutral" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

const tones: Record<ButtonTone, string> = {
  accent:
    "font-bold border-accent-line bg-accent-soft text-accent-ink hover:bg-accent-soft-hi",
  neutral: "font-semibold border-line text-ink hover:bg-surface-2",
  ghost:
    "font-semibold border-line text-ink-muted hover:bg-surface-2 hover:text-ink",
  danger:
    "font-semibold border-danger-line bg-danger-soft text-danger-ink hover:bg-danger-soft-hi",
};

const sizes: Record<ButtonSize, string> = {
  sm: "px-5 py-2.5 text-sm",
  md: "px-5 py-3 text-[15px]",
  lg: "px-6 py-4 text-base",
};

interface ButtonProps {
  tone?: ButtonTone;
  size?: ButtonSize;
  onClick?: () => void;
  disabled?: boolean;
  class?: string;
  "aria-expanded"?: boolean;
  ref?: (el: HTMLButtonElement) => void;
  children: JSX.Element;
}

export function Button(props: ButtonProps) {
  return (
    <button
      ref={(el) => props.ref?.(el)}
      onClick={props.onClick}
      disabled={props.disabled}
      aria-expanded={props["aria-expanded"]}
      class={`rounded-lg border transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed ${
        tones[props.tone ?? "neutral"]
      } ${sizes[props.size ?? "md"]} ${props.class ?? ""}`}
    >
      {props.children}
    </button>
  );
}
