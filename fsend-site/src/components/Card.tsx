import type { JSX } from "solid-js";

export function Card(props: { children: JSX.Element; class?: string }) {
  return (
    <div
      class={`bg-surface border border-line rounded-xl p-6 ${props.class ?? ""}`}
    >
      {props.children}
    </div>
  );
}
