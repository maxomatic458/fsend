import { createSignal } from 'solid-js';
import { FiUpload, FiFolder, FiFile } from 'solid-icons/fi';
import { pickFiles, pickDirectory, handleDrop } from '../lib/filePicker';
import type { SelectedEntry } from '../lib/types';
import { Button } from './Button';

interface FileDropZoneProps {
  onFilesSelected: (entries: SelectedEntry[]) => void;
  disabled?: boolean;
}

export function FileDropZone(props: FileDropZoneProps) {
  const [isDragging, setIsDragging] = createSignal(false);
  let dragCounter = 0;

  const onDragEnter = (e: DragEvent) => {
    e.preventDefault();
    if (props.disabled) return;
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

  const onDragOver = (e: DragEvent) => {
    e.preventDefault();
  };

  const onDrop = async (e: DragEvent) => {
    e.preventDefault();
    dragCounter = 0;
    setIsDragging(false);
    if (props.disabled || !e.dataTransfer) return;
    const entries = await handleDrop(e.dataTransfer);
    if (entries.length > 0) props.onFilesSelected(entries);
  };

  const selectFiles = async () => {
    try {
      const entries = await pickFiles();
      if (entries.length > 0) props.onFilesSelected(entries);
    } catch {}
  };

  const selectFolder = async () => {
    try {
      const entry = await pickDirectory();
      props.onFilesSelected([entry]);
    } catch {}
  };

  return (
    <div
      onDragEnter={onDragEnter}
      onDragLeave={onDragLeave}
      onDragOver={onDragOver}
      onDrop={onDrop}
      class={`border-2 border-dashed rounded-xl p-8 text-center transition-colors ${
        isDragging()
          ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/20'
          : 'border-gray-300 dark:border-neutral-600'
      }`}
    >
      <FiUpload class="w-10 h-10 mx-auto mb-3 text-gray-400 dark:text-neutral-500" />
      <p class="text-gray-600 dark:text-gray-400 mb-4">
        {isDragging() ? 'Drop files here' : 'Drag and drop files or folders here'}
      </p>
      <div class="flex gap-3 justify-center">
        <Button variant="gray" onClick={selectFiles}>
          <FiFile class="inline-block w-4 h-4 mr-1 -mt-0.5" />
          Files
        </Button>
        <Button variant="gray" onClick={selectFolder}>
          <FiFolder class="inline-block w-4 h-4 mr-1 -mt-0.5" />
          Folder
        </Button>
      </div>
    </div>
  );
}
