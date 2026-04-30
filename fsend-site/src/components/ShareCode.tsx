import { createSignal, createEffect, onCleanup, Show } from 'solid-js';
import { FiCopy, FiCheck, FiClock } from 'solid-icons/fi';
import qrcode from 'qrcode-generator';
import { Card } from './Card';
import { Button } from './Button';

interface ShareCodeProps {
  code: string;
  expiresAt: number;
  onCancel?: () => void;
}

export function ShareCode(props: ShareCodeProps) {
  const [copied, setCopied] = createSignal(false);
  const [timeLeft, setTimeLeft] = createSignal(0);

  const shareUrl = () => `${window.location.origin}/receive/${props.code}`;

  const qrSvg = () => {
    const qr = qrcode(0, 'M');
    qr.addData(shareUrl());
    qr.make();
    return qr.createSvgTag({ cellSize: 4, margin: 2 });
  };

  createEffect(() => {
    const update = () => {
      const remaining = Math.max(0, Math.floor((props.expiresAt - Date.now()) / 1000));
      setTimeLeft(remaining);
    };
    update();
    const id = setInterval(update, 1000);
    onCleanup(() => clearInterval(id));
  });

  const copyUrl = async () => {
    await navigator.clipboard.writeText(shareUrl());
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <Card class="mb-6">
      <h2 class="text-xl font-semibold mb-4 text-gray-800 dark:text-gray-100">Share this code</h2>

      <div class="text-center mb-6">
        <div class="text-4xl font-bold tracking-widest text-blue-600 dark:text-blue-400 mb-2">
          {props.code}
        </div>
        <p class="text-sm text-gray-500 dark:text-gray-400">Share this code with the receiver</p>
      </div>

      <div class="mb-6">
        <label class="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
          Or share this link
        </label>
        <div class="flex gap-2">
          <input
            type="text"
            value={shareUrl()}
            readOnly
            class="flex-1 p-3 border dark:border-neutral-600 rounded-lg bg-gray-50 dark:bg-neutral-700 text-sm text-gray-800 dark:text-gray-200"
          />
          <Button variant="blue" onClick={copyUrl}>
            <span class="flex items-center gap-2">
              {copied() ? <FiCheck class="w-4 h-4" /> : <FiCopy class="w-4 h-4" />}
              {copied() ? 'Copied!' : 'Copy'}
            </span>
          </Button>
        </div>
      </div>

      <Show when={true}>
        <div class="flex justify-center mb-6">
          <div class="p-3 bg-white rounded-lg" innerHTML={qrSvg()} />
        </div>
      </Show>

      <div class="flex items-center justify-center gap-2 mb-4">
        <FiClock
          class={`w-4 h-4 ${
            timeLeft() <= 60 ? 'text-red-500' : 'text-gray-500 dark:text-gray-400'
          }`}
        />
        <span
          class={`text-sm font-medium ${
            timeLeft() <= 60 ? 'text-red-500' : 'text-gray-500 dark:text-gray-400'
          }`}
        >
          Code expires in {Math.floor(timeLeft() / 60)}:{String(timeLeft() % 60).padStart(2, '0')}
        </span>
      </div>

      <div class="flex items-center justify-center gap-3 text-gray-500 dark:text-gray-400 mb-6">
        <div class="animate-pulse w-3 h-3 bg-yellow-500 rounded-full" />
        <span>Waiting for receiver to connect...</span>
      </div>

      <Show when={props.onCancel}>
        <div class="text-center">
          <Button variant="gray" onClick={props.onCancel}>
            Cancel
          </Button>
        </div>
      </Show>
    </Card>
  );
}
