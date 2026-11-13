use zstd::bulk::Decompressor;

use super::frame::{self, Deframer, FrameError, ZSTD};
use super::limits::MAX_BLOCK_BYTES;
use super::{AcceptedFile, Entry};

/// Ahead of a compressed payload: the `u32 BE` count of file bytes in it.
const RAW_LEN_BYTES: usize = 4;

/// A data stream block of `data`, 1 to [`MAX_BLOCK_BYTES`] file bytes, framed
/// like a control message. If `compress` and that helps, its payload is the
/// count of file bytes and their zstd compression.
pub fn encode(data: &[u8], compress: bool) -> Vec<u8> {
    assert!(
        (1..=MAX_BLOCK_BYTES).contains(&data.len()),
        "a block holds 1 to {MAX_BLOCK_BYTES} bytes"
    );
    match compress.then(|| frame::compress(data)).flatten() {
        Some(compressed) if RAW_LEN_BYTES + compressed.len() < data.len() => {
            frame::frame(ZSTD, &[&(data.len() as u32).to_be_bytes(), &compressed])
        }
        _ => frame::frame(0, &[data]),
    }
}

/// Reassembles the file bytes of blocks from data stream bytes split anywhere.
pub struct BlockDecoder {
    deframer: Deframer,
    decompressor: Option<Decompressor<'static>>,
}

impl Default for BlockDecoder {
    fn default() -> Self {
        Self {
            // A compressed payload is smaller than the bytes it holds.
            deframer: Deframer::new(MAX_BLOCK_BYTES + 1),
            decompressor: None,
        }
    }
}

impl BlockDecoder {
    pub fn push(&mut self, data: &[u8]) {
        self.deframer.push(data);
    }

    /// Whether no partial block is buffered.
    pub fn is_empty(&self) -> bool {
        self.deframer.is_empty()
    }

    /// The file bytes of the first buffered block, in proportion to how much of it arrived.
    pub fn partial(&self) -> u64 {
        let Some((len, arrived)) = self.deframer.first() else {
            return 0;
        };
        let raw = match arrived {
            [ZSTD, raw_len @ ..] => match raw_len.first_chunk::<RAW_LEN_BYTES>() {
                Some(raw_len) => u32::from_be_bytes(*raw_len) as usize,
                None => return 0,
            },
            _ => len.saturating_sub(1),
        };
        let raw = raw.min(MAX_BLOCK_BYTES) as u64;
        raw * arrived.len() as u64 / len.max(1) as u64
    }

    /// The file bytes of the next complete block; after an error the stream is unusable.
    pub fn next_block(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let Some((flags, payload)) = self.deframer.next()? else {
            return Ok(None);
        };
        let data = match flags {
            0 => payload,
            ZSTD => {
                let (raw_len, compressed) = payload
                    .split_first_chunk::<RAW_LEN_BYTES>()
                    .ok_or(FrameError::Compression)?;
                let raw_len = u32::from_be_bytes(*raw_len) as usize;
                if raw_len > MAX_BLOCK_BYTES {
                    return Err(FrameError::TooLarge(raw_len));
                }
                let data = self.decompress(compressed, raw_len)?;
                if data.len() != raw_len {
                    return Err(FrameError::Compression);
                }
                data
            }
            other => return Err(FrameError::Flags(other)),
        };
        if data.is_empty() {
            return Err(FrameError::Empty);
        }
        Ok(Some(data))
    }

    /// Fails on more than `capacity` bytes, whatever the payload claims.
    fn decompress(&mut self, payload: &[u8], capacity: usize) -> Result<Vec<u8>, FrameError> {
        if self.decompressor.is_none() {
            self.decompressor = Some(Decompressor::new().map_err(|_| FrameError::Compression)?);
        }
        let decompressor = self.decompressor.as_mut().expect("just created");
        decompressor
            .decompress(payload, capacity)
            .map_err(|_| FrameError::Compression)
    }
}

/// Turns counts of data stream bytes into progress per top-level entry, in stream order.
#[derive(Debug, Clone)]
pub struct Progress {
    /// The top-level entry of each run of accepted bytes, and its length.
    runs: Vec<(usize, u64)>,
    run: usize,
    /// Bytes of the current run already counted.
    used: u64,
}

impl Progress {
    /// For `files` accepted of `entries`, with `top_of` from [`super::offer::top_entries`].
    pub fn new(entries: &[Entry], files: &[AcceptedFile], top_of: &[usize]) -> Self {
        let mut runs = Vec::<(usize, u64)>::new();
        for file in files {
            let id = file.id as usize;
            let bytes = entries[id].size() - file.offset;
            match runs.last_mut() {
                Some((top, run)) if *top == top_of[id] => *run += bytes,
                _ if bytes > 0 => runs.push((top_of[id], bytes)),
                _ => {}
            }
        }
        Self {
            runs,
            run: 0,
            used: 0,
        }
    }

    /// The bytes of each top-level entry among the next `n`; nothing past the end.
    pub fn advance(&mut self, mut n: u64) -> Vec<(usize, u64)> {
        let mut steps = Vec::new();
        while n > 0
            && let Some(&(top, len)) = self.runs.get(self.run)
        {
            let take = n.min(len - self.used);
            steps.push((top, take));
            n -= take;
            self.used += take;
            if self.used == len {
                self.run += 1;
                self.used = 0;
            }
        }
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        let mut decoder = BlockDecoder::default();
        decoder.push(bytes);
        let mut out = Vec::new();
        while let Some(block) = decoder.next_block()? {
            out.push(block);
        }
        assert!(decoder.is_empty());
        Ok(out)
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn raw_block(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut block = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
        block.push(flags);
        block.extend_from_slice(payload);
        block
    }

    #[test]
    fn compressible_blocks_shrink_and_noise_goes_raw() {
        let text = b"fsend ".repeat(MAX_BLOCK_BYTES / 6);
        let packed = encode(&text, true);
        assert_eq!(packed[4], ZSTD);
        assert_eq!(packed[5..9], (text.len() as u32).to_be_bytes());
        assert!(packed.len() < text.len() / 10);

        let random = noise(MAX_BLOCK_BYTES);
        let stored = encode(&random, true);
        assert_eq!(stored, raw_block(0, &random));
        assert_eq!(encode(&text, false), raw_block(0, &text));

        assert_eq!(
            decode_all(&[packed, stored].concat()),
            Ok(vec![text, random])
        );
    }

    #[test]
    fn blocks_split_anywhere_reassemble() {
        let blocks = [b"abc".repeat(1000), noise(5000), vec![7]];
        let bytes: Vec<u8> = blocks.iter().flat_map(|b| encode(b, true)).collect();
        let mut decoder = BlockDecoder::default();
        let mut out = Vec::new();
        for chunk in bytes.chunks(333) {
            decoder.push(chunk);
            while let Some(block) = decoder.next_block().unwrap() {
                out.push(block);
            }
        }
        assert_eq!(out, blocks);
        assert!(decoder.is_empty());
    }

    #[test]
    fn a_partial_block_counts_in_proportion() {
        let text = b"fsend ".repeat(100_000);
        for block in [encode(&text, true), encode(&noise(600_000), true)] {
            let raw = if block[4] == ZSTD {
                text.len()
            } else {
                600_000
            } as u64;
            let mut decoder = BlockDecoder::default();
            assert_eq!(decoder.partial(), 0);
            let half = block.len() / 2;
            decoder.push(&block[..half]);
            let expected = raw * (half - 4) as u64 / (block.len() - 4) as u64;
            assert_eq!(decoder.partial(), expected);
            decoder.push(&block[half..]);
            assert_eq!(decoder.partial(), raw);
            assert!(decoder.next_block().unwrap().is_some());
            assert_eq!(decoder.partial(), 0);
        }
    }

    #[test]
    fn progress_walks_the_accepted_bytes() {
        let file = |path: &str, size| Entry::File {
            path: vec![path.into()],
            size,
        };
        let entries = [file("a", 10), file("b", 0), file("c", 5), file("d", 4)];
        let files = [
            AcceptedFile { id: 0, offset: 2 },
            AcceptedFile { id: 1, offset: 0 },
            AcceptedFile { id: 2, offset: 0 },
            AcceptedFile { id: 3, offset: 0 },
        ];
        // a and b share a top-level entry, c and d do not.
        let mut progress = Progress::new(&entries, &files, &[0, 0, 1, 2]);
        assert_eq!(progress.advance(3), [(0, 3)]);
        assert_eq!(progress.advance(7), [(0, 5), (1, 2)]);
        assert_eq!(progress.advance(0), []);
        assert_eq!(progress.advance(100), [(1, 3), (2, 4)]);
        assert_eq!(progress.advance(1), []);
    }

    #[test]
    fn hostile_blocks_are_rejected() {
        assert_eq!(decode_all(&raw_block(0, b"")), Err(FrameError::Empty));
        assert_eq!(decode_all(&raw_block(2, b"x")), Err(FrameError::Flags(2)));
        assert_eq!(
            decode_all(&raw_block(ZSTD, b"\0\0\0\x08not zstd")),
            Err(FrameError::Compression)
        );
        let too_large = (MAX_BLOCK_BYTES as u32 + 2).to_be_bytes();
        assert_eq!(
            decode_all(&too_large),
            Err(FrameError::TooLarge(MAX_BLOCK_BYTES + 2))
        );
        let compressed = |raw_len: u32, data: &[u8]| {
            let mut payload = raw_len.to_be_bytes().to_vec();
            payload.extend(zstd::bulk::compress(data, 1).unwrap());
            raw_block(ZSTD, &payload)
        };
        // More than it says once decompressed, less, or more than a block may hold.
        let zeros = vec![0; 1000];
        assert_eq!(
            decode_all(&compressed(999, &zeros)),
            Err(FrameError::Compression)
        );
        assert_eq!(
            decode_all(&compressed(1001, &zeros)),
            Err(FrameError::Compression)
        );
        let bomb = vec![0; MAX_BLOCK_BYTES + 1];
        assert_eq!(
            decode_all(&compressed(bomb.len() as u32, &bomb)),
            Err(FrameError::TooLarge(MAX_BLOCK_BYTES + 1))
        );
        assert_eq!(
            decode_all(&raw_block(ZSTD, &[0, 0])),
            Err(FrameError::Compression)
        );
    }

    #[test]
    #[should_panic(expected = "a block holds")]
    fn empty_blocks_are_not_encoded() {
        encode(&[], true);
    }
}
