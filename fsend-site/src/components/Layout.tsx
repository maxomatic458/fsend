import { Show, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { ThemeToggle } from "./ThemeToggle";
import { Logo } from "./Logo";
import { FiGithub } from "solid-icons/fi";
import { GITHUB_URL } from "../lib/links";
import { formatAge } from "../lib/format";

export function Layout(props: { children: JSX.Element }) {
  // How old the build is depends on when the page is *viewed*, but these pages
  // are prerendered — so the age stays empty through hydration and is filled in
  // on the client, then ticked so a long-lived tab does not go stale.
  const [age, setAge] = createSignal<string | null>(null);

  onMount(() => {
    const tick = () => setAge(formatAge(Date.now() - __BUILD_TIMESTAMP__));
    tick();
    const timer = setInterval(tick, 60_000);
    onCleanup(() => clearInterval(timer));
  });

  return (
    <div class="min-h-svh flex flex-col bg-canvas text-ink">
      <header class="h-[var(--app-header-h)] shrink-0 flex items-center justify-between gap-4 px-5 sm:px-8">
        <A
          href="/"
          class="flex items-center gap-2.5 shrink-0"
          aria-label="fsend home"
        >
          <Logo class="w-[26px]" />
          <span class="text-[19px] font-bold tracking-tight">fsend</span>
        </A>

        <nav class="flex items-center gap-1 text-ink-dim">
          <a
            href={GITHUB_URL}
            target="_blank"
            rel="noopener noreferrer"
            class="p-2 rounded-lg hover:text-ink hover:bg-surface-2 transition-colors"
            aria-label="fsend on GitHub"
          >
            <FiGithub class="w-5 h-5" />
          </a>
          <ThemeToggle />
        </nav>
      </header>

      <main class="flex-1 flex flex-col">{props.children}</main>

      <footer class="border-t border-line-soft px-5 sm:px-8 py-7">
        <div class="flex flex-col sm:flex-row items-center justify-between gap-4 text-sm text-ink-faint">
          <div class="flex items-center gap-2.5">
            <Logo class="w-5" />
            <span>fsend — peer-to-peer file transfer in the browser</span>
          </div>
          <div class="flex items-center gap-4">
            <span class="font-mono text-[11.5px]">
              <Show when={__BUILD_COMMIT_FULL__} fallback="dev">
                <a
                  href={`${GITHUB_URL}/commit/${__BUILD_COMMIT_FULL__}`}
                  target="_blank"
                  rel="noopener noreferrer"
                  class="hover:text-ink-muted transition-colors"
                >
                  {__BUILD_COMMIT__}
                </a>
              </Show>
              <span class="mx-1.5">·</span>
              {__BUILD_TIME__}
              <Show when={age()}>
                {(value) => <span class="ml-1.5">({value()})</span>}
              </Show>
            </span>

            <a
              href={GITHUB_URL}
              target="_blank"
              rel="noopener noreferrer"
              class="flex items-center gap-2 hover:text-ink-muted transition-colors"
            >
              <FiGithub class="w-4 h-4" />
              GitHub
            </a>
          </div>
        </div>
      </footer>
    </div>
  );
}
