import { createSignal, onMount } from "solid-js";
import { FiSun, FiMoon } from "solid-icons/fi";

export function ThemeToggle() {
  const [dark, setDark] = createSignal(false);

  onMount(() => {
    const stored = localStorage.getItem("theme");
    const prefersDark = window.matchMedia(
      "(prefers-color-scheme: dark)",
    ).matches;
    const isDark = stored === "dark" || (!stored && prefersDark);
    setDark(isDark);
    document.documentElement.classList.toggle("dark", isDark);
  });

  const toggle = () => {
    const next = !dark();
    setDark(next);
    document.documentElement.classList.toggle("dark", next);
    localStorage.setItem("theme", next ? "dark" : "light");
  };

  return (
    <button
      onClick={toggle}
      class="p-2 rounded-lg transition-colors bg-white/50 dark:bg-neutral-800/50 hover:bg-white dark:hover:bg-neutral-700 text-gray-700 dark:text-gray-200"
      aria-label="Toggle theme"
    >
      {dark() ? <FiSun class="w-5 h-5" /> : <FiMoon class="w-5 h-5" />}
    </button>
  );
}
