import { For } from "solid-js";

function StepIndicator(props: {
  steps: string[];
  // Zero-based index of the step in progress.
  current: number;
}) {
  return (
    <ol class="flex items-center gap-2.5 font-mono text-[11.5px] text-ink-faint">
      <For each={props.steps}>
        {(label, i) => (
          <>
            {i() > 0 && <li aria-hidden="true" class="w-6 h-px bg-line" />}
            <li
              class={i() === props.current ? "text-accent" : undefined}
              aria-current={i() === props.current ? "step" : undefined}
            >
              {String(i() + 1).padStart(2, "0")} {label}
            </li>
          </>
        )}
      </For>
    </ol>
  );
}

export function TransferHeader(props: {
  title: string;
  steps: string[];
  current: number;
  onBack: () => void;
}) {
  return (
    <div class="flex flex-col items-center gap-4.5">
      <button
        onClick={props.onBack}
        class="self-start text-[13px] text-azure hover:text-azure-hi transition-colors cursor-pointer"
      >
        ← Back
      </button>
      <div class="flex flex-col items-center gap-3">
        <h1 class="text-3xl font-bold tracking-[-0.02em]">{props.title}</h1>
        <StepIndicator steps={props.steps} current={props.current} />
      </div>
    </div>
  );
}
