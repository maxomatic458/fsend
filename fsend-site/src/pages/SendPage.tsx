import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { FiArrowLeft, FiSend, FiFile, FiFolder, FiPlus } from 'solid-icons/fi';
import type { SelectedEntry } from '../lib/types';
import { pickFiles, pickDirectory, handleDrop } from '../lib/filePicker';
import { runSender } from '../lib/sender';
import { createProgressTracker } from '../primitives/createProgressTracker';
import { SESSION_EXPIRY_SEC } from '../config';
import { FileList } from '../components/FileList';
import { ShareCode } from '../components/ShareCode';
import { TransferProgress } from '../components/TransferProgress';
import { ErrorCard } from '../components/ErrorCard';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { formatBytes, formatTime } from '../lib/format';
import { buildFileTree, totalSize as computeTotalSize } from '../lib/fileTree';

type SendState =
  | 'selecting'
  | 'connecting'
  | 'waiting'
  | 'handshaking'
  | 'waitingAccept'
  | 'transferring'
  | 'completed'
  | 'error';

export function SendPage() {
  const navigate = useNavigate();
  const tracker = createProgressTracker();

  const [state, setState] = createSignal<SendState>('selecting');
  const [entries, setEntries] = createSignal<SelectedEntry[]>([]);
  const [shareCode, setShareCode] = createSignal('');
  const [expiresAt, setExpiresAt] = createSignal(0);
  const [error, setError] = createSignal('');
  const [connectionType, setConnectionType] = createSignal<string>('unknown');
  const [fileTotalSize, setFileTotalSize] = createSignal(0);
  const [startTime, setStartTime] = createSignal(0);
  const [isDragging, setIsDragging] = createSignal(false);
  const [isProcessing, setIsProcessing] = createSignal(false);
  let dragCounter = 0;

  const abortController = new AbortController();

  onMount(() => {
    const pending = (window as any).__fsend_pending as SelectedEntry[] | undefined;
    if (pending && pending.length > 0) {
      setEntries(pending);
      updateTotalSize(pending);
      delete (window as any).__fsend_pending;
    }
  });

  onCleanup(() => {
    abortController.abort();
    tracker.cleanup();
  });

  const updateTotalSize = async (items: SelectedEntry[]) => {
    const tree = await buildFileTree(items);
    setFileTotalSize(computeTotalSize(tree));
  };

  const addFiles = (newEntries: SelectedEntry[]) => {
    const updated = [...entries(), ...newEntries];
    setEntries(updated);
    updateTotalSize(updated);
  };

  const removeEntry = (index: number) => {
    const updated = entries().filter((_, i) => i !== index);
    setEntries(updated);
    updateTotalSize(updated);
  };

  const handleAddFiles = async () => {
    try {
      const picked = await pickFiles();
      if (picked.length > 0) addFiles(picked);
    } catch {}
  };

  const handleAddDirectory = async () => {
    try {
      const entry = await pickDirectory();
      addFiles([entry]);
    } catch {}
  };

  // Drag-and-drop handlers for the full page
  const onDragEnter = (e: DragEvent) => {
    e.preventDefault();
    if (state() !== 'selecting') return;
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
    if (state() !== 'selecting' || !e.dataTransfer) return;
    setIsProcessing(true);
    try {
      const dropped = await handleDrop(e.dataTransfer);
      if (dropped.length > 0) addFiles(dropped);
    } finally {
      setIsProcessing(false);
    }
  };

  const startSending = () => {
    if (entries().length === 0) return;
    setState('connecting');
    setStartTime(Date.now());

    runSender(entries(), {
      onCode: (code) => {
        setShareCode(code);
        setExpiresAt(Date.now() + SESSION_EXPIRY_SEC * 1000);
        setState('waiting');
      },
      onWaitingPeer: () => {},
      onHandshaking: () => setState('handshaking'),
      onWaitingAccept: () => setState('waitingAccept'),
      onTransferring: (items) => {
        tracker.initialize(items);
        setState('transferring');
      },
      onProgress: (bytes) => tracker.recordBytes(bytes),
      onComplete: () => setState('completed'),
      onError: (msg) => {
        setError(msg);
        setState('error');
      },
      onConnectionType: (type) => setConnectionType(type),
    }, abortController.signal);
  };

  const goBack = () => { abortController.abort(); navigate('/'); };

  const reset = () => {
    setState('selecting');
    setEntries([]);
    setShareCode('');
    setError('');
    setFileTotalSize(0);
  };

  const elapsed = () => Math.floor((Date.now() - startTime()) / 1000);

  return (
    <div
      class="flex-1 bg-indigo-100 dark:bg-neutral-900 py-8 px-4 transition-colors relative"
      onDragEnter={onDragEnter}
      onDragLeave={onDragLeave}
      onDragOver={onDragOver}
      onDrop={onDrop}
    >
      {/* Full-page drag overlay */}
      <div
        class={`absolute inset-4 border-2 border-dashed rounded-xl z-10 flex items-center justify-center pointer-events-none transition-all duration-200 ${
          isDragging()
            ? 'opacity-100 bg-blue-500/50 dark:bg-blue-500/10 border-blue-400/50 dark:border-blue-400/40'
            : 'opacity-0 bg-transparent border-transparent'
        }`}
      >
        <div
          class={`text-center text-blue-500/70 dark:text-blue-400/70 transition-opacity duration-200 ${
            isDragging() ? 'opacity-100' : 'opacity-0'
          }`}
        >
          <FiPlus class="w-12 h-12 mx-auto mb-3" />
          <div class="text-xl font-medium">Drop files or folders here</div>
        </div>
      </div>

      <div class="max-w-2xl mx-auto relative z-0">
        {/* Page header */}
        <button
          onClick={goBack}
          class="mb-6 text-blue-600 dark:text-blue-400 hover:text-blue-800 dark:hover:text-blue-300 flex items-center gap-2 transition-colors cursor-pointer"
        >
          <FiArrowLeft class="w-4 h-4" /> Back
        </button>
        <h1 class="text-3xl font-bold text-gray-800 dark:text-gray-100 mb-8">Send Files</h1>

        {/* State: selecting */}
        <Show when={state() === 'selecting'}>
          <Card class="mb-6">
            <h2 class="text-xl font-semibold mb-4 text-gray-800 dark:text-gray-100">
              Select Files or Directories
            </h2>

            <Show when={entries().length > 0}>
              <FileList entries={entries()} onRemove={removeEntry} totalSize={fileTotalSize()} />
              <hr class="border-gray-200 dark:border-neutral-700 mb-6" />
            </Show>

            {/* Drag-drop zone inside the card */}
            <div class="border-2 border-dashed rounded-lg p-8 mb-6 text-center border-gray-300 dark:border-neutral-600">
              <Show
                when={!isProcessing()}
                fallback={
                  <div class="text-blue-600 dark:text-blue-400 flex flex-col items-center">
                    <div class="animate-spin w-12 h-12 border-4 border-gray-300 border-t-blue-500 rounded-full" />
                    <div class="font-medium mt-3">Processing files...</div>
                  </div>
                }
              >
                <div class="text-gray-500 dark:text-gray-400">
                  <FiFolder class="w-10 h-10 mx-auto mb-2" />
                  <div>Drag and drop files or folders</div>
                </div>
              </Show>
            </div>

            <div class="flex gap-4">
              <Button variant="blue" onClick={handleAddFiles} class="flex-1 py-3">
                <span class="flex items-center justify-center gap-2">
                  <FiFile class="w-5 h-5" />
                  Add Files
                </span>
              </Button>
              <Button variant="green" onClick={handleAddDirectory} class="flex-1 py-3">
                <span class="flex items-center justify-center gap-2">
                  <FiFolder class="w-5 h-5" />
                  Add Folder
                </span>
              </Button>
            </div>

            <Show when={entries().length > 0}>
              <Button variant="orange" onClick={startSending} class="w-full py-3 mt-6">
                <span class="flex items-center justify-center gap-2">
                  <FiSend class="w-5 h-5" />
                  Generate Share Code
                </span>
              </Button>
            </Show>
          </Card>
        </Show>

        {/* State: connecting */}
        <Show when={state() === 'connecting'}>
          <Card class="text-center">
            <div class="flex justify-center mb-4">
              <div class="animate-spin w-12 h-12 border-4 border-gray-300 border-t-blue-500 rounded-full" />
            </div>
            <p class="text-gray-600 dark:text-gray-400 mb-4">Creating session...</p>
            <Button variant="gray" onClick={goBack}>
              Cancel
            </Button>
          </Card>
        </Show>

        {/* State: waiting for peer */}
        <Show when={state() === 'waiting'}>
          <ShareCode
            code={shareCode()}
            expiresAt={expiresAt()}
            onCancel={() => { abortController.abort(); reset(); }}
          />
        </Show>

        {/* State: handshaking */}
        <Show when={state() === 'handshaking'}>
          <Card class="text-center">
            <div class="flex justify-center mb-4">
              <div class="animate-spin w-12 h-12 border-4 border-gray-300 border-t-blue-500 rounded-full" />
            </div>
            <p class="text-gray-600 dark:text-gray-400">Establishing connection...</p>
          </Card>
        </Show>

        {/* State: waiting accept */}
        <Show when={state() === 'waitingAccept'}>
          <Card class="text-center">
            <div class="flex justify-center mb-4">
              <div class="animate-spin w-12 h-12 border-4 border-gray-300 border-t-blue-500 rounded-full" />
            </div>
            <p class="text-gray-600 dark:text-gray-400">Waiting for receiver to accept...</p>
          </Card>
        </Show>

        {/* State: transferring / completed */}
        <Show when={state() === 'transferring' || state() === 'completed'}>
          <Card>
            <Show when={connectionType() !== 'unknown'}>
              <div class="flex items-center justify-center gap-2 mb-4">
                <div class={`w-3 h-3 rounded-full ${connectionType() === 'direct' ? 'bg-green-500' : 'bg-yellow-500'}`} />
                <span
                  class={`text-sm font-medium ${
                    connectionType() === 'direct'
                      ? 'text-green-600 dark:text-green-400'
                      : 'text-yellow-600 dark:text-yellow-400'
                  }`}
                >
                  {connectionType() === 'direct' ? 'Direct Connection' : 'Relay Connection'}
                </span>
              </div>
            </Show>
            <TransferProgress
              progress={tracker.progress}
              status={state() === 'completed' ? 'Transfer Complete!' : 'Sending...'}
              speedLabel="Upload"
            />

            <Show when={state() === 'transferring'}>
              <div class="mt-6 text-center">
                <p class="text-sm text-gray-500 dark:text-gray-400 mb-3">
                  Please keep this page open until the transfer completes
                </p>
              </div>
            </Show>

            <Show when={state() === 'completed'}>
              <div class="mt-6 text-center">
                <p class="text-green-600 dark:text-green-400 font-semibold text-lg mb-4">
                  All files sent successfully!
                </p>
                <Button variant="blue" onClick={goBack}>
                  Back to Home
                </Button>
              </div>
            </Show>
          </Card>
        </Show>

        {/* State: error */}
        <Show when={state() === 'error'}>
          <ErrorCard class="text-center">
            <p class="text-red-600 dark:text-red-400 font-semibold mb-4">{error()}</p>
            <Button variant="red" onClick={reset}>
              Try Again
            </Button>
          </ErrorCard>
        </Show>
      </div>
    </div>
  );
}
