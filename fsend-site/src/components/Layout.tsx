import type { JSX } from "solid-js";
import { A } from "@solidjs/router";
import { ThemeToggle } from "./ThemeToggle";
import { FiGithub, FiMail } from "solid-icons/fi";

export function Layout(props: { children: JSX.Element }) {
  return (
    <div class="min-h-screen flex flex-col bg-indigo-100 dark:bg-neutral-900 transition-colors">
      <header class="flex items-center justify-between px-4 py-3">
        <A
          href="/"
          class="text-xl font-bold text-gray-800 dark:text-gray-100 hover:text-indigo-600 dark:hover:text-indigo-400 transition-colors"
        >
          fsend
        </A>
        <ThemeToggle />
      </header>

      <main class="flex-1 flex flex-col">{props.children}</main>

      <footer class="bg-indigo-200 dark:bg-neutral-800 border-t border-indigo-300 dark:border-neutral-700 py-6 px-4 transition-colors">
        <div class="max-w-4xl mx-auto flex flex-col sm:flex-row items-center justify-between gap-4">
          <div class="text-sm text-gray-600 dark:text-gray-400">
            fsend - Peer-to-peer file sharing
          </div>
          <div class="flex items-center gap-6">
            <a
              href="https://github.com/maxomatic458/fsend"
              target="_blank"
              rel="noopener noreferrer"
              class="text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100 transition text-sm flex items-center gap-2"
            >
              <FiGithub class="w-4 h-4" />
              GitHub
            </a>
            <a
              href="mailto:contact@fsend.sh"
              class="text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100 transition text-sm flex items-center gap-1"
            >
              <FiMail class="w-4 h-4" />
              Contact
            </a>
          </div>
        </div>
      </footer>
    </div>
  );
}
