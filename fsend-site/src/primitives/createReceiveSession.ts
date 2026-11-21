import { createSignal, onMount } from "solid-js";
import { runReceive } from "../lib/transfer/receive";
import {
    BrowserStorage,
    folderKind,
    chooseFolder as pickFolder,
    type FolderKind,
    type StorageMode,
} from "../lib/files/storage";
import { createFolderSink, createZipSink } from "../lib/transfer/sinks";
import { createTransferRun, stepOf } from "./createTransferRun";
import type { TransferEvent } from "../lib/transfer/events";
import { CODE_LENGTH, type ConflictPolicy, type Offer } from "../lib/proto";

type ReceiveState = "input" | "connecting" | "offered" | "transferring" | "completed" | "error";

export function createReceiveSession() {
    const [state, setState] = createSignal<ReceiveState>("input");
    const run = createTransferRun();

    /// The folder the fs mode writes into; without one, zip is the only option.
    const [kind, setKind] = createSignal<FolderKind | null>(folderKind());
    const [mode, setMode] = createSignal<StorageMode>(kind() ? "fs" : "zip");
    const [storage, setStorage] = createSignal<BrowserStorage | null>(null);
    /// Bytes kept in browser storage, once known.
    const [storageUsed, setStorageUsed] = createSignal<number | null>(null);

    const refreshUsage = async () => {
        const opened = storage();
        if (opened) setStorageUsed(await opened.usage().catch(() => null));
    };

    onMount(async () => {
        if (kind() !== "browser") return;
        const opened = await BrowserStorage.open();
        if (!opened) {
            // Refused, as in private windows.
            setKind(null);
            setMode("zip");
            return;
        }
        setStorage(opened);
        await refreshUsage();
    });

    /// False when another tab is using the storage.
    const clearStorage = async () => {
        const opened = storage();
        if (!opened) return true;
        const cleared = await opened.exclusive(() => opened.clear());
        await refreshUsage();
        return cleared;
    };

    const [code, setCodeRaw] = createSignal("");
    const [offered, setOffered] = createSignal<Offer | null>(null);
    const [folder, setFolder] = createSignal<FileSystemDirectoryHandle | null>(null);
    const [policy, setPolicy] = createSignal<ConflictPolicy>("skip");
    /// Existing files that differed from the offer, once the transfer started.
    const [conflicts, setConflicts] = createSignal({ replaced: 0, skipped: 0 });
    /// 0-100 while the sink packs a zip, null when there is nothing to pack.
    const [packing, setPacking] = createSignal<number | null>(null);
    /// Whether the browser is asking the user to let fsend store more.
    const [askingStorage, setAskingStorage] = createSignal(false);
    /// Whether browser storage had no room, so the transfer is held in memory.
    const [heldInMemory, setHeldInMemory] = createSignal(false);
    /// Whether the offer was accepted, while the transfer gets ready.
    const [accepted, setAccepted] = createSignal(false);
    /// 0-100 while what is already here is checked against the checksums.
    const [verifying, setVerifying] = createSignal<number | null>(null);
    /// Stops waiting for the browser to allow more room, and downloads as a zip.
    let skipAsking = () => {};

    let accept: (() => void) | null = null;
    let reject: (() => void) | null = null;
    /// Events of earlier attempts are ignored; one may still be winding down.
    let attempt = 0;
    let previous: Promise<unknown> = Promise.resolve();
    /// A share link opened while a transfer runs, until the user decides.
    const [pendingLink, setPendingLink] = createSignal<string | null>(null);

    const chooseFolder = async () => {
        if (mode() !== "fs" || kind() !== "picked") return;
        try {
            setFolder(await pickFolder());
        } catch {
            // The picker was dismissed; nothing to do.
        }
    };

    /// Writing to a folder needs one before it can start.
    const isReady = () =>
        code().length === CODE_LENGTH &&
        (mode() === "zip" || (kind() === "picked" ? folder() !== null : storage() !== null));

    const start = () => {
        if (!isReady()) return;
        const signal = run.beginAttempt();
        const current = ++attempt;
        const onEvent = (event: TransferEvent) => {
            if (current === attempt) handle(event);
        };
        setState("connecting");
        setConflicts({ replaced: 0, skipped: 0 });
        setAskingStorage(false);
        setHeldInMemory(false);
        setAccepted(false);
        setVerifying(null);

        const staging = mode() === "fs" && kind() === "browser" ? storage() : null;
        const sink =
            mode() === "zip"
                ? createZipSink()
                : staging
                  ? createFolderSink(staging.incoming, staging)
                  : createFolderSink(folder()!);

        const skipped = new Promise<void>((resolve) => (skipAsking = resolve));
        const receive = () =>
            runReceive(
                code(),
                sink,
                // Browser storage only holds earlier transfers, so anything that differs is stale.
                staging ? "replace" : policy(),
                onEvent,
                signal,
                // Without room in browser storage, it is held in memory like a zip download.
                staging ? { fallback: createZipSink, skipAsking: skipped } : {},
            );
        if (!staging) {
            previous = receive();
            return;
        }
        // A cancelled attempt holds the storage until it has wound down.
        previous = previous
            .then(() => staging.exclusive(receive))
            .then((ran) => {
                if (ran || current !== attempt) return;
                run.setError(
                    "Another tab is receiving into browser storage. Finish that transfer first, or choose Download as Zip.",
                );
                setState("error");
            });
    };

    /// Back to entering a code, cancelling whatever attempt is running.
    const reset = () => {
        attempt++;
        run.abort();
        run.setError("");
        accept = reject = null;
        setOffered(null);
        setConflicts({ replaced: 0, skipped: 0 });
        setPacking(null);
        setAskingStorage(false);
        setState("input");
    };

    /// Whether an attempt is underway, so a new link would cancel it.
    const isBusy = () => !["input", "completed", "error"].includes(state());

    /// A share link opened on this page: its code replaces the last session,
    /// unless a transfer is running, which the user is asked about first.
    const openLink = (link: string) => {
        if (isBusy()) {
            setPendingLink(link);
            return;
        }
        if (state() !== "input") reset();
        setCodeRaw(link);
    };

    const switchToLink = () => {
        const link = pendingLink();
        setPendingLink(null);
        if (link === null) return;
        reset();
        setCodeRaw(link);
    };

    const handle = (event: TransferEvent) => {
        switch (event.type) {
            case "connecting":
                setState("connecting");
                break;
            case "offered":
                setOffered(event.offer);
                accept = event.accept;
                reject = event.reject;
                setState("offered");
                break;
            case "verifying":
                setVerifying(event.total > 0 ? (event.done / event.total) * 100 : 100);
                break;
            case "askingStorage":
                setAskingStorage(true);
                break;
            case "heldInMemory":
                setAskingStorage(false);
                setHeldInMemory(true);
                break;
            case "transferring":
                setAskingStorage(false);
                run.tracker.initialize(event.entries);
                setState("transferring");
                break;
            case "conflicts":
                setConflicts({ replaced: event.replaced, skipped: event.skipped });
                break;
            case "progress":
                run.tracker.recordBytes(event.entry, event.bytes);
                break;
            case "packing":
                setPacking(event.percent);
                break;
            case "connectionType":
                run.setConnection(event.kind);
                break;
            case "complete":
                run.tracker.complete();
                setPacking(null);
                setState("completed");
                void refreshUsage();
                break;
            case "error":
                setAskingStorage(false);
                run.tracker.cleanup();
                run.setError(event.message);
                setState("error");
                void refreshUsage();
                break;
        }
    };

    const acceptOffer = async () => {
        // A picked folder may still be missing if the code was deep-linked.
        if (mode() === "fs" && kind() === "picked" && !folder()) {
            await chooseFolder();
            if (!folder()) return;
        }
        accept?.();
        setAccepted(true);
    };

    return {
        kind,
        mode,
        setMode,
        state,
        error: run.error,
        connection: run.connection,
        progress: run.progress,
        isTransferring: () => state() === "transferring",
        /** Which of Enter Code / Connect / Receive is in progress. */
        step: () => stepOf(state(), "input"),
        code,
        setCode: setCodeRaw,
        openLink,
        pendingLink,
        switchToLink,
        keepTransfer: () => setPendingLink(null),
        offered,
        folder,
        policy,
        setPolicy,
        conflicts,
        storageUsed,
        clearStorage,
        packing,
        askingStorage,
        heldInMemory,
        accepted,
        verifying,
        downloadAsZipInstead: () => skipAsking(),
        isReady,
        chooseFolder,
        start,
        acceptOffer,
        rejectOffer: () => reject?.(),
        retry: reset,
    };
}
