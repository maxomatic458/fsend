import { Show } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiFolder } from "solid-icons/fi";
import { SITE_URL } from "../lib/links";
import { createReceiveSession } from "../primitives/createReceiveSession";
import { createWindowDropTarget } from "../primitives/createWindowDropTarget";
import { createExitGuard } from "../primitives/createExitGuard";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { TransferHeader } from "../components/StepIndicator";
import { CodeInput } from "../components/CodeInput";
import { FileOffer } from "../components/FileOffer";
import { TransferProgress } from "../components/TransferProgress";
import { ErrorCard } from "../components/ErrorCard";
import { Button } from "../components/Button";
import { Busy } from "../components/Busy";
import { formatBytes } from "../lib/format";

export function ReceivePage() {
  const navigate = useNavigate();
  const params = useParams<{ code?: string }>();
  const receive = createReceiveSession(params.code ?? "");
  const toDisk = () => receive.mode() === "disk";

  createWindowDropTarget(() => {});
  const exitGuard = createExitGuard(receive.isTransferring);

  const goBack = () => navigate("/");
  const cancelTransfer = () => exitGuard.withoutPrompt(goBack);

  return (
    <div class="theme-azure flex-1 bg-canvas py-8 px-4">
      <ConfirmDialog
        open={exitGuard.isPrompting()}
        title="Leave while downloading?"
        message="The download is still running. Leaving this page cancels it."
        confirmLabel="Leave and cancel"
        cancelLabel="Keep downloading"
        onConfirm={exitGuard.confirm}
        onCancel={exitGuard.cancel}
      />

      <Title>Receive Files — fsend</Title>
      <Meta
        name="description"
        content="Enter a session code to receive files directly from the sender's device over an encrypted peer-to-peer WebRTC connection. No accounts, no size limits."
      />
      <Link rel="canonical" href={`${SITE_URL}/receive`} />

      <div class="max-w-2xl mx-auto flex flex-col gap-6">
        <TransferHeader
          title="Receive Files"
          steps={["Enter code", "Connect", "Receive"]}
          current={receive.step()}
          onBack={goBack}
        />

        <Show when={receive.state() === "input"}>
          <div class="w-full max-w-[560px] mx-auto flex flex-col items-center gap-4 pt-6">
            <CodeInput
              value={receive.code()}
              onChange={receive.setCode}
              onComplete={() => receive.isReady() && receive.start()}
            />

            <p class="text-sm text-ink-dim">Paste the sender's link instead</p>

            <div class="w-full flex flex-col gap-3">
              {/* If the File System Access API is available the user can choose download type */}
              <Show when={receive.canUseDisk}>
                <label class="flex items-center gap-2 text-sm text-ink-dim cursor-pointer">
                  <input
                    type="checkbox"
                    checked={toDisk()}
                    onChange={(e) =>
                      receive.setMode(
                        e.currentTarget.checked ? "disk" : "download",
                      )
                    }
                    class="rounded"
                  />
                  Save straight to a folder
                </label>
              </Show>

              <Show when={toDisk()}>
                <div class="flex gap-2.5">
                  <div class="flex-1 min-w-0 flex items-center px-4 py-3 border border-line rounded-lg bg-surface-2 text-ink truncate">
                    {receive.folder()?.name ?? "No folder selected"}
                  </div>
                  <Button
                    onClick={receive.chooseFolder}
                    class="whitespace-nowrap"
                  >
                    <span class="flex items-center gap-2">
                      <FiFolder class="w-4 h-4" />
                      Choose folder
                    </span>
                  </Button>
                </div>
                <label class="flex items-center gap-2 text-sm text-ink-dim cursor-pointer">
                  <input
                    type="checkbox"
                    checked={receive.resume()}
                    onChange={(e) => receive.setResume(e.currentTarget.checked)}
                    class="rounded"
                  />
                  Resume an interrupted transfer
                </label>
              </Show>
            </div>

            <Button
              tone="accent"
              size="lg"
              class="w-full"
              disabled={!receive.isReady()}
              onClick={receive.start}
            >
              Connect &amp; receive
            </Button>

            <p class="text-[13px] text-ink-faint text-center leading-relaxed max-w-[460px]">
              {toDisk()
                ? "Files are written straight to the folder you choose, so size is limited by disk space and an interrupted transfer can be resumed."
                : "Files land in your Downloads folder, zipped if there is more than one. The transfer is held in memory until it finishes, so size is limited by RAM and it can't be resumed."}
            </p>
          </div>
        </Show>

        <Show when={receive.state() === "connecting"}>
          <Busy label="Connecting to sender..." onCancel={goBack} />
        </Show>

        <Show when={receive.state() === "handshaking"}>
          <Busy label="Establishing connection..." />
        </Show>

        <Show when={receive.state() === "offered"}>
          <FileOffer
            files={receive.offered()}
            inMemory={!toDisk()}
            onAccept={receive.acceptOffer}
            onReject={receive.rejectOffer}
          />
        </Show>

        <Show
          when={
            receive.state() === "transferring" ||
            receive.state() === "completed"
          }
        >
          <TransferProgress
            progress={receive.progress}
            verb="received"
            connection={receive.connection()}
            hint={
              receive.packing() !== null
                ? `Packing the zip… ${Math.round(receive.packing()!)}%`
                : receive.state() !== "transferring"
                  ? undefined
                  : toDisk()
                    ? "Cancelling keeps what has arrived, so you can resume later."
                    : "Keep this tab open until the transfer finishes."
            }
            actions={
              <Show when={receive.state() === "transferring" && toDisk()}>
                <Button tone="ghost" size="sm" onClick={cancelTransfer}>
                  Cancel
                </Button>
              </Show>
            }
          />

          <Show when={receive.state() === "completed"}>
            <div class="text-center">
              <p class="text-ok font-semibold text-lg mb-4">
                All files received successfully!
              </p>
              <p class="text-ink-muted mb-4">
                {formatBytes(receive.progress.totalTransferredBytes)}{" "}
                {toDisk()
                  ? `saved to ${receive.folder()?.name}`
                  : "handed to your browser"}
              </p>

              <Show when={!toDisk()}>
                <p class="text-[13px] text-ink-faint mb-4">
                  Check your downloads — saving may still be in progress for a
                  large file.
                </p>
              </Show>
              <Button tone="accent" onClick={goBack}>
                Back to Home
              </Button>
            </div>
          </Show>
        </Show>

        <Show when={receive.state() === "error"}>
          <ErrorCard
            class="text-center"
            message={receive.error()}
            onRetry={receive.retry}
          />
        </Show>
      </div>
    </div>
  );
}
