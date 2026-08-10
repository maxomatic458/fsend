import { onMount, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiSend } from "solid-icons/fi";
import { SITE_URL } from "../lib/links";
import { pickFiles, pickDirectory, handleDrop } from "../lib/files/source";
import type { SelectedEntry } from "../lib/types";
import { createSendSession } from "../primitives/createSendSession";
import { createWindowDropTarget } from "../primitives/createWindowDropTarget";
import { createExitGuard } from "../primitives/createExitGuard";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { TransferHeader } from "../components/StepIndicator";
import { Logo } from "../components/Logo";
import { FileList } from "../components/FileList";
import { ShareCode } from "../components/ShareCode";
import { TransferProgress } from "../components/TransferProgress";
import { ErrorCard } from "../components/ErrorCard";
import { Button } from "../components/Button";

export function SendPage() {
  const navigate = useNavigate();
  const send = createSendSession();

  onMount(() => {
    const pending = (window as any).__fsend_pending as
      | SelectedEntry[]
      | undefined;
    if (pending?.length) {
      send.add(pending);
      delete (window as any).__fsend_pending;
    }
  });

  const dragActive = createWindowDropTarget(async (data) => {
    if (send.state() !== "selecting") return;
    const dropped = await handleDrop(data);
    if (dropped.length > 0) send.add(dropped);
  });
  const isDragging = () => dragActive() && send.state() === "selecting";

  const exitGuard = createExitGuard(send.isTransferring);

  const addFiles = async () => {
    try {
      const picked = await pickFiles();
      if (picked.length > 0) send.add(picked);
    } catch {}
  };

  const addFolder = async () => {
    try {
      send.add([await pickDirectory()]);
    } catch {}
  };

  const goBack = () => navigate("/");

  return (
    <div class="flex-1 bg-canvas py-8 px-4">
      <div
        class={`fixed inset-2 rounded-xl border pointer-events-none z-40 transition-opacity duration-150 ${
          isDragging() ? "opacity-100 border-flame/50" : "opacity-0 border-transparent"
        }`}
      />

      <ConfirmDialog
        open={exitGuard.isPrompting()}
        title="Leave while sending?"
        message="The transfer is still running. Leaving this page cancels it, and the receiver will have to start over."
        confirmLabel="Leave and cancel"
        cancelLabel="Keep sending"
        onConfirm={exitGuard.confirm}
        onCancel={exitGuard.cancel}
      />

      <Title>Send Files — fsend</Title>
      <Meta
        name="description"
        content="Drag and drop files or folders to send them directly to another device. No uploads to a server — the transfer is peer-to-peer over WebRTC, end-to-end encrypted."
      />
      <Link rel="canonical" href={`${SITE_URL}/send`} />

      <div class="max-w-2xl mx-auto flex flex-col gap-6">
        <TransferHeader
          title="Send files"
          steps={["Choose", "Share code", "Transfer"]}
          current={send.step()}
          accent="flame"
          onBack={goBack}
        />

        <Show when={send.state() === "selecting"}>
          <div class="flex flex-col items-center gap-3 pt-10 pb-2">
            <Show when={send.entries().length === 0}>
              <Logo
                tint="flame"
                class={`w-14 mb-1.5 transition-opacity duration-150 ${
                  isDragging() ? "opacity-100" : "opacity-80"
                }`}
              />
              <h2
                class={`text-2xl font-bold text-center transition-colors duration-150 ${
                  isDragging() ? "text-flame" : ""
                }`}
              >
                {isDragging()
                  ? "Drop to add them"
                  : "Drop files anywhere on this page"}
              </h2>
              <p class="text-[14.5px] text-ink-dim text-center">
                Any size · nothing is uploaded to a server
              </p>
            </Show>

            <Show when={send.entries().length > 0}>
              <div class="w-full max-w-[620px]">
                <FileList
                  entries={send.entries()}
                  sizes={send.entrySizes()}
                  onRemove={send.remove}
                  totalSize={send.selectionSize()}
                />
              </div>
              <p
                class={`text-sm transition-colors duration-150 ${
                  isDragging() ? "text-flame" : "text-ink-faint"
                }`}
              >
                {isDragging()
                  ? "Drop to add them"
                  : "or drop more files anywhere on this page"}
              </p>
            </Show>

            <div class="flex flex-wrap justify-center gap-2.5 mt-4">
              <button
                onClick={addFiles}
                class={`px-5 py-3 rounded-lg font-bold text-[15px] transition-colors cursor-pointer border ${
                  send.entries().length === 0
                    ? "border-orange-700 dark:border-orange-600 bg-orange-100 dark:bg-orange-900/70 text-orange-900 dark:text-orange-50 hover:bg-orange-200 dark:hover:bg-orange-800/70"
                    : "border-line text-ink hover:bg-surface-2"
                }`}
              >
                Browse files
              </button>
              <button
                onClick={addFolder}
                class="px-5 py-3 rounded-lg border border-line text-ink font-semibold text-[15px] hover:bg-surface-2 transition-colors cursor-pointer"
              >
                Browse folder
              </button>
            </div>

            <Show when={send.entries().length > 0}>
              <button
                onClick={send.start}
                class="w-full max-w-[620px] mt-2 py-4 rounded-lg border border-orange-700 dark:border-orange-600 bg-orange-100 dark:bg-orange-900/70 text-orange-900 dark:text-orange-50 font-bold text-base hover:bg-orange-200 dark:hover:bg-orange-800/70 transition-colors cursor-pointer"
              >
                <span class="flex items-center justify-center gap-2">
                  <FiSend class="w-5 h-5" />
                  Generate share code
                </span>
              </button>
            </Show>
          </div>
        </Show>

        <Show when={send.state() === "connecting"}>
          <Busy label="Creating session..." onCancel={goBack} />
        </Show>

        <Show when={send.state() === "waiting"}>
          <ShareCode
            code={send.shareCode()}
            expiresAt={send.expiresAt()}
            onCancel={send.cancel}
          />
        </Show>

        <Show when={send.state() === "handshaking"}>
          <Busy label="Establishing connection..." />
        </Show>

        <Show when={send.state() === "waitingAccept"}>
          <Busy label="Waiting for receiver to accept..." />
        </Show>

        <Show
          when={send.state() === "transferring" || send.state() === "completed"}
        >
          <TransferProgress
            progress={send.progress}
            verb="sent"
            connection={send.connection()}
            hint={
              send.state() === "transferring"
                ? "Keep this tab open until the transfer finishes."
                : undefined
            }
          />

          <Show when={send.state() === "completed"}>
            <div class="text-center">
              <p class="text-green-600 dark:text-green-400 font-semibold text-lg mb-4">
                All files sent successfully!
              </p>
              <Button variant="blue" onClick={goBack}>
                Back to Home
              </Button>
            </div>
          </Show>
        </Show>

        <Show when={send.state() === "error"}>
          <ErrorCard class="text-center">
            <p class="text-red-600 dark:text-red-400 font-semibold mb-4">
              {send.error()}
            </p>
            <Button variant="red" onClick={send.reset}>
              Try Again
            </Button>
          </ErrorCard>
        </Show>
      </div>
    </div>
  );
}

/** Spinner + label for every "waiting on the other side" state. */
function Busy(props: { label: string; onCancel?: () => void }) {
  return (
    <div class="flex flex-col items-center gap-4 pt-10 text-center">
      <div class="animate-spin w-10 h-10 border-2 border-line border-t-azure rounded-full" />
      <p class="text-ink-muted">{props.label}</p>
      <Show when={props.onCancel}>
        <button
          onClick={props.onCancel!}
          class="px-5 py-2.5 rounded-lg border border-line text-ink-muted font-semibold text-sm hover:bg-surface-2 hover:text-ink transition-colors cursor-pointer"
        >
          Cancel
        </button>
      </Show>
    </div>
  );
}
