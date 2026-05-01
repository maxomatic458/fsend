import { createSignal, Show } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { FiUpload, FiDownload } from "solid-icons/fi";
import { handleDrop } from "../lib/filePicker";
import { supportsFileSystemAccess } from "../lib/fsAccess";

export function HomePage() {
  const navigate = useNavigate();
  const [isDragging, setIsDragging] = createSignal(false);
  const [isProcessing, setIsProcessing] = createSignal(false);
  let dragCounter = 0;

  const onDragEnter = (e: DragEvent) => {
    e.preventDefault();
    dragCounter++;
    setIsDragging(true);
  };

  const onDragLeave = (e: DragEvent) => {
    e.preventDefault();
    dragCounter--;
    if (dragCounter <= 0) {
      dragCounter = 0;
      setIsDragging(false);
    }
  };

  const onDragOver = (e: DragEvent) => e.preventDefault();

  const onDrop = async (e: DragEvent) => {
    e.preventDefault();
    dragCounter = 0;
    setIsDragging(false);
    if (!e.dataTransfer) return;
    setIsProcessing(true);
    try {
      const entries = await handleDrop(e.dataTransfer);
      if (entries.length > 0) {
        (window as any).__fsend_pending = entries;
        navigate("/send");
      }
    } finally {
      setIsProcessing(false);
    }
  };

  return (
    <div
      class="flex-1 bg-indigo-100 dark:bg-neutral-900 flex items-center justify-center p-4 transition-colors relative"
      onDragEnter={onDragEnter}
      onDragLeave={onDragLeave}
      onDragOver={onDragOver}
      onDrop={onDrop}
    >
      <Show when={isProcessing()}>
        <div class="absolute inset-0 bg-indigo-100/80 dark:bg-neutral-900/80 flex items-center justify-center z-10">
          <div class="flex flex-col items-center">
            <div class="animate-spin w-16 h-16 border-4 border-gray-300 border-t-blue-500 rounded-full" />
            <p class="mt-4 text-gray-600 dark:text-gray-400 font-medium">
              Processing files...
            </p>
          </div>
        </div>
      </Show>

      <div class="max-w-lg w-full">
        <div class="text-center mb-12">
          <h1 class="text-5xl font-bold text-gray-800 dark:text-gray-100 mb-4">
            fsend
          </h1>
          <p class="text-sm text-gray-500 dark:text-neutral-400 mt-2 mb-6">
            Direct peer-to-peer transfers in your browser using WebRTC
          </p>
          <p class="text-sm text-gray-500 dark:text-neutral-400">
            No filesize limits &middot; End-to-end encrypted &middot; Free
          </p>
        </div>

        <Show when={!supportsFileSystemAccess()}>
          <div class="bg-yellow-100 dark:bg-yellow-900/40 text-yellow-800 dark:text-yellow-200 border border-yellow-300 dark:border-yellow-700 px-3 py-2 rounded-lg text-sm font-medium mb-6">
            Your browser has limited support. Folders will be downloaded as zip
            files and transfer resumption won't be available when receiving.
          </div>
        </Show>

        <div class="space-y-4 flex flex-col">
          <A
            href="/send"
            class="bg-orange-950 dark:bg-orange-900 rounded-xl border-none cursor-pointer font-bold text-lg group"
          >
            <span
              class={`block box-border border-2 border-orange-900 dark:border-orange-700 rounded-xl py-3 px-6 bg-orange-100 text-orange-900 transition-all duration-150 dark:bg-orange-900/80 dark:text-orange-100 text-center ${
                isDragging()
                  ? "bg-orange-200 -translate-y-0.5 dark:bg-orange-800/80"
                  : "group-hover:bg-orange-200 group-hover:-translate-y-0.5 dark:group-hover:bg-orange-800/80"
              }`}
            >
              <FiUpload class="inline-block w-5 h-5 mr-2 -mt-1" />
              {isDragging() ? "Drop to Send" : "Send Files"}
            </span>
          </A>

          <A
            href="/receive"
            class="bg-blue-950 dark:bg-blue-900 rounded-xl border-none cursor-pointer font-bold text-lg group"
          >
            <span class="block box-border border-2 border-blue-900 dark:border-blue-700 rounded-xl py-3 px-6 bg-blue-100 text-blue-900 transition-all duration-150 group-hover:bg-blue-200 group-hover:-translate-y-0.5 dark:bg-blue-900/80 dark:text-blue-100 dark:group-hover:bg-blue-800/80 text-center">
              <FiDownload class="inline-block w-5 h-5 mr-2 -mt-1" />
              Receive Files
            </span>
          </A>
        </div>
      </div>
    </div>
  );
}
