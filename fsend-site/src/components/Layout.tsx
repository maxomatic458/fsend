import type { JSX } from "solid-js";
import { For, Show } from "solid-js";
import { A, useLocation } from "@solidjs/router";
import { ThemeToggle } from "./ThemeToggle";
import { Logo } from "./Logo";
import { FiGithub } from "solid-icons/fi";
import { GITHUB_URL } from "../lib/links";

const SECTIONS = [
  { href: "#how", label: "How it works" },
  { href: "#faq", label: "FAQ" },
];

export function Layout(props: { children: JSX.Element }) {
  const location = useLocation();
  const isLanding = () => location.pathname === "/";

  return (
    <div class="min-h-screen flex flex-col bg-canvas text-ink">
      <header class="sticky top-0 z-40 border-b border-line bg-canvas/85 backdrop-blur-md">
        <div class="flex items-center justify-between gap-4 px-5 sm:px-10 py-3.5">
          <A href="/" class="flex items-center gap-2.5 shrink-0" aria-label="fsend home">
            <Logo class="w-[26px]" />
            <span class="text-[19px] font-bold tracking-tight">fsend</span>
          </A>

          <nav class="flex items-center gap-5 sm:gap-6 text-sm text-ink-dim">
            <Show when={isLanding()}>
              <For each={SECTIONS}>
                {(s) => (
                  <a
                    href={s.href}
                    class="hidden sm:inline hover:text-ink transition-colors"
                  >
                    {s.label}
                  </a>
                )}
              </For>
            </Show>
            <a
              href={GITHUB_URL}
              target="_blank"
              rel="noopener noreferrer"
              class="hover:text-ink transition-colors"
              aria-label="fsend on GitHub"
            >
              <FiGithub class="w-[18px] h-[18px]" />
            </a>
            <ThemeToggle />
          </nav>
        </div>
      </header>

      <main class="flex-1 flex flex-col">{props.children}</main>

      <footer class="border-t border-line-soft px-5 sm:px-10 py-7">
        <div class="flex flex-col sm:flex-row items-center justify-between gap-4 text-sm text-ink-faint">
          <div class="flex items-center gap-2.5">
            <Logo class="w-5" />
            <span>fsend — peer-to-peer file transfer in the browser</span>
          </div>
          <div class="flex items-center gap-6">
            <A href="/#faq" class="hover:text-ink-muted transition-colors">
              FAQ
            </A>
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
