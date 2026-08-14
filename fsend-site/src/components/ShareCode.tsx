import { createSignal, createEffect, onCleanup, Show } from "solid-js";
import { FiCopy, FiCheck } from "solid-icons/fi";
import qrcode from "qrcode-generator";
import { Button } from "./Button";

interface ShareCodeProps {
  code: string;
  expiresAtMs: number;
  onCancel?: () => void;
}

export function ShareCode(props: ShareCodeProps) {
  const [copied, setCopied] = createSignal(false);
  const [showQr, setShowQr] = createSignal(false);
  const [secondsLeft, setSecondsLeft] = createSignal(0);

  // Built from the current origin so a link copied while running locally
  // points at the local server rather than production.
  const shareUrl = () => `${window.location.origin}/receive/${props.code}`;

  const qrSvg = () => {
    const qr = qrcode(0, "M");
    qr.addData(shareUrl());
    qr.make();
    return qr.createSvgTag({ cellSize: 4, margin: 2 });
  };

  createEffect(() => {
    const update = () =>
      setSecondsLeft(
        Math.max(0, Math.floor((props.expiresAtMs - Date.now()) / 1000)),
      );
    update();
    const id = setInterval(update, 1000);
    onCleanup(() => clearInterval(id));
  });

  const expiry = () => {
    const s = secondsLeft();
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  };

  const copy = async () => {
    await navigator.clipboard.writeText(shareUrl());
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div class="w-full max-w-[620px] mx-auto flex flex-col gap-4 pt-5">
      <div class="flex flex-col items-center gap-2">
        <p class="text-sm text-ink-dim">Give this code to the receiver</p>
        <div class="font-mono text-4xl sm:text-[52px] font-medium tracking-[0.14em] text-accent leading-tight break-all text-center">
          {props.code}
        </div>
      </div>

      <div class="flex gap-2.5">
        <div class="flex-1 min-w-0 flex items-center px-4 py-3 border border-line rounded-lg bg-surface-2 font-mono text-[13.5px] text-ink-muted overflow-hidden text-ellipsis whitespace-nowrap">
          {shareUrl()}
        </div>
        <Button tone="accent" onClick={copy} class="whitespace-nowrap">
          <span class="flex items-center gap-2">
            {copied() ? (
              <FiCheck class="w-4 h-4" />
            ) : (
              <FiCopy class="w-4 h-4" />
            )}
            {/* Both labels share one grid cell, so the button stays as wide as
                the longer of the two and does not resize when it flips. */}
            <span class="grid justify-items-center">
              <span
                class={`col-start-1 row-start-1 ${copied() ? "invisible" : ""}`}
              >
                Copy link
              </span>
              <span
                class={`col-start-1 row-start-1 ${copied() ? "" : "invisible"}`}
              >
                Copied!
              </span>
            </span>
          </span>
        </Button>
      </div>

      <div class="flex items-center justify-between text-[13.5px] text-ink-faint px-0.5">
        <button
          onClick={() => setShowQr(!showQr())}
          class="border-b border-line pb-0.5 hover:text-ink-muted transition-colors cursor-pointer"
          aria-expanded={showQr()}
        >
          {showQr() ? "Hide QR code" : "Show QR code"}
        </button>
        <span
          class={`font-mono text-[12.5px] ${
            secondsLeft() <= 60 ? "text-warn-ink" : ""
          }`}
        >
          expires in {expiry()}
        </span>
      </div>

      <Show when={showQr()}>
        <div class="flex justify-center">
          <div class="p-3 bg-white rounded-lg" innerHTML={qrSvg()} />
        </div>
      </Show>

      <div class="flex items-center justify-between gap-4 border-t border-line pt-4 mt-1.5">
        <div class="flex items-center gap-2.5 text-sm text-ink-muted">
          <span class="w-2 h-2 rounded-full bg-accent animate-pulse" />
          Waiting for the receiver to connect
        </div>
        <Show when={props.onCancel}>
          <Button tone="ghost" size="sm" onClick={props.onCancel}>
            Cancel
          </Button>
        </Show>
      </div>
    </div>
  );
}
