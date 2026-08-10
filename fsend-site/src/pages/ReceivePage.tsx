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
import { formatBytes } from "../lib/format";

export function ReceivePage() {
  const navigate = useNavigate();
  const params = useParams<{ code?: string }>();
  const receive = createReceiveSession(params.code ?? "");
  const toDisk = receive.storage.kind === "disk";


  createWindowDropTarget(() => {});
  const exitGuard = createExitGuard(receive.isTransferring);

  const goBack = () => navigate("/");
  const cancelTransfer = () => exitGuard.withoutPrompt(goBack);

  return (
    <div class="flex-1 bg-canvas py-8 px-4">
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
          title="Receive files"
          steps={["Enter code", "Connect", "Receive"]}
          current={receive.step()}
          accent="azure"
          onBack={goBack}
        />

        <Show when={receive.state() === "input"}>
          <div class="w-full max-w-[560px] mx-auto flex flex-col items-center gap-4 pt-6">
            <CodeInput
              value={receive.code()}
              onChange={receive.setCode}
              onComplete={() => receive.isReady() && receive.start()}
            />

            <p class="text-sm text-ink-dim">
              Paste the sender's link instead
            </p>

            <Show when={toDisk}>
              <div class="w-full flex flex-col gap-3">
                <div class="flex gap-2.5">
                  <div class="flex-1 min-w-0 flex items-center px-4 py-3 border border-line rounded-lg bg-surface-2 text-ink truncate">
                    {receive.folder()?.name ?? "No folder selected"}
                  </div>
                  <button
                    onClick={receive.chooseFolder}
                    class="px-5 py-3 rounded-lg border border-line text-ink font-semibold text-sm hover:bg-surface-2 transition-colors cursor-pointer whitespace-nowrap"
                  >
                    <span class="flex items-center gap-2">
                      <FiFolder class="w-4 h-4" />
                      Choose folder
                    </span>
                  </button>
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
              </div>
            </Show>

            <button
              onClick={receive.start}
              disabled={!receive.isReady()}
              class="w-full py-4 rounded-lg border border-blue-700 dark:border-blue-600 bg-blue-100 dark:bg-blue-800/80 text-blue-900 dark:text-blue-50 font-bold text-base hover:bg-blue-200 dark:hover:bg-blue-700/80 disabled:opacity-50 disabled:cursor-not-allowed transition-colors cursor-pointer"
            >
              Connect &amp; receive
            </button>

            <p class="text-[13px] text-ink-faint text-center leading-relaxed max-w-[460px]">
              {toDisk
                ? "Files are written straight to the folder you choose, so size is limited by disk space and an interrupted transfer can be resumed."
                : "Files land in your Downloads folder. This browser holds the transfer in memory, so very large files are limited by RAM and can't be resumed."}
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
              receive.state() !== "transferring"
                ? undefined
                : toDisk
                  ? "Cancelling keeps what has arrived, so you can resume later."
                  : "Keep this tab open until the transfer finishes."
            }
            actions={
              <Show when={receive.state() === "transferring" && toDisk}>
                <button
                  onClick={cancelTransfer}
                  class="px-5 py-2.5 rounded-lg border border-line text-ink-muted font-semibold text-sm hover:bg-surface-2 hover:text-ink transition-colors cursor-pointer"
                >
                  Cancel
                </button>
              </Show>
            }
          />

          <Show when={receive.state() === "completed"}>
            <div class="text-center">
              <p class="text-green-600 dark:text-green-400 font-semibold text-lg mb-4">
                All files received successfully!
              </p>
              <p class="text-ink-muted mb-4">
                {formatBytes(receive.progress.totalTransferred)}{" "}
                {toDisk ? `saved to ${receive.folder()?.name}` : "downloaded"}
              </p>
              <Button variant="blue" onClick={goBack}>
                Back to Home
              </Button>
            </div>
          </Show>
        </Show>

        <Show when={receive.state() === "error"}>
          <ErrorCard class="text-center">
            <p class="text-red-600 dark:text-red-400 font-semibold mb-4">
              {receive.error()}
            </p>
            <Button variant="red" onClick={receive.retry}>
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
