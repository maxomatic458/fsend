import { For, Show, createSignal, onCleanup } from "solid-js";
import { FiCheck, FiChevronRight, FiFile, FiFolder } from "solid-icons/fi";
import { TbOutlineFingerprint } from "solid-icons/tb";
import { formatBytes, plural } from "../lib/format";
import { nodeSize } from "../lib/files/tree";
import { ALGORITHM_NAMES, type Node, type Offer } from "../lib/proto";

/** Entries a folder shows at first, and adds per "Show More". */
const PAGE = 100;
const COPIED_MS = 1500;

/** What every row of a tree shares. */
interface Tree {
    /** The row that takes focus on Tab. */
    current: () => string;
    setCurrent: (key: string) => void;
    focus: (key: string) => void;
    announce: (text: string) => void;
    /** Of the checksums, if the offer has them. */
    algorithm?: string;
}

/** The offer to click through; folders open on request, a page of entries at a time. */
export function FileTree(props: { offer: Offer; label: string }) {
    let element!: HTMLDivElement;
    const nodes = props.offer.entries;
    const [current, setCurrent] = createSignal(nodes[0]?.name ?? "");
    const [announced, setAnnounced] = createSignal("");
    const dirs = nodes.filter((node) => node.kind === "dir");
    const tree: Tree = {
        current,
        setCurrent,
        focus: (key) => {
            setCurrent(key);
            element.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`)?.focus();
        },
        announce: setAnnounced,
        algorithm: props.offer.checksums && ALGORITHM_NAMES[props.offer.checksums],
    };

    // Up, Down, Home and End move through the rows on screen.
    const onKeyDown = (event: KeyboardEvent) => {
        const row = (event.target as HTMLElement).closest<HTMLElement>("[role=treeitem]");
        const rows = [...element.querySelectorAll<HTMLElement>("[role=treeitem]")];
        const at = row ? rows.indexOf(row) : -1;
        const to = { ArrowDown: at + 1, ArrowUp: at - 1, Home: 0, End: rows.length - 1 }[event.key];
        if (to === undefined || !rows[to]) return;
        event.preventDefault();
        tree.focus(rows[to].dataset.key!);
    };

    return (
        <div>
            <div
                ref={element}
                role="tree"
                aria-label={props.label}
                onKeyDown={onKeyDown}
                class="flex flex-col border-t border-line max-h-[26rem] overflow-y-auto"
            >
                <Rows
                    nodes={nodes}
                    path={[]}
                    tree={tree}
                    // The only folder at the top opens right away.
                    open={(node) => dirs.length === 1 && node === dirs[0]}
                />
            </div>
            <div class="sr-only" aria-live="polite">
                {announced()}
            </div>
        </div>
    );
}

interface RowsProps {
    nodes: Node[];
    /** Of the folder holding `nodes`. */
    path: string[];
    tree: Tree;
    open: (node: Node) => boolean;
}

/** Indentation of a row `depth` folders deep. */
const indent = (depth: number, extra = 0) => ({ "padding-left": `${4 + depth * 22 + extra}px` });

function Rows(props: RowsProps) {
    const [shown, setShown] = createSignal(PAGE);
    const hidden = () => props.nodes.length - shown();
    return (
        <>
            <For each={props.nodes.slice(0, shown())}>
                {(node, i) => <Row {...props} node={node} position={i() + 1} />}
            </For>
            <Show when={hidden() > 0}>
                <div
                    class="flex items-center justify-between gap-3 py-[11px] pr-1 border-b border-line font-mono text-xs text-ink-faint"
                    style={indent(props.path.length, 22)}
                >
                    <span>... {plural(hidden(), "more entry", "more entries")}</span>
                    <button
                        type="button"
                        class="font-sans text-[13px] text-azure hover:text-azure-hi transition-colors cursor-pointer"
                        onClick={() => setShown((n) => n + PAGE)}
                    >
                        Show {Math.min(PAGE, hidden()).toLocaleString("en-US")} More
                    </button>
                </div>
            </Show>
        </>
    );
}

function Row(props: RowsProps & { node: Node; position: number }) {
    const path = [...props.path, props.node.name];
    const key = path.join("/");
    const [expanded, setExpanded] = createSignal(props.open(props.node));
    const node = props.node;
    let copy: (() => void) | undefined;

    const onKeyDown = (event: KeyboardEvent) => {
        if (event.target !== event.currentTarget) return;
        const open = node.kind === "dir" && expanded();
        if (event.key === "ArrowRight" && node.kind === "dir" && !open) setExpanded(true);
        else if (event.key === "ArrowLeft" && open) setExpanded(false);
        else if (event.key === "ArrowLeft" && props.path.length > 0) {
            props.tree.focus(props.path.join("/"));
        } else if (event.key === "Enter" || event.key === " ") {
            if (node.kind === "dir") setExpanded(!open);
            else copy?.();
        } else return;
        // Not handled here, ArrowRight on an open folder moves on to its first entry.
        event.preventDefault();
        event.stopPropagation();
    };

    return (
        <>
            <div
                role="treeitem"
                aria-level={props.path.length + 1}
                aria-posinset={props.position}
                aria-setsize={props.nodes.length}
                aria-expanded={node.kind === "dir" ? expanded() : undefined}
                tabIndex={props.tree.current() === key ? 0 : -1}
                data-key={key}
                onClick={() => node.kind === "dir" && setExpanded((open) => !open)}
                onFocus={() => props.tree.setCurrent(key)}
                onKeyDown={onKeyDown}
                class={`flex items-center justify-between gap-4 py-[11px] pr-1 border-b border-line focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent ${
                    node.kind === "dir" ? "cursor-pointer hover:bg-surface-2" : ""
                }`}
                style={indent(props.path.length)}
            >
                <div class="flex items-center gap-2 min-w-0">
                    {node.kind === "dir" ? (
                        <>
                            <FiChevronRight
                                class={`w-3.5 h-3.5 shrink-0 text-ink-faint transition-transform motion-reduce:transition-none ${
                                    expanded() ? "rotate-90" : ""
                                }`}
                            />
                            <FiFolder class="w-4 h-4 mr-1 shrink-0 text-ink-faint" />
                        </>
                    ) : (
                        <>
                            <span class="w-3.5 shrink-0" />
                            <FiFile class="w-4 h-4 mr-1 shrink-0 text-ink-faint" />
                        </>
                    )}
                    <span class="text-[15px] truncate min-w-0">{node.name}</span>
                    {node.kind === "file" && node.checksum && props.tree.algorithm && (
                        <CopyChecksum
                            name={node.name}
                            checksum={node.checksum}
                            algorithm={props.tree.algorithm}
                            announce={props.tree.announce}
                            ref={(run) => (copy = run)}
                        />
                    )}
                </div>
                <span class="font-mono text-[13px] text-ink-dim shrink-0 tabular-nums">
                    {formatBytes(nodeSize(node))}
                </span>
            </div>
            {node.kind === "dir" && (
                <Show when={expanded()}>
                    <Rows {...props} nodes={node.entries} path={path} />
                </Show>
            )}
        </>
    );
}

/** Copies a file's checksum; keyboard users press Enter on its row instead. */
function CopyChecksum(props: {
    name: string;
    checksum: string;
    algorithm: string;
    announce: (text: string) => void;
    ref: (copy: () => void) => void;
}) {
    const [copied, setCopied] = createSignal(false);
    let timer: ReturnType<typeof setTimeout> | undefined;
    onCleanup(() => clearTimeout(timer));

    const copy = async () => {
        try {
            await navigator.clipboard.writeText(props.checksum);
        } catch {
            props.announce("Could not copy the checksum");
            return;
        }
        props.announce(`Copied the ${props.algorithm} of ${props.name}`);
        setCopied(true);
        clearTimeout(timer);
        timer = setTimeout(() => setCopied(false), COPIED_MS);
    };
    props.ref(copy);

    const label = `Copy the ${props.algorithm} of ${props.name}`;
    return (
        <button
            type="button"
            tabIndex={-1}
            title={label}
            aria-label={label}
            onClick={(event) => {
                event.stopPropagation();
                void copy();
            }}
            class={`flex items-center justify-center w-[26px] h-[26px] shrink-0 cursor-pointer transition-colors ${
                copied() ? "text-ok" : "text-ink-faint hover:text-ink-muted"
            }`}
        >
            {copied() ? (
                <FiCheck class="w-[15px] h-[15px]" />
            ) : (
                <TbOutlineFingerprint class="w-[15px] h-[15px]" />
            )}
        </button>
    );
}
