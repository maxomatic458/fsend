import { For } from "solid-js";
import { Dynamic } from "solid-js/web";
import type { IconTypes } from "solid-icons";

interface ChoiceOption<T> {
    value: T;
    label: string;
    icon: IconTypes;
}

interface ChoiceProps<T> {
    /// Names the group for screen readers.
    label: string;
    options: ChoiceOption<T>[];
    value: T;
    onChange: (value: T) => void;
}

/// Radio buttons drawn as a row of buttons; the selected one takes the page's accent.
export function Choice<T extends string>(props: ChoiceProps<T>) {
    return (
        <div role="radiogroup" aria-label={props.label} class="grid grid-cols-2 gap-2.5">
            <For each={props.options}>
                {(option) => (
                    <label
                        class={`relative flex items-center justify-center gap-2 px-4 py-3 rounded-lg border text-sm font-semibold text-center cursor-pointer transition-colors has-focus-visible:outline-2 has-focus-visible:outline-offset-2 has-focus-visible:outline-accent ${
                            props.value === option.value
                                ? "bg-accent-soft/40 border-accent-line text-accent-ink"
                                : "border-line text-ink-muted hover:bg-surface-2 hover:text-ink"
                        }`}
                    >
                        <input
                            type="radio"
                            name={props.label}
                            // Invisible, but over the whole option, so it is what gets clicked.
                            class="absolute inset-0 m-0 opacity-0 cursor-pointer"
                            checked={props.value === option.value}
                            onChange={() => props.onChange(option.value)}
                        />
                        <Dynamic
                            component={option.icon}
                            class="w-4 h-4 shrink-0"
                            aria-hidden="true"
                        />
                        {option.label}
                    </label>
                )}
            </For>
        </div>
    );
}
