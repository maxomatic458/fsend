/// The fsend server: it pairs the peers and counts the bytes receivers got.
const API_URL = (import.meta.env.VITE_API_URL ?? "https://api.fsend.sh").replace(/\/+$/, "");
export const RELAY_URL = `${API_URL.replace(/^http/, "ws")}/relay`;
export const STATS_URL = `${API_URL}/stats`;

export const STUN_SERVERS = ["stun:stun.l.google.com:19302", "stun:stun1.l.google.com:19302"];

export const ICE_GATHERING_TIMEOUT_MS = 5_000;
export const CONNECT_TIMEOUT_MS = 30_000;
/// Largest data channel message every browser takes.
export const MAX_MESSAGE_BYTES = 16 * 1024;
/// Data stream message cap, where the peer's SCTP allows it.
export const MAX_DATA_MESSAGE_BYTES = 256 * 1024;
export const MAX_BUFFERED_BYTES = 1_048_576;
export const CONCURRENT_FILES = 8;
