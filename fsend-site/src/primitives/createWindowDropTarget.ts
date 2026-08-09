import { createSignal, onCleanup, onMount } from "solid-js";

function isEditable(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable ||
      target.tagName === "INPUT" ||
      target.tagName === "TEXTAREA")
  );
}

function looksLikeFiles(e: DragEvent): boolean {
  const types = e.dataTransfer?.types;
  if (!types) return true;
  const list = Array.from(types);
  return (
    list.length === 0 ||
    list.includes("Files") ||
    list.includes("application/x-moz-file")
  );
}

export function createWindowDropTarget(onFiles: (data: DataTransfer) => void) {
  const [isDragging, setIsDragging] = createSignal(false);
  // dragenter/dragleave fire per element crossed, so track depth rather than
  // clearing on the first leave.
  let depth = 0;

  const clear = () => {
    depth = 0;
    setIsDragging(false);
  };

  // Let a plain text drag land in an input normally; anything carrying files
  // is ours regardless of where it is dropped.
  const passThrough = (e: DragEvent) =>
    !looksLikeFiles(e) && isEditable(e.target);

  const handleEnter = (e: DragEvent) => {
    if (passThrough(e)) return;
    e.preventDefault();
    depth++;
    if (looksLikeFiles(e)) setIsDragging(true);
  };

  const handleOver = (e: DragEvent) => {
    if (passThrough(e)) return;
    e.preventDefault();
  };

  const handleLeave = (e: DragEvent) => {
    if (passThrough(e)) return;
    e.preventDefault();
    depth--;
    if (depth <= 0) clear();
  };

  const handleDrop = (e: DragEvent) => {
    if (passThrough(e)) return;
    e.preventDefault();
    clear();
  
    const data = e.dataTransfer;
    if (data && data.files.length > 0) onFiles(data);
  };

  const handleMouseOver = () => {
    if (isDragging()) clear();
  };

  onMount(() => {
    window.addEventListener("dragenter", handleEnter);
    window.addEventListener("dragover", handleOver);
    window.addEventListener("dragleave", handleLeave);
    window.addEventListener("dragend", clear);
    window.addEventListener("drop", handleDrop);
    window.addEventListener("mouseover", handleMouseOver);
  });

  onCleanup(() => {
    window.removeEventListener("dragenter", handleEnter);
    window.removeEventListener("dragover", handleOver);
    window.removeEventListener("dragleave", handleLeave);
    window.removeEventListener("dragend", clear);
    window.removeEventListener("drop", handleDrop);
    window.removeEventListener("mouseover", handleMouseOver);
  });

  return isDragging;
}
