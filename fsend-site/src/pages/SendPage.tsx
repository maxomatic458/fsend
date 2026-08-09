import { onMount, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiArrowLeft, FiSend, FiFile, FiFolder } from "solid-icons/fi";
import { SITE_URL } from "../lib/links";
import { pickFiles, pickDirectory, handleDrop } from "../lib/files/source";
import type { SelectedEntry } from "../lib/types";
import { createSendSession } from "../primitives/createSendSession";
import { createWindowDropTarget } from "../primitives/createWindowDropTarget";
import { createExitGuard } from "../primitives/createExitGuard";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FileList } from "../components/FileList";
import { ShareCode } from "../components/ShareCode";
import { TransferProgress } from "../components/TransferProgress";
import { ErrorCard } from "../components/ErrorCard";
import { Button } from "../components/Button";
import { Card } from "../components/Card";
import { formatBytes } from "../lib/format";

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

      <div class="max-w-2xl mx-auto">
        <button
          onClick={goBack}
          class="mb-6 text-azure hover:text-azure-hi flex items-center gap-2 transition-colors cursor-pointer"
        >
          <FiArrowLeft class="w-4 h-4" /> Back
        </button>
        <h1 class="text-3xl font-bold text-ink mb-8">Send Files</h1>

        <Show when={send.state() === "selecting"}>
          <Card class="mb-6">
            <h2 class="text-xl font-semibold mb-4 text-ink">
              Select Files or Directories
            </h2>

            <Show when={send.entries().length > 0}>
              <FileList
                entries={send.entries()}
                onRemove={send.remove}
                totalSize={send.selectionSize()}
              />
              <hr class="border-line mb-6" />
            </Show>

            <div
              class={`border-2 border-dashed rounded-lg p-8 mb-6 text-center transition-colors duration-150 ${
                isDragging() ? "border-azure bg-azure/5" : "border-line"
              }`}
            >
              <div
                class={`transition-colors duration-150 ${
                  isDragging() ? "text-azure" : "text-ink-dim"
                }`}
              >
                <FiFolder class="w-10 h-10 mx-auto mb-2" />
                <div>
                  {isDragging()
                    ? "Drop to add"
                    : "Drag and drop files or folders"}
                </div>
              </div>
            </div>

            <div class="flex gap-4">
              <Button variant="blue" onClick={addFiles} class="flex-1 py-3">
                <span class="flex items-center justify-center gap-2">
                  <FiFile class="w-5 h-5" />
                  Add Files
                </span>
              </Button>
              <Button variant="green" onClick={addFolder} class="flex-1 py-3">
                <span class="flex items-center justify-center gap-2">
                  <FiFolder class="w-5 h-5" />
                  Add Folder
                </span>
              </Button>
            </div>

            <Show when={send.entries().length > 0}>
              <Button
                variant="orange"
                onClick={send.start}
                class="w-full py-3 mt-6"
              >
                <span class="flex items-center justify-center gap-2">
                  <FiSend class="w-5 h-5" />
                  Generate Share Code
                </span>
              </Button>
            </Show>
          </Card>
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
          <Card>
            <TransferProgress
              progress={send.progress}
              status={
                send.state() === "completed"
                  ? "Transfer Complete!"
                  : "Sending..."
              }
              speedLabel="Upload"
            />

            <Show when={send.state() === "transferring"}>
              <p class="mt-6 text-center text-sm text-ink-dim">
                Please keep this page open until the transfer completes
              </p>
            </Show>

            <Show when={send.state() === "completed"}>
              <div class="mt-6 text-center">
                <p class="text-green-600 dark:text-green-400 font-semibold text-lg mb-4">
                  All files sent successfully!
                </p>
                <p class="text-ink-dim text-sm mb-4">
                  {formatBytes(send.progress.totalTransferred)} transferred
                </p>
                <Button variant="blue" onClick={goBack}>
                  Back to Home
                </Button>
              </div>
            </Show>
          </Card>
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

function Busy(props: { label: string; onCancel?: () => void }) {
  return (
    <Card class="text-center">
      <div class="flex justify-center mb-4">
        <div class="animate-spin w-12 h-12 border-4 border-line border-t-azure rounded-full" />
      </div>
      <p class="text-ink-muted mb-4">{props.label}</p>
      <Show when={props.onCancel}>
        <Button variant="gray" onClick={props.onCancel!}>
          Cancel
        </Button>
      </Show>
    </Card>
  );
}
