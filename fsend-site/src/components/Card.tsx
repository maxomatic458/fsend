import type { JSX } from "solid-js";

export function Card(props: { children: JSX.Element; class?: string }) {
  return (
    <div
      class={`bg-white dark:bg-neutral-800 rounded-lg shadow p-6 transition-colors ${props.class ?? ""}`}
    >
      {props.children}
    </div>
  );
}
