import { Show } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiArrowLeft, FiDownload, FiFolder, FiLink } from "solid-icons/fi";
import { SITE_URL } from "../lib/links";
import { createReceiveSession } from "../primitives/createReceiveSession";
import { createWindowDropTarget } from "../primitives/createWindowDropTarget";
import { createExitGuard } from "../primitives/createExitGuard";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FileOffer } from "../components/FileOffer";
import { TransferProgress } from "../components/TransferProgress";
import { ErrorCard } from "../components/ErrorCard";
import { Button } from "../components/Button";
import { Card } from "../components/Card";
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

      <div class="max-w-2xl mx-auto">
        <button
          onClick={goBack}
          class="mb-6 text-azure hover:text-azure-hi flex items-center gap-2 transition-colors cursor-pointer"
        >
          <FiArrowLeft class="w-4 h-4" /> Back
        </button>
        <h1 class="text-3xl font-bold text-ink mb-8">Receive Files</h1>

        <Show when={receive.state() === "input"}>
          <Show when={!toDisk}>
            <div class="bg-warn-bg text-warn-ink border border-warn-line px-3 py-2 rounded-lg text-sm font-medium mb-4">
              This browser can't write straight to disk, so the transfer is held
              in memory and saved at the end. Large transfers are limited by
              available RAM, and an interrupted one can't be resumed.
            </div>
          </Show>

          <Card class="mb-6">
            <h2 class="text-xl font-semibold mb-4 text-ink">Enter Share Code</h2>

            <div class="mb-6">
              <label class="block text-sm font-medium text-ink-muted mb-2">
                Code from sender
              </label>
              <input
                type="text"
                value={receive.code()}
                onInput={(e) => receive.setCode(e.currentTarget.value)}
                placeholder="ABCD1234"
                class="w-full p-4 border border-line rounded-lg text-2xl font-mono text-center tracking-widest uppercase bg-surface-2 text-ink"
                maxLength={8}
              />
            </div>

            <Show
              when={toDisk}
              fallback={
                <div class="mb-6 p-3 bg-surface-2 rounded-lg text-ink-muted text-sm">
                  <FiDownload class="w-4 h-4 inline-block mr-2" />
                  Files will be downloaded to your Downloads folder
                </div>
              }
            >
              <div class="mb-6">
                <label class="block text-sm font-medium text-ink-muted mb-2">
                  Download Location
                </label>
                <div class="flex gap-2">
                  <div class="flex-1 p-3 border border-line rounded-lg bg-surface-2 text-ink">
                    {receive.folder()?.name ?? "No directory selected"}
                  </div>
                  <Button variant="blue" onClick={receive.chooseFolder}>
                    <span class="flex items-center gap-2">
                      <FiFolder class="w-4 h-4" />
                      Select Folder
                    </span>
                  </Button>
                </div>

                <label class="flex items-center gap-2 mt-3 text-sm text-ink-dim">
                  <input
                    type="checkbox"
                    checked={receive.resume()}
                    onChange={(e) => receive.setResume(e.currentTarget.checked)}
                    class="rounded"
                  />
                  Resume interrupted transfer
                </label>
              </div>
            </Show>

            <Button
              variant="green"
              onClick={receive.start}
              disabled={!receive.isReady()}
              class="w-full py-3"
            >
              <span class="flex items-center justify-center gap-2">
                <FiLink class="w-5 h-5" />
                Connect &amp; Receive
              </span>
            </Button>
          </Card>
        </Show>

        <Show when={receive.state() === "connecting"}>
          <Busy label="Connecting to sender..." onCancel={goBack} />
        </Show>

        <Show when={receive.state() === "handshaking"}>
          <Busy label="Establishing connection..." />
        </Show>

        <Show when={receive.state() === "offered"}>
          <Card>
            <FileOffer
              files={receive.offered()}
              onAccept={receive.acceptOffer}
              onReject={receive.rejectOffer}
            />
          </Card>
        </Show>

        <Show
          when={
            receive.state() === "transferring" ||
            receive.state() === "completed"
          }
        >
          <Card>
            <TransferProgress
              progress={receive.progress}
              status={
                receive.state() === "completed"
                  ? "Download Complete!"
                  : "Receiving..."
              }
              speedLabel="Download"
            />

            <Show when={receive.state() === "transferring"}>
              <div class="mt-6 text-center">
                <Show
                  when={toDisk}
                  fallback={
                    <p class="text-sm text-ink-dim">
                      Please keep this page open until the transfer completes
                    </p>
                  }
                >
                  <p class="text-sm text-ink-dim mb-3">
                    Canceling will keep what has arrived so far, so you can
                    resume later
                  </p>
                  <Button variant="red" onClick={cancelTransfer}>
                    Cancel Transfer
                  </Button>
                </Show>
              </div>
            </Show>

            <Show when={receive.state() === "completed"}>
              <div class="mt-6 text-center">
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
          </Card>
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
