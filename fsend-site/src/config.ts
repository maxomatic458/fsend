export const RELAY_URL =
  import.meta.env.VITE_RELAY_URL ?? "wss://relay.fsend.sh/ws";

export const STUN_SERVERS = [
  "stun:stun.l.google.com:19302",
  "stun:stun1.l.google.com:19302",
];

export const ICE_GATHERING_TIMEOUT_MS = 5_000;
export const CONNECT_TIMEOUT_MS = 30_000;
/// How long the peer gets to read our last message before we hang up.
export const GOODBYE_TIMEOUT_MS = 5_000;
export const PROTO_VERSION = "4";
/// Largest data channel message every peer accepts.
export const MAX_MESSAGE_BYTES = 16 * 1024;
export const FRAME_HEADER_BYTES = 4;
export const DATA_CHUNK_BYTES = 16383;
export const MAX_BUFFERED_BYTES = 1_048_576;
export const SESSION_EXPIRY_SECS = 300;
