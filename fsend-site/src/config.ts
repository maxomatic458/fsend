/// Base URL of the relay; the protocol path is appended.
export const RELAY_URL =
  import.meta.env.VITE_RELAY_URL ?? "wss://relay.fsend.sh";

export const STUN_SERVERS = [
  "stun:stun.l.google.com:19302",
  "stun:stun1.l.google.com:19302",
];

export const ICE_GATHERING_TIMEOUT_MS = 5_000;
export const CONNECT_TIMEOUT_MS = 30_000;
/// Largest data channel message every browser takes.
export const MAX_MESSAGE_BYTES = 16 * 1024;
/// Largest data stream message, where both ends take it: fewer messages move faster.
export const MAX_DATA_MESSAGE_BYTES = 256 * 1024;
export const MAX_BUFFERED_BYTES = 1_048_576;
export const CONCURRENT_FILES = 8;
