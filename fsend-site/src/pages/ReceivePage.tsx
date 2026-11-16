import { createEffect, on, Show } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiFolder } from "solid-icons/fi";
import { SITE_URL } from "../lib/links";
import { createReceiveSession } from "../primitives/createReceiveSession";
import { createWindowDropTarget } from "../primitives/createWindowDropTarget";
import { createExitGuard } from "../primitives/createExitGuard";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { TransferHeader } from "../components/TransferHeader";
import { CodeInput } from "../components/CodeInput";
import { FileOffer } from "../components/FileOffer";
import { TransferProgress } from "../components/TransferProgress";
import { ErrorCard } from "../components/ErrorCard";
import { Button } from "../components/Button";
import { Busy } from "../components/Busy";
import { StorageUsage } from "../components/StorageUsage";
import { formatBytes, plural } from "../lib/format";

export function ReceivePage() {
  const navigate = useNavigate();
  const location = useLocation();
  const receive = createReceiveSession();
  const toFs = () => receive.mode() === "fs";
  const toFolder = () => toFs() && receive.kind() === "picked";
  /// Held in memory: chosen, or because browser storage had no room.
  const inMemory = () => !toFs() || receive.heldInMemory();

  // Swallows stray drops so they don't open the file.
  createWindowDropTarget(() => {});
  const exitGuard = createExitGuard(receive.isTransferring);

  // Share links carry the code in the fragment, which never reaches a server.
  // Once read it is dropped, so opening the same link again counts as new.
  createEffect(
    on(
      () => location.hash,
      (hash) => {
        if (!hash) return;
        receive.openLink(hash.slice(1));
        exitGuard.withoutPrompt(() =>
          navigate(location.pathname, { replace: true, scroll: false }),
        );
      },
    ),
  );

  const goBack = () => navigate("/");
  const cancelTransfer = () => exitGuard.withoutPrompt(goBack);

  return (
    <div class="theme-azure flex-1 bg-canvas py-8 px-4">
      <ConfirmDialog
        open={exitGuard.isPrompting()}
        title="Leave While Downloading?"
        message="The download is still running. Leaving this page cancels it."
        confirmLabel="Leave and Cancel"
        cancelLabel="Keep Downloading"
        onConfirm={exitGuard.confirm}
        onCancel={exitGuard.cancel}
      />

      <ConfirmDialog
        open={receive.pendingLink() !== null}
        title="Receive From the New Link?"
        message={
          !inMemory()
            ? "This cancels the current transfer. What has arrived so far stays, so it can resume later."
            : "This cancels the current transfer."
        }
        confirmLabel="Cancel and Switch"
        cancelLabel="Keep Transfer"
        onConfirm={receive.switchToLink}
        onCancel={receive.keepTransfer}
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
          steps={["Enter Code", "Connect", "Receive"]}
          current={receive.step()}
          onBack={goBack}
        />

        <Show when={receive.state() === "input"}>
          <div class="w-full max-w-[560px] mx-auto flex flex-col items-center gap-4 pt-6">
            <CodeInput
              value={receive.code()}
              onInput={receive.setCode}
              onComplete={() => receive.isReady() && receive.start()}
            />

            <div class="w-full flex flex-col gap-3">
              <Show when={receive.kind()}>
                <div class="flex flex-wrap gap-x-5 gap-y-2 text-sm text-ink-dim">
                  <label class="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="storage"
                      checked={toFs()}
                      onChange={() => receive.setMode("fs")}
                    />
                    {receive.kind() === "picked"
                      ? "Save to Folder"
                      : "Save in Browser Storage"}
                  </label>
                  <label class="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="storage"
                      checked={!toFs()}
                      onChange={() => receive.setMode("zip")}
                    />
                    Download as Zip
                  </label>
                </div>
              </Show>

              <Show when={toFs() && receive.kind() === "browser"}>
                <StorageUsage
                  used={receive.storageUsed()}
                  onClear={receive.clearStorage}
                />
              </Show>

              <Show when={toFolder()}>
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
                      Choose Folder
                    </span>
                  </Button>
                </div>
                <label class="flex items-center justify-between gap-3 text-sm text-ink-dim">
                  If Files Already Exist
                  <select
                    value={receive.policy()}
                    onChange={(e) =>
                      receive.setPolicy(
                        e.currentTarget.value === "replace"
                          ? "replace"
                          : "skip",
                      )
                    }
                    class="px-3 py-2 border border-line rounded-lg bg-surface-2 text-ink cursor-pointer"
                  >
                    <option value="skip">Skip Them</option>
                    <option value="replace">Replace Them</option>
                  </select>
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
              Connect &amp; Receive
            </Button>

            <p class="text-[13px] text-ink-faint text-center leading-relaxed max-w-[460px]">
              {toFolder()
                ? "Files are written straight to the folder you choose, so size is limited by disk space. An interrupted transfer resumes where it stopped; other files that already exist are skipped or replaced, as chosen above."
                : toFs()
                  ? "Files are kept in this browser's storage while they arrive, then saved to your Downloads folder, zipped if there is more than one. An interrupted transfer resumes where it stopped, as long as it stays in browser storage."
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
            inMemory={!toFs()}
            busy={receive.accepted()}
            onAccept={receive.acceptOffer}
            onReject={receive.rejectOffer}
          />
          <Show when={receive.askingStorage()}>
            <div class="flex flex-col items-center gap-3 text-center">
              <p class="text-sm text-ink-dim">
                This transfer needs more browser storage than fsend may use by
                default. Allow it when your browser asks, or download it as a
                zip, which holds it in memory until it finishes.
              </p>
              <Button
                tone="ghost"
                size="sm"
                onClick={receive.downloadAsZipInstead}
              >
                Download as Zip Instead
              </Button>
            </div>
          </Show>
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
                ? `Packing the zip... ${Math.round(receive.packing()!)}%`
                : receive.state() !== "transferring"
                  ? undefined
                  : receive.heldInMemory()
                    ? "Browser storage had no room, so this transfer is held in memory. Keep this tab open until it finishes."
                    : !inMemory()
                      ? "Cancelling keeps what has arrived, so you can resume later."
                      : "Keep this tab open until the transfer finishes."
            }
            actions={
              <Show when={receive.state() === "transferring" && !inMemory()}>
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
                {toFolder()
                  ? `saved to ${receive.folder()?.name}`
                  : "should now be downloading in your browser"}
              </p>

              <Show when={receive.conflicts().skipped > 0}>
                <p class="text-[13px] text-ink-faint mb-4">
                  Skipped{" "}
                  {plural(
                    receive.conflicts().skipped,
                    "existing entry",
                    "existing entries",
                  )}{" "}
                  that differed from the offer.
                </p>
              </Show>
              <Show when={receive.conflicts().replaced > 0}>
                <p class="text-[13px] text-ink-faint mb-4">
                  Replaced{" "}
                  {plural(
                    receive.conflicts().replaced,
                    "existing file",
                    "existing files",
                  )}{" "}
                  that differed from the offer.
                </p>
              </Show>

              <Show when={!toFolder()}>
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
