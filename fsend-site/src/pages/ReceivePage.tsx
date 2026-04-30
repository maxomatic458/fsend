import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { useNavigate, useParams } from '@solidjs/router';
import { FiArrowLeft, FiDownload, FiFolder, FiLink } from 'solid-icons/fi';
import type { FilesAvailable } from '../lib/types';
import { supportsFileSystemAccess } from '../lib/fsAccess';
import { pickSaveDirectory } from '../lib/filePicker';
import { runReceiver } from '../lib/receiver';
import { runFallbackReceiver } from '../lib/fallbackReceiver';
import { createProgressTracker } from '../primitives/createProgressTracker';
import { FileOffer } from '../components/FileOffer';
import { TransferProgress } from '../components/TransferProgress';
import { ErrorCard } from '../components/ErrorCard';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { formatBytes } from '../lib/format';

type ReceiveState =
  | 'input'
  | 'connecting'
  | 'handshaking'
  | 'offered'
  | 'transferring'
  | 'completed'
  | 'error';

export function ReceivePage() {
  const navigate = useNavigate();
  const params = useParams<{ code?: string }>();
  const tracker = createProgressTracker();
  const hasNativeFS = supportsFileSystemAccess();

  const [state, setState] = createSignal<ReceiveState>('input');
  const [code, setCode] = createSignal('');
  const [error, setError] = createSignal('');
  const [offeredFiles, setOfferedFiles] = createSignal<FilesAvailable[]>([]);
  const [acceptFn, setAcceptFn] = createSignal<(() => void) | null>(null);
  const [rejectFn, setRejectFn] = createSignal<(() => void) | null>(null);
  const [dirHandle, setDirHandle] = createSignal<FileSystemDirectoryHandle | null>(null);
  const [connectionType, setConnectionType] = createSignal<string>('unknown');
  const [resume, setResume] = createSignal(false);

  const abortController = new AbortController();

  onMount(() => {
    if (params.code) {
      setCode(params.code.toUpperCase());
    }
  });

  onCleanup(() => {
    abortController.abort();
    tracker.cleanup();
  });

  const formatCode = (input: string) => {
    return input.toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 8);
  };

  const selectSaveDir = async () => {
    try {
      const handle = await pickSaveDirectory();
      setDirHandle(handle);
    } catch {}
  };

  const startReceiving = () => {
    if (code().length === 0) return;
    setState('connecting');

    const callbacks = {
      onConnecting: () => setState('connecting'),
      onHandshaking: () => setState('handshaking'),
      onFilesOffered: (files: FilesAvailable[], accept: () => void, reject: () => void) => {
        setOfferedFiles(files);
        setAcceptFn(() => accept);
        setRejectFn(() => reject);
        setState('offered');
      },
      onTransferring: (items: Array<{ name: string; size: number; skip: number; isDir: boolean }>) => {
        tracker.initialize(items);
        setState('transferring');
      },
      onProgress: (bytes: number) => tracker.recordBytes(bytes),
      onComplete: () => setState('completed'),
      onError: (msg: string) => {
        setError(msg);
        setState('error');
      },
      onConnectionType: (type: 'direct' | 'relay' | 'unknown') => setConnectionType(type),
    };

    if (hasNativeFS && dirHandle()) {
      runReceiver(code(), dirHandle()!, resume(), callbacks, abortController.signal);
    } else {
      runFallbackReceiver(code(), callbacks, abortController.signal);
    }
  };

  const handleAccept = async () => {
    if (hasNativeFS && !dirHandle()) {
      try {
        const handle = await pickSaveDirectory();
        setDirHandle(handle);
      } catch {
        return;
      }
    }
    acceptFn()?.();
  };

  const handleReject = () => {
    rejectFn()?.();
  };

  const goBack = () => { abortController.abort(); navigate('/'); };

  const handleTryAgain = () => {
    setError('');
    setState('input');
  };

  return (
    <div class="flex-1 bg-indigo-100 dark:bg-neutral-900 py-8 px-4 transition-colors">
      <div class="max-w-2xl mx-auto">
        {/* Page header */}
        <button
          onClick={goBack}
          class="mb-6 text-blue-600 dark:text-blue-400 hover:text-blue-800 dark:hover:text-blue-300 flex items-center gap-2 transition-colors cursor-pointer"
        >
          <FiArrowLeft class="w-4 h-4" /> Back
        </button>
        <h1 class="text-3xl font-bold text-gray-800 dark:text-gray-100 mb-8">Receive Files</h1>

        {/* State: input */}
        <Show when={state() === 'input'}>
          <Show when={!hasNativeFS}>
            <div class="bg-yellow-100 dark:bg-yellow-900/40 text-yellow-800 dark:text-yellow-200 border border-yellow-300 dark:border-yellow-700 px-3 py-2 rounded-lg text-sm font-medium mb-4">
              Your browser doesn't support the File System Access API.
              Files will be downloaded as a zip archive. Transfer resumption won't be available.
            </div>
          </Show>

          <Card class="mb-6">
            <h2 class="text-xl font-semibold mb-4 text-gray-800 dark:text-gray-100">
              Enter Share Code
            </h2>

            <div class="mb-6">
              <label class="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
                Code from sender
              </label>
              <input
                type="text"
                value={code()}
                onInput={(e) => setCode(formatCode(e.currentTarget.value))}
                placeholder="ABCD1234"
                class="w-full p-4 border dark:border-neutral-600 rounded-lg text-2xl font-mono text-center tracking-widest uppercase bg-white dark:bg-neutral-700 text-gray-800 dark:text-gray-100"
                maxLength={8}
              />
            </div>

            <Show when={hasNativeFS}>
              <div class="mb-6">
                <label class="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
                  Download Location
                </label>
                <div class="flex gap-2">
                  <div class="flex-1 p-3 border dark:border-neutral-600 rounded-lg bg-gray-50 dark:bg-neutral-700 text-gray-800 dark:text-gray-200">
                    {dirHandle() ? dirHandle()!.name : 'No directory selected'}
                  </div>
                  <Button variant="blue" onClick={selectSaveDir}>
                    <span class="flex items-center gap-2">
                      <FiFolder class="w-4 h-4" />
                      Select Folder
                    </span>
                  </Button>
                </div>

                <label class="flex items-center gap-2 mt-3 text-sm text-gray-500 dark:text-neutral-400">
                  <input
                    type="checkbox"
                    checked={resume()}
                    onChange={(e) => setResume(e.currentTarget.checked)}
                    class="rounded"
                  />
                  Resume interrupted transfer
                </label>
              </div>
            </Show>

            <Show when={!hasNativeFS}>
              <div class="mb-6 p-3 bg-gray-50 dark:bg-neutral-700 rounded-lg text-gray-600 dark:text-gray-300 text-sm">
                <FiDownload class="w-4 h-4 inline-block mr-2" />
                Files will be automatically downloaded to your Downloads folder
              </div>
            </Show>

            <Button
              variant="green"
              onClick={startReceiving}
              disabled={!code() || code().length !== 8 || (hasNativeFS && !dirHandle())}
              class="w-full py-3"
            >
              <span class="flex items-center justify-center gap-2">
                <FiLink class="w-5 h-5" />
                Connect & Receive
              </span>
            </Button>
          </Card>
        </Show>

        {/* State: connecting */}
        <Show when={state() === 'connecting'}>
          <Card class="text-center">
            <div class="flex justify-center mb-4">
              <div class="animate-spin w-12 h-12 border-4 border-gray-300 border-t-blue-500 rounded-full" />
            </div>
            <p class="text-gray-600 dark:text-gray-400 mb-4">Connecting to sender...</p>
            <Button variant="gray" onClick={goBack}>
              Cancel
            </Button>
          </Card>
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

        {/* State: offered */}
        <Show when={state() === 'offered'}>
          <Card>
            <FileOffer
              files={offeredFiles()}
              onAccept={handleAccept}
              onReject={handleReject}
            />
          </Card>
        </Show>

        {/* State: transferring / completed */}
        <Show when={state() === 'transferring' || state() === 'completed'}>
          <Show when={!hasNativeFS}>
            <div class="bg-yellow-100 dark:bg-yellow-900/40 text-yellow-800 dark:text-yellow-200 border border-yellow-300 dark:border-yellow-700 px-3 py-2 rounded-lg text-sm font-medium mb-4">
              Your browser does not support the Native Filesystem API. The entire download (
              {formatBytes(tracker.progress.totalSize)}) needs to be saved to memory first. Ensure
              you have enough free system memory.
            </div>
          </Show>
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
              status={state() === 'completed' ? 'Download Complete!' : 'Receiving...'}
              speedLabel="Download"
            />

            <Show when={state() === 'transferring'}>
              <div class="mt-6 text-center">
                <Show when={hasNativeFS}>
                  <p class="text-sm text-gray-500 dark:text-gray-400 mb-3">
                    Canceling will save progress and allow resuming later
                  </p>
                  <Button variant="red" onClick={goBack}>
                    Cancel Transfer
                  </Button>
                </Show>
                <Show when={!hasNativeFS}>
                  <p class="text-sm text-gray-500 dark:text-gray-400">
                    Please keep this page open until the transfer completes
                  </p>
                </Show>
              </div>
            </Show>

            <Show when={state() === 'completed'}>
              <div class="mt-6 text-center">
                <p class="text-green-600 dark:text-green-400 font-semibold text-lg mb-4">
                  All files received successfully!
                </p>
                <Show when={hasNativeFS && dirHandle()}>
                  <p class="text-gray-600 dark:text-gray-400 mb-4">
                    Files saved to: {dirHandle()?.name}
                  </p>
                </Show>
                <Show when={!hasNativeFS}>
                  <p class="text-gray-600 dark:text-gray-400 mb-4">
                    Files downloaded to your Downloads folder
                  </p>
                </Show>
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
            <Button variant="red" onClick={handleTryAgain}>
              Try Again
            </Button>
          </ErrorCard>
        </Show>
      </div>
    </div>
  );
}
