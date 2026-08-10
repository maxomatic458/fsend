/**
 * The fsend mark: two right-aligned bars, each fading in from transparent,
 * sheared 20deg. Mirrors public/logo.svg — keep the two in sync.
 *
 * The viewBox starts at -5.824 (= -16 * tan 20deg) so the shear stays inside
 * the box. Aspect ratio is 31.824 : 16, so sizing the width is enough.
 */
let uid = 0;

const FLAME = "#e6570f";
const AZURE = "#4d84ff";

export function Logo(props: {
  class?: string;
  tint?: "brand" | "flame" | "azure";
}) {
  const id = `fsend-mark-${uid++}`;
  const top = () => (props.tint === "azure" ? AZURE : FLAME);
  const bottom = () =>
    props.tint === "flame" ? FLAME : props.tint === "azure" ? AZURE : AZURE;

  return (
    <svg viewBox="-5.824 0 31.824 16" class={props.class} aria-hidden="true">
      <defs>
        <linearGradient id={`${id}-top`} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stop-color={top()} stop-opacity="0" />
          <stop offset="1" stop-color={top()} />
        </linearGradient>
        <linearGradient id={`${id}-bottom`} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stop-color={bottom()} stop-opacity="0" />
          <stop offset="1" stop-color={bottom()} />
        </linearGradient>
      </defs>
      <g transform="skewX(-20)">
        <rect x="9" y="0" width="17" height="8" fill={`url(#${id}-top)`} />
        <rect x="0" y="8" width="26" height="8" fill={`url(#${id}-bottom)`} />
      </g>
    </svg>
  );
}
