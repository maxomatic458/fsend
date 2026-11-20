use std::time::Duration;

/// Largest control frame, flags byte included.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Largest message after decompression.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Control messages above this are compressed if that helps.
pub const COMPRESS_ABOVE: usize = 1024;

pub const MAX_ENTRIES: usize = 1_000_000;
/// Components in a path; each nests the offer's JSON two levels deeper, of 127.
pub const MAX_DEPTH: usize = 60;
pub const MAX_COMPONENT_BYTES: usize = 255;
/// A path's components joined with `/`.
pub const MAX_PATH_BYTES: usize = 4096;
/// Largest offer total: the largest integer a JavaScript number holds exactly.
pub const MAX_TOTAL_BYTES: u64 = (1 << 53) - 1;

/// The receiver's window: unacknowledged bytes it buffers.
pub const WINDOW: u64 = 16 * 1024 * 1024;
/// The smallest window a sender takes.
pub const MIN_WINDOW: u64 = 64 * 1024;
/// Unacknowledged bytes that make the receiver ack.
pub const ACK_STEP: u64 = WINDOW / 4;
/// Longest written bytes wait for an ack.
pub const ACK_INTERVAL: Duration = Duration::from_secs(1);

/// Longest gap without data or acks after the accept.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// For the peer to read our last message and hang up.
pub const GOODBYE_TIMEOUT: Duration = Duration::from_secs(5);

/// File bytes in a data stream block.
pub const MAX_BLOCK_BYTES: usize = 1024 * 1024;
