import { FiFile, FiFolder } from "solid-icons/fi";

/** A file or folder icon and its name, cut short to fit. */
export function EntryName(props: {
  name: string;
  dir: boolean;
  size?: string;
}) {
  return (
    <div class="flex items-center gap-3 min-w-0">
      {props.dir ? (
        <FiFolder class="w-4 h-4 text-ink-faint shrink-0" />
      ) : (
        <FiFile class="w-4 h-4 text-ink-faint shrink-0" />
      )}
      <span class={`${props.size ?? "text-[15.5px]"} truncate`}>
        {props.name}
      </span>
    </div>
  );
}
