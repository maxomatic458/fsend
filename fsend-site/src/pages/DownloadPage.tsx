import { createSignal, For, Show, onMount, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { OcCopy2, OcCheck2 } from "solid-icons/oc";
import { FiArrowLeft, FiChevronRight } from "solid-icons/fi";
import {
    CLI_VERSION,
    OSES,
    RELEASE_NOTES_URL,
    RELEASES_URL,
    USAGE,
    detectOs,
    type Method,
    type Os,
} from "../lib/install";
import { SITE_URL } from "../lib/links";
import { createCopy } from "../primitives/createCopy";
import { Ext } from "../components/Ext";

function Terminal(props: { shell?: string; code: string }) {
    const { copied, copy } = createCopy(1600);

    return (
        <div class="bg-surface-2 border border-line rounded-[9px]">
            <Show when={props.shell}>
                <div class="flex items-center justify-between gap-3 px-[15px] py-[9px] border-b border-line-soft">
                    <span class="font-mono text-[11px] text-ink-faint">{props.shell}</span>
                    <button
                        type="button"
                        onClick={() => copy(props.code)}
                        class="-mr-1 p-1 rounded-md text-ink-dim hover:text-ink hover:bg-surface transition-colors cursor-pointer"
                        aria-label={copied() ? "Copied" : "Copy to clipboard"}
                    >
                        <Show
                            when={copied()}
                            fallback={<OcCopy2 class="w-4 h-4" aria-hidden="true" />}
                        >
                            <OcCheck2 class="w-4 h-4 text-ok" aria-hidden="true" />
                        </Show>
                    </button>
                </div>
            </Show>
            <pre class="m-0 p-[15px] font-mono text-[13px] leading-[1.75] text-ink-muted whitespace-pre-wrap break-words">
                <code>{props.code}</code>
            </pre>
        </div>
    );
}

function Section(props: { children: JSX.Element }) {
    return <div class="border-t border-line-soft pt-5 flex flex-col gap-3">{props.children}</div>;
}

function Heading(props: { title: string; note: JSX.Element }) {
    return (
        <div class="flex flex-col gap-1.5">
            <h2 class="text-[19px] font-bold">{props.title}</h2>
            <p class="text-sm text-ink-dim leading-[1.55]">{props.note}</p>
        </div>
    );
}

function InstallMethod(props: { method: Method }) {
    const [uninstallOpen, setUninstallOpen] = createSignal(false);

    return (
        <div class="flex flex-col gap-2.5">
            <Heading title={props.method.title} note={props.method.note} />

            <Terminal shell={props.method.shell} code={props.method.install} />

            <div class="flex items-center gap-4 text-[13px]">
                <button
                    type="button"
                    onClick={() => setUninstallOpen(!uninstallOpen())}
                    aria-expanded={uninstallOpen()}
                    class="flex items-center gap-1.5 text-ink-dim hover:text-ink transition-colors cursor-pointer"
                >
                    <FiChevronRight
                        class={`w-4 h-4 transition-transform ${uninstallOpen() ? "rotate-90" : ""}`}
                        aria-hidden="true"
                    />
                    Uninstall
                </button>
                <Show when={props.method.script}>
                    {(script) => <Ext href={script()}>View Script</Ext>}
                </Show>
            </div>

            <Show when={uninstallOpen()}>
                <Terminal shell={props.method.shell} code={props.method.uninstall} />
                <p class="text-[13px] text-ink-faint leading-[1.55]">
                    Removes the binary{props.method.uninstallExtra ?? ""}.
                </p>
            </Show>
        </div>
    );
}

function Binaries(props: { downloads: Os["downloads"] }) {
    return (
        <div class="flex flex-col gap-2.5">
            <Heading
                title="Prebuilt Binaries"
                note={
                    <>
                        Archives of the latest release. Checksums are on the{" "}
                        <Ext href={RELEASE_NOTES_URL}>release page</Ext>.
                    </>
                }
            />
            <ul class="flex flex-col border border-line rounded-[9px] divide-y divide-line-soft">
                <For each={props.downloads}>
                    {(item) => (
                        <li class="flex items-center justify-between gap-3 px-[15px] py-2.5 text-sm">
                            <span class="text-ink-muted">{item.label}</span>
                            <a
                                href={item.url}
                                class="font-mono text-[12px] text-azure hover:text-azure-hi transition-colors truncate"
                            >
                                {item.file}
                            </a>
                        </li>
                    )}
                </For>
            </ul>
        </div>
    );
}

export function DownloadPage() {
    const [osId, setOsId] = createSignal<Os["id"]>("linux");
    // A method's label, or "Binaries".
    const [choice, setChoice] = createSignal(OSES[0].methods[0].label);
    const os = () => OSES.find((o) => o.id === osId())!;
    const choices = () => [...os().methods.map((m) => m.label), "Binaries"];
    const method = () => os().methods.find((m) => m.label === choice());

    const selectOs = (id: Os["id"]) => {
        setOsId(id);
        setChoice(os().methods[0].label);
    };
    onMount(() => selectOs(detectOs()));

    return (
        <div class="flex-1 flex flex-col items-center gap-[26px] px-5 sm:px-10 pt-[26px] pb-16">
            <Title>Download fsend — CLI for Peer-to-Peer File Transfer</Title>
            <Meta
                name="description"
                content="Install the fsend command-line tool on Linux, macOS or Windows with one command. Also available from an APT repository, with cargo, or as prebuilt binaries."
            />
            <Link rel="canonical" href={`${SITE_URL}/download`} />

            <div class="w-full max-w-[660px]">
                <A
                    href="/"
                    class="inline-flex items-center gap-1 text-[13px] text-azure hover:text-azure-hi transition-colors cursor-pointer"
                >
                    <FiArrowLeft class="w-3.5 h-3.5" />
                    Back
                </A>
            </div>

            <div class="flex flex-col items-center gap-3.5">
                <h1 class="text-[32px] font-bold tracking-[-0.02em] text-center">
                    Install the fsend CLI
                </h1>
                <div class="flex items-center gap-2 font-mono text-[12.5px] text-ink-faint">
                    <span>v{CLI_VERSION}</span>
                    <span>·</span>
                    <Ext href={RELEASE_NOTES_URL}>Release Notes</Ext>
                    <span>·</span>
                    <Ext href={RELEASES_URL}>All Releases</Ext>
                </div>

                <div
                    role="tablist"
                    aria-label="Operating system"
                    class="flex items-center justify-center flex-wrap gap-2 mt-1"
                >
                    <For each={OSES}>
                        {(o) => (
                            <button
                                type="button"
                                role="tab"
                                aria-selected={osId() === o.id}
                                onClick={() => selectOs(o.id)}
                                class={`theme-flame px-4 py-2 rounded-lg text-sm font-semibold border cursor-pointer transition-colors ${
                                    osId() === o.id
                                        ? "bg-accent-soft border-accent-line text-accent-ink"
                                        : "bg-transparent border-line text-ink-muted hover:text-ink hover:border-ink-faint"
                                }`}
                            >
                                {o.label}
                            </button>
                        )}
                    </For>
                </div>
                <div
                    role="tablist"
                    aria-label="Install method"
                    class="flex items-center justify-center flex-wrap gap-1.5"
                >
                    <For each={choices()}>
                        {(label) => (
                            <button
                                type="button"
                                role="tab"
                                aria-selected={choice() === label}
                                onClick={() => setChoice(label)}
                                class={`px-3 py-1 rounded-md font-mono text-[12px] border cursor-pointer transition-colors ${
                                    choice() === label
                                        ? "bg-surface-2 border-line text-ink"
                                        : "bg-transparent border-transparent text-ink-dim hover:text-ink hover:bg-surface"
                                }`}
                            >
                                {label}
                            </button>
                        )}
                    </For>
                </div>
            </div>

            <div class="w-full max-w-[660px] flex flex-col gap-6">
                {/* Keyed, so switching tabs closes the uninstall section. */}
                <Show when={method()} keyed fallback={<Binaries downloads={os().downloads} />}>
                    {(method) => <InstallMethod method={method} />}
                </Show>

                <Section>
                    <h2 class="text-[15px] font-bold">Usage</h2>
                    <div class="grid sm:grid-cols-2 gap-6">
                        <For each={USAGE}>
                            {(item) => (
                                <div class="flex flex-col gap-[7px] min-w-0">
                                    <div class="font-mono text-[11px] tracking-[0.1em] uppercase text-ink-faint">
                                        {item.label}
                                    </div>
                                    <Terminal code={item.command} />
                                </div>
                            )}
                        </For>
                    </div>
                </Section>
            </div>
        </div>
    );
}
