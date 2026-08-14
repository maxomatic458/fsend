import { onMount, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiSend } from "solid-icons/fi";
import { SITE_URL } from "../lib/links";
import { pickFiles, pickDirectory, handleDrop } from "../lib/files/source";
import { takePendingDrop } from "../lib/pendingDrop";
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
import { Busy } from "../components/Busy";

export function SendPage() {
  const navigate = useNavigate();
  const send = createSendSession();

  onMount(() => {
    const pending = takePendingDrop();
    if (pending?.length) send.add(pending);
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
    <div class="theme-flame flex-1 bg-canvas py-8 px-4">
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
          title="Send Files"
          steps={["Choose", "Share code", "Transfer"]}
          current={send.step()}
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
                  isDragging() ? "text-accent" : ""
                }`}
              >
                {isDragging()
                  ? "Drop to add them"
                  : "Drop files anywhere on this page"}
              </h2>
              <p class="text-[14.5px] text-ink-dim text-center">
                No size limits
              </p>
            </Show>

            <Show when={send.entries().length > 0}>
              <div class="w-full max-w-[620px]">
                <FileList
                  items={send.items()}
                  onRemove={send.remove}
                  totalSizeBytes={send.selectionSizeBytes()}
                />
              </div>
              <p
                class={`text-sm transition-colors duration-150 ${
                  isDragging() ? "text-accent" : "text-ink-faint"
                }`}
              >
                {isDragging()
                  ? "Drop to add them"
                  : "or drop more files anywhere on this page"}
              </p>
            </Show>

            <div class="flex flex-wrap justify-center gap-2.5 mt-4">
              {/* The first selection is the primary action; once there is one,
                  Generate share code takes over and this steps back. */}
              <Button
                tone={send.entries().length === 0 ? "accent" : "neutral"}
                onClick={addFiles}
              >
                Browse files
              </Button>
              <Button onClick={addFolder}>Browse folder</Button>
            </div>

            <Show when={send.entries().length > 0}>
              <Button
                tone="accent"
                size="lg"
                class="w-full max-w-[620px] mt-2"
                onClick={send.start}
              >
                <span class="flex items-center justify-center gap-2">
                  <FiSend class="w-5 h-5" />
                  Generate share code
                </span>
              </Button>
            </Show>
          </div>
        </Show>

        <Show when={send.state() === "connecting"}>
          <Busy label="Creating session..." onCancel={goBack} />
        </Show>

        <Show when={send.state() === "waiting"}>
          <ShareCode
            code={send.shareCode()}
            expiresAtMs={send.expiresAtMs()}
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
              <p class="text-ok font-semibold text-lg mb-4">
                All files sent successfully!
              </p>
              <Button tone="accent" onClick={goBack}>
                Back to Home
              </Button>
            </div>
          </Show>
        </Show>

        <Show when={send.state() === "error"}>
          <ErrorCard
            class="text-center"
            message={send.error()}
            onRetry={send.reset}
          />
        </Show>
      </div>
    </div>
  );
}
