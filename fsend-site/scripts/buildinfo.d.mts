export interface BuildInfo {
  /** Short SHA for display, or null when no commit could be resolved. */
  commit: string | null;
  /** Full SHA, used to build the GitHub commit link. */
  commitFull: string | null;
  /** Preformatted UTC timestamp, e.g. "2026-08-18 08:00 UTC". */
  builtAt: string;
  /** Same instant as epoch millis, for computing the age client-side. */
  builtAtMs: number;
}

export function generateBuildInfo(): BuildInfo;
export function loadBuildInfo(): BuildInfo;
