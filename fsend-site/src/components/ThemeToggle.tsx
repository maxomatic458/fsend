import { createSignal, onMount } from "solid-js";
import { FiSun, FiMoon } from "solid-icons/fi";

export function ThemeToggle() {
  const [dark, setDark] = createSignal(true);

  onMount(() => {
    // index.html already applied the class before paint.
    setDark(document.documentElement.classList.contains("dark"));
  });

  const toggle = () => {
    const next = !dark();
    setDark(next);
    document.documentElement.classList.toggle("dark", next);
    document
      .querySelector('meta[name="theme-color"]')
      ?.setAttribute("content", next ? "#151515" : "#f7f5f2");
    localStorage.setItem("theme", next ? "dark" : "light");
  };

  return (
    <button
      onClick={toggle}
      class="p-1.5 rounded-lg text-ink-dim hover:text-ink hover:bg-surface-2 transition-colors cursor-pointer"
      aria-label={dark() ? "Switch to light theme" : "Switch to dark theme"}
    >
      {dark() ? (
        <FiSun class="w-[18px] h-[18px]" />
      ) : (
        <FiMoon class="w-[18px] h-[18px]" />
      )}
    </button>
  );
}
