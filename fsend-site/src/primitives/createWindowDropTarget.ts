import { createSignal, onCleanup, onMount } from "solid-js";

/**
 * Makes the whole window a file drop target.
 *
 * Listening on `window` rather than on a page element matters: the sticky
 * header and the footer live outside the page component, so element-scoped
 * handlers leave them as dead zones — and the header is exactly what sits
 * under the cursor when a file is dragged in from the top of the screen.
 *
 * Only drags that actually carry files activate it, so dragging selected text
 * or a link around the page doesn't trip the overlay.
 */
export function createWindowDropTarget(onFiles: (data: DataTransfer) => void) {
  const [isDragging, setIsDragging] = createSignal(false);
  // dragenter/dragleave fire per element crossed, so track depth rather than
  // clearing on the first leave.
  let depth = 0;
  let watchdog: ReturnType<typeof setTimeout> | undefined;

  const hasFiles = (e: DragEvent) =>
    Array.from(e.dataTransfer?.types ?? []).includes("Files");

  const clear = () => {
    depth = 0;
    clearTimeout(watchdog);
    watchdog = undefined;
    setIsDragging(false);
  };

  // A drag that ends outside the window doesn't always deliver a final
  // dragleave, which would strand the UI in its dragging state. dragover
  // repeats every few hundred ms for as long as a drag is live, so treat a
  // gap in that stream as the drag being over.
  const kickWatchdog = () => {
    clearTimeout(watchdog);
    watchdog = setTimeout(clear, 1000);
  };

  const handleEnter = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth++;
    setIsDragging(true);
    kickWatchdog();
  };

  const handleOver = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault(); // without this the browser refuses the drop outright
    kickWatchdog();
  };

  const handleLeave = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    depth--;
    if (depth <= 0) clear();
  };

  const handleDrop = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    clear();
    if (e.dataTransfer) onFiles(e.dataTransfer);
  };

  onMount(() => {
    window.addEventListener("dragenter", handleEnter);
    window.addEventListener("dragover", handleOver);
    window.addEventListener("dragleave", handleLeave);
    window.addEventListener("dragend", clear);
    window.addEventListener("drop", handleDrop);
  });

  onCleanup(() => {
    clearTimeout(watchdog);
    window.removeEventListener("dragenter", handleEnter);
    window.removeEventListener("dragover", handleOver);
    window.removeEventListener("dragleave", handleLeave);
    window.removeEventListener("dragend", clear);
    window.removeEventListener("drop", handleDrop);
  });

  return isDragging;
}
