import { createMemo, createSignal } from "solid-js";
import { runSend } from "../lib/transfer/send";
import { buildOffer, offerSize } from "../lib/files/tree";
import { createTransferRun, stepOf } from "./createTransferRun";
import type { SelectedEntry } from "../lib/types";
import type { Algorithm } from "../lib/proto";

/// How the files are checksummed, if the user asks for checksums.
const ALGORITHM: Algorithm = "sha256";

export interface SelectedItem {
    entry: SelectedEntry;
    /// null while measuring
    sizeBytes: number | null;
    /// Why it could not be measured.
    error?: string;
}

type SendState =
    | "selecting"
    | "hashing"
    | "connecting"
    | "waiting"
    | "handshaking"
    | "waitingAccept"
    | "transferring"
    | "completed"
    | "error";

export function createSendSession() {
    const [state, setState] = createSignal<SendState>("selecting");
    const run = createTransferRun();

    const [items, setItems] = createSignal<SelectedItem[]>([]);
    const [shareCode, setShareCode] = createSignal("");
    const [expiresAtMs, setExpiresAtMs] = createSignal(0);
    /// Whether every file is offered with its checksum.
    const [checksums, setChecksums] = createSignal(false);
    /// 0-100 while the files are read for their checksums.
    const [hashed, setHashed] = createSignal(0);

    const entries = createMemo(() => items().map((item) => item.entry));
    const selectionSizeBytes = createMemo(() =>
        items().reduce((total, item) => total + (item.sizeBytes ?? 0), 0),
    );

    const measure = async (item: SelectedItem) => {
        let measured: Partial<SelectedItem>;
        try {
            measured = {
                sizeBytes: offerSize((await buildOffer([item.entry])).offer),
            };
        } catch (err) {
            measured = { error: err instanceof Error ? err.message : String(err) };
        }
        setItems((current) =>
            current.includes(item)
                ? current.map((each) => (each === item ? { ...each, ...measured } : each))
                : current,
        );
    };

    const add = (added: SelectedEntry[]) => {
        const pending: SelectedItem[] = added.map((entry) => ({
            entry,
            sizeBytes: null,
        }));
        setItems((current) => [...current, ...pending]);
        for (const item of pending) void measure(item);
    };

    const remove = (index: number) => {
        setItems((current) => current.filter((_, i) => i !== index));
    };

    // Events of an earlier attempt that are still on their way are ignored.
    let attempt = 0;

    const start = () => {
        if (entries().length === 0) return;
        const signal = run.beginAttempt();
        const current = ++attempt;
        setState("connecting");

        runSend(
            entries(),
            (event) => {
                if (current !== attempt) return;
                switch (event.type) {
                    case "hashing":
                        setHashed(event.total > 0 ? (event.done / event.total) * 100 : 100);
                        setState(event.done < event.total ? "hashing" : "connecting");
                        break;
                    case "code":
                        setShareCode(event.code);
                        setExpiresAtMs(Date.now() + event.expiresInSecs * 1000);
                        setState("waiting");
                        break;
                    case "handshaking":
                        setState("handshaking");
                        break;
                    case "waitingAccept":
                        setState("waitingAccept");
                        break;
                    case "transferring":
                        run.tracker.initialize(event.entries);
                        setState("transferring");
                        break;
                    case "progress":
                        run.tracker.recordBytes(event.entry, event.bytes);
                        break;
                    case "connectionType":
                        run.setConnection(event.kind);
                        break;
                    case "complete":
                        run.tracker.complete();
                        setState("completed");
                        break;
                    case "error":
                        run.tracker.cleanup();
                        run.setError(event.message);
                        setState("error");
                        break;
                }
            },
            signal,
            { checksums: checksums() ? ALGORITHM : undefined },
        );
    };

    /// Back to the selection, which is kept.
    const reset = () => {
        attempt++;
        setState("selecting");
        setShareCode("");
        run.setError("");
    };

    return {
        state,
        error: run.error,
        connection: run.connection,
        progress: run.progress,
        isTransferring: () => state() === "transferring",
        /** Which of Choose / Share Code / Transfer is in progress. */
        step: () => stepOf(state(), "selecting"),
        entries,
        shareCode,
        expiresAtMs,
        algorithm: ALGORITHM,
        checksums,
        setChecksums,
        hashed,
        items,
        selectionSizeBytes,
        add,
        remove,
        start,
        /// Back to an empty selection.
        cancel: () => {
            run.abort();
            reset();
            setItems([]);
        },
        reset,
    };
}
