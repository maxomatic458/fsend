const BYTE_UNITS = [
  ["KB", 1],
  ["MB", 1],
  ["GB", 2],
] as const;

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  let value = bytes / 1024;
  let unit = 0;
  // Rounded before picking the unit, so it never shows "1024.0 KB".
  while (
    unit < BYTE_UNITS.length - 1 &&
    Number(value.toFixed(BYTE_UNITS[unit][1])) >= 1024
  ) {
    value /= 1024;
    unit++;
  }
  const [name, digits] = BYTE_UNITS[unit];
  return `${value.toFixed(digits)} ${name}`;
}

export function formatSpeed(bytesPerSec: number): string {
  return `${formatBytes(bytesPerSec)}/s`;
}

/// Long-form age for the footer's deploy stamp: "2 days, 0 hours, 5 minutes ago".
/// Units above the largest non-zero one are dropped, so a fresh deploy reads
/// "5 minutes ago" rather than padding out with zeroes.
export function formatAge(ms: number): string {
  const totalMinutes = Math.floor(ms / 60_000);
  if (totalMinutes < 1) return "just now";

  const days = Math.floor(totalMinutes / 1440);
  const hours = Math.floor((totalMinutes % 1440) / 60);
  const minutes = totalMinutes % 60;

  const parts: string[] = [];
  if (days > 0) parts.push(pluralize(days, "day"));
  if (days > 0 || hours > 0) parts.push(pluralize(hours, "hour"));
  parts.push(pluralize(minutes, "minute"));

  return `${parts.join(", ")} ago`;
}

function pluralize(n: number, unit: string): string {
  return `${n} ${unit}${n === 1 ? "" : "s"}`;
}

export function formatTime(seconds: number): string {
  if (seconds < 1) return "< 1s";
  // Rounded before splitting, so it never shows "1m 60s".
  const total = Math.round(seconds);
  if (total < 60) return `${total}s`;
  if (total < 3600) return `${Math.floor(total / 60)}m ${total % 60}s`;
  const minutes = Math.round(total / 60);
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}
