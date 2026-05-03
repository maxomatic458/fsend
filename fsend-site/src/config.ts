export const RELAY_URL =
  import.meta.env.VITE_RELAY_URL ?? "ws://relay.fsend.sh/ws";

export const STUN_SERVERS = [
  "stun:stun.l.google.com:19302",
  "stun:stun1.l.google.com:19302",
];

export const PROTO_VERSION = "0.1.0";

export const CHUNK_SIZE = 16384;
export const MAX_DC_PAYLOAD = CHUNK_SIZE - 1;
export const FRAG_MORE = 0x00;
export const FRAG_LAST = 0x01;

export const DATA_CHUNK_SIZE = 16383;
export const MAX_BUFFERED = 1_048_576;

export const SESSION_EXPIRY_SEC = 300;
