import { Show, createSignal } from "solid-js";
import { formatBytes } from "../lib/format";
import { Button } from "./Button";
import { ConfirmDialog } from "./ConfirmDialog";

interface StorageUsageProps {
    /// Bytes kept, null while unknown.
    used: number | null;
    /// False when the storage is in use elsewhere and was left alone.
    onClear: () => Promise<boolean>;
}

/// What fsend keeps in the browser's storage, with a way to delete it.
export function StorageUsage(props: StorageUsageProps) {
    const [confirming, setConfirming] = createSignal(false);
    const [clearing, setClearing] = createSignal(false);
    const [busy, setBusy] = createSignal(false);

    const clear = async () => {
        setConfirming(false);
        setClearing(true);
        try {
            setBusy(!(await props.onClear()));
        } finally {
            setClearing(false);
        }
    };

    return (
        <div class="flex items-center gap-3 px-4 py-3 border border-line rounded-lg bg-surface-2 text-sm">
            <ConfirmDialog
                open={confirming()}
                title="Clear Browser Storage?"
                message="This deletes the files fsend keeps in this browser, including interrupted transfers that could still be resumed."
                confirmLabel="Clear"
                cancelLabel="Keep"
                onConfirm={clear}
                onCancel={() => setConfirming(false)}
            />
            <span class="flex-1 min-w-0 text-ink-dim">
                Browser Storage:{" "}
                <span class="text-ink">
                    {props.used === null ? "..." : `${formatBytes(props.used)} Used`}
                </span>
                <Show when={busy()}>
                    <span class="block text-[13px] text-ink-faint">
                        Another tab is using it, so nothing was cleared.
                    </span>
                </Show>
            </span>
            <Button
                tone="ghost"
                size="sm"
                disabled={!props.used || clearing()}
                onClick={() => setConfirming(true)}
            >
                {clearing() ? "Clearing..." : "Clear"}
            </Button>
        </div>
    );
}
