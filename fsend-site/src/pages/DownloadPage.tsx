import {
  createSignal,
  For,
  Show,
  onMount,
  onCleanup,
  type JSX,
} from "solid-js";
import { A } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { OcCopy2, OcCheck2 } from "solid-icons/oc";
import { FiChevronRight } from "solid-icons/fi";
import {
  OSES,
  USAGE,
  RELEASES_URL,
  detectPlatform,
  type Os,
} from "../lib/install";
import { SITE_URL } from "../lib/links";

function Terminal(props: { shell?: string; code: string }) {
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.code);
      setCopied(true);
      clearTimeout(timer);
      timer = setTimeout(() => setCopied(false), 1600);
    } catch {}
  };

  return (
    <div class="bg-surface-2 border border-line rounded-[9px]">
      <Show when={props.shell}>
        <div class="flex items-center justify-between gap-3 px-[15px] py-[9px] border-b border-line-soft">
          <span class="font-mono text-[11px] text-ink-faint">
            {props.shell}
          </span>
          <button
            type="button"
            onClick={copy}
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
  return (
    <div class="border-t border-line-soft pt-5 flex flex-col gap-3">
      {props.children}
    </div>
  );
}

export function DownloadPage() {
  const [activeOs, setActiveOs] = createSignal<Os["id"]>(OSES[0].id);
  const [activeKey, setActiveKey] = createSignal(OSES[0].variants[0].key);
  const [uninstallOpen, setUninstallOpen] = createSignal(false);

  onMount(() => {
    const guess = detectPlatform();
    setActiveOs(guess.os);
    setActiveKey(guess.key);
  });

  const os = () => OSES.find((o) => o.id === activeOs()) ?? OSES[0];
  const variant = () =>
    os().variants.find((v) => v.key === activeKey()) ?? os().variants[0];

  const selectOs = (id: Os["id"]) => {
    const next = OSES.find((o) => o.id === id) ?? OSES[0];
    setActiveOs(id);
    setActiveKey(next.variants[0].key);
    setUninstallOpen(false);
  };

  const selectVariant = (key: string) => {
    setActiveKey(key);
    setUninstallOpen(false);
  };

  return (
    <div class="flex-1 flex flex-col items-center gap-[26px] px-5 sm:px-10 pt-[26px] pb-16">
      <Title>Download fsend — CLI for peer-to-peer file transfer</Title>
      <Meta
        name="description"
        content="Install the fsend command-line tool on Linux, macOS or Windows. Signed APT repository for Debian and Ubuntu, static binaries for everything else, or build from source with cargo."
      />
      <Link rel="canonical" href={`${SITE_URL}/download`} />

      <div class="w-full max-w-[660px]">
        <A
          href="/"
          class="text-[13px] text-azure hover:text-azure-hi transition-colors cursor-pointer"
        >
          ← Back
        </A>
      </div>

      <div class="flex flex-col items-center gap-3.5">
        <h1 class="text-[32px] font-bold tracking-[-0.02em] text-center">
          Install fsend CLI
        </h1>
        <div class="font-mono text-[12.5px] text-ink-faint">
          Use fsend from your command line
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
                aria-selected={activeOs() === o.id}
                onClick={() => selectOs(o.id)}
                class={`theme-flame px-4 py-2 rounded-lg text-sm font-semibold border cursor-pointer transition-colors ${
                  activeOs() === o.id
                    ? "bg-accent-soft border-accent-line text-accent-ink"
                    : "bg-transparent border-line text-ink-muted hover:text-ink hover:border-ink-faint"
                }`}
              >
                {o.label}
              </button>
            )}
          </For>
        </div>
        {/* if the OS has variants (e.g x86, aarch64) */}
        <Show when={os().variants.length > 1}>
          <div
            role="tablist"
            aria-label="Architecture"
            class="flex items-center justify-center flex-wrap gap-1.5"
          >
            <For each={os().variants}>
              {(v) => (
                <button
                  type="button"
                  role="tab"
                  aria-selected={activeKey() === v.key}
                  onClick={() => selectVariant(v.key)}
                  class={`px-3 py-1 rounded-md font-mono text-[12px] border cursor-pointer transition-colors ${
                    activeKey() === v.key
                      ? "bg-surface-2 border-line text-ink"
                      : "bg-transparent border-transparent text-ink-dim hover:text-ink hover:bg-surface"
                  }`}
                >
                  {v.label}
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>

      <div class="w-full max-w-[660px] flex flex-col gap-6">
        <div class="flex flex-col gap-1.5">
          <h2 class="text-[19px] font-bold">{variant().title}</h2>
          <p class="text-sm text-ink-dim leading-[1.55]">{variant().note}</p>
        </div>

        <div class="flex flex-col gap-2.5">
          <Terminal shell={variant().shell} code={variant().install} />
          <p class="text-[13px] text-ink-faint leading-[1.55]">
            {variant().scriptNote}
          </p>
        </div>

        <Section>
          <button
            type="button"
            onClick={() => setUninstallOpen(!uninstallOpen())}
            aria-expanded={uninstallOpen()}
            class="flex items-center gap-2.5 text-sm text-ink-muted hover:text-ink transition-colors cursor-pointer"
          >
            <FiChevronRight
              class={`w-4 h-4 transition-transform ${
                uninstallOpen() ? "rotate-90" : ""
              }`}
              aria-hidden="true"
            />
            <span>Uninstall</span>
          </button>

          <Show when={uninstallOpen()}>
            <div class="flex flex-col gap-2.5">
              <Terminal shell={variant().shell} code={variant().uninstall} />
              <p class="text-[13px] text-ink-faint leading-[1.55]">
                Removes the binary{variant().uninstallExtra ?? ""}.
              </p>
            </div>
          </Show>
        </Section>

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

        <Section>
          <p class="text-[13.5px] text-ink-dim leading-relaxed">
            Prebuilt binaries for every platform are on{" "}
            <a
              href={RELEASES_URL}
              target="_blank"
              rel="noopener noreferrer"
              class="text-azure underline underline-offset-2 decoration-azure/40 hover:text-azure-hi hover:decoration-azure-hi transition-colors"
            >
              GitHub releases
            </a>
            .
          </p>
        </Section>
      </div>
    </div>
  );
}
