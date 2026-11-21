import { Show } from "solid-js";
import { formatBytes, plural } from "../lib/format";
import { nodeFiles, offerSize } from "../lib/files/tree";
import type { Offer } from "../lib/proto";
import { Button } from "./Button";
import { FileTree } from "./FileTree";

interface FileOfferProps {
    offer: Offer;
    /// The transfer is held in memory.
    inMemory?: boolean;
    /// Once accepted, what is under way; both buttons are disabled meanwhile.
    busy?: string;
    onAccept: () => void;
    onReject: () => void;
}

export function FileOffer(props: FileOfferProps) {
    const tops = () => props.offer.entries;
    const dirs = () => tops().filter((node) => node.kind === "dir").length;
    const files = () => tops().reduce((n, node) => n + nodeFiles(node), 0);
    const totalBytes = () => offerSize(props.offer);

    const summary = () => {
        const n = tops().length;
        if (dirs() === n && n > 0) return plural(n, "folder", "folders");
        if (dirs() > 0) return plural(n, "item", "items");
        return plural(n, "file", "files");
    };

    return (
        <div class="w-full max-w-[620px] mx-auto flex flex-col gap-4.5 pt-5">
            <div class="flex flex-col items-center gap-1.5 text-center">
                <h2 class="text-xl font-bold">The sender wants to share {summary()}</h2>
                <p class="text-sm text-ink-dim">Accepting starts the transfer straight away.</p>
            </div>

            <div>
                <FileTree offer={props.offer} label="Offered files" />
                <div class="flex items-center justify-between py-3 px-1 font-mono text-xs text-ink-faint">
                    <span>
                        {summary()}
                        {files() !== tops().length && ` - ${plural(files(), "file", "files")}`}
                    </span>
                    <span>total {formatBytes(totalBytes())}</span>
                </div>
            </div>

            <Show when={props.inMemory}>
                <div class="bg-warn-bg text-warn-ink border border-warn-line rounded-lg px-4 py-3 text-sm leading-relaxed">
                    You are receiving the data directly! This means the transfer will be buffered in memory until it finishes. 
                    Ensure you have at least {formatBytes(totalBytes())} of free system memory!
                </div>
            </Show>

            <div class="flex gap-2.5">
                <Button
                    tone="accent"
                    size="lg"
                    class="flex-1"
                    disabled={props.busy !== undefined}
                    onClick={props.onAccept}
                >
                    {props.busy ?? "Accept & Receive"}
                </Button>
                <Button
                    tone="ghost"
                    size="lg"
                    disabled={props.busy !== undefined}
                    onClick={props.onReject}
                >
                    Reject
                </Button>
            </div>
        </div>
    );
}
