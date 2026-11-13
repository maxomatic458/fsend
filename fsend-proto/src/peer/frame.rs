use std::{borrow::Cow, io::Read};

use thiserror::Error;

use super::{
    Message,
    limits::{COMPRESS_ABOVE, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES},
};

const HEADER_BYTES: usize = 4;
const ZSTD: u8 = 1;
const ZSTD_LEVEL: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameError {
    #[error("a frame has length zero")]
    Empty,
    #[error("a frame of {0} bytes is over the limit")]
    TooLarge(usize),
    #[error("unknown frame flags {0:#04x}")]
    Flags(u8),
    #[error("a message is over the size limit")]
    MessageTooLarge,
    #[error("a compressed payload is corrupt")]
    Compression,
    #[error("malformed message: {0}")]
    Json(String),
}

/// `u32 BE length | u8 flags | payload`, the length counting flags and payload.
pub fn encode(message: &impl Message) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(message).expect("messages serialize");
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(FrameError::MessageTooLarge);
    }
    let (flags, payload) = if json.len() > COMPRESS_ABOVE {
        let compressed =
            zstd::bulk::compress(&json, ZSTD_LEVEL).map_err(|_| FrameError::Compression)?;
        (ZSTD, compressed)
    } else {
        (0, json)
    };
    let len = payload.len() + 1;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut frame = Vec::with_capacity(HEADER_BYTES + len);
    frame.extend_from_slice(&(len as u32).to_be_bytes());
    frame.push(flags);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Reassembles messages from control stream bytes split anywhere.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// The next complete message; after an error the stream is unusable.
    pub fn next_message<M: Message>(&mut self) -> Result<Option<M>, FrameError> {
        let Some(header) = self.buffer.first_chunk::<HEADER_BYTES>() else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(*header) as usize;
        if len == 0 {
            return Err(FrameError::Empty);
        }
        if len > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge(len));
        }
        let Some(frame) = self.buffer.get(HEADER_BYTES..HEADER_BYTES + len) else {
            return Ok(None);
        };
        let message = decode(frame);
        self.buffer.drain(..HEADER_BYTES + len);
        message.map(Some)
    }
}

fn decode<M: Message>(frame: &[u8]) -> Result<M, FrameError> {
    let (&flags, payload) = frame.split_first().ok_or(FrameError::Empty)?;
    let json = match flags {
        0 => Cow::Borrowed(payload),
        ZSTD => Cow::Owned(decompress(payload)?),
        other => return Err(FrameError::Flags(other)),
    };
    serde_json::from_slice(&json).map_err(|e| FrameError::Json(e.to_string()))
}

/// Never allocates more than the message limit, whatever the frame claims.
fn decompress(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    let decoder =
        zstd::stream::read::Decoder::with_buffer(payload).map_err(|_| FrameError::Compression)?;
    let mut json = Vec::new();
    decoder
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut json)
        .map_err(|_| FrameError::Compression)?;
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(FrameError::MessageTooLarge);
    }
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::{
        AcceptedFile, Entry, ErrorCode, ErrorReport, ReceiverMessage, SenderMessage,
    };

    fn decode_all<M: Message>(bytes: &[u8]) -> Result<Vec<M>, FrameError> {
        let mut decoder = FrameDecoder::default();
        decoder.push(bytes);
        let mut out = Vec::new();
        while let Some(message) = decoder.next_message()? {
            out.push(message);
        }
        Ok(out)
    }

    /// Frames from the receiver.
    fn from_receiver(bytes: &[u8]) -> Result<Vec<ReceiverMessage>, FrameError> {
        decode_all(bytes)
    }

    fn big_offer(files: usize) -> SenderMessage {
        SenderMessage::Offer {
            entries: (0..files)
                .map(|i| Entry::File {
                    path: vec![format!("file-{i}.bin")],
                    size: i as u64,
                })
                .collect(),
        }
    }

    fn raw_frame(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
        frame.push(flags);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn small_messages_go_uncompressed() {
        let frame = encode(&ReceiverMessage::Done).unwrap();
        assert_eq!(frame, raw_frame(0, br#"{"type":"done"}"#));
    }

    #[test]
    fn large_messages_are_compressed_and_round_trip() {
        let offer = big_offer(5000);
        let frame = encode(&offer).unwrap();
        assert_eq!(frame[4], ZSTD);
        assert!(frame.len() < serde_json::to_vec(&offer).unwrap().len() / 2);
        assert_eq!(decode_all(&frame), Ok(vec![offer]));
    }

    #[test]
    fn frames_split_anywhere_reassemble() {
        let messages = [
            ReceiverMessage::Done,
            ReceiverMessage::Accept {
                files: (0..300).map(|id| AcceptedFile { id, offset: 0 }).collect(),
            },
            ReceiverMessage::Ack { bytes: 7 },
        ];
        let bytes: Vec<u8> = messages.iter().flat_map(|m| encode(m).unwrap()).collect();
        let mut decoder = FrameDecoder::default();
        let mut out = Vec::<ReceiverMessage>::new();
        for byte in bytes {
            decoder.push(&[byte]);
            while let Some(message) = decoder.next_message().unwrap() {
                out.push(message);
            }
        }
        assert_eq!(out, messages);
    }

    #[test]
    fn hostile_frames_are_rejected() {
        assert_eq!(from_receiver(&[0, 0, 0, 0]), Err(FrameError::Empty));
        let too_large = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        assert_eq!(
            from_receiver(&too_large),
            Err(FrameError::TooLarge(MAX_FRAME_BYTES + 1))
        );
        assert_eq!(
            from_receiver(&raw_frame(2, b"{}")),
            Err(FrameError::Flags(2))
        );
        assert_eq!(
            from_receiver(&raw_frame(ZSTD, b"not zstd")),
            Err(FrameError::Compression)
        );
        for payload in [
            &b"not json"[..],
            // A message type without its fields.
            br#"{"type":"ack"}"#,
            // An unknown type, and one only the sender sends.
            br#"{"type":"later","x":[1]}"#,
            br#"{"type":"offer","entries":[]}"#,
            // Flags without a payload.
            b"",
        ] {
            assert!(
                matches!(
                    from_receiver(&raw_frame(0, payload)),
                    Err(FrameError::Json(_))
                ),
                "{payload:?}"
            );
        }
        assert_eq!(
            from_receiver(&raw_frame(0x81, b"{}")),
            Err(FrameError::Flags(0x81))
        );
    }

    #[test]
    fn small_payloads_may_be_compressed_too() {
        let small = zstd::bulk::compress(br#"{"type":"done"}"#, 1).unwrap();
        assert_eq!(
            from_receiver(&raw_frame(ZSTD, &small)),
            Ok(vec![ReceiverMessage::Done])
        );
    }

    #[test]
    fn a_compression_bomb_is_capped() {
        let bomb = zstd::bulk::compress(&vec![b' '; MAX_MESSAGE_BYTES + 1], 1).unwrap();
        assert!(bomb.len() < MAX_FRAME_BYTES);
        assert_eq!(
            from_receiver(&raw_frame(ZSTD, &bomb)),
            Err(FrameError::MessageTooLarge)
        );
    }

    #[test]
    fn oversized_messages_are_not_encoded() {
        let huge = SenderMessage::Error(ErrorReport {
            code: ErrorCode::Internal,
            message: "x".repeat(MAX_MESSAGE_BYTES),
        });
        assert_eq!(encode(&huge), Err(FrameError::MessageTooLarge));
    }

    #[test]
    fn a_partial_frame_waits_for_more() {
        let frame = encode(&ReceiverMessage::Done).unwrap();
        let mut decoder = FrameDecoder::default();
        decoder.push(&frame[..2]);
        assert_eq!(decoder.next_message::<ReceiverMessage>(), Ok(None));
        decoder.push(&frame[2..frame.len() - 1]);
        assert_eq!(decoder.next_message::<ReceiverMessage>(), Ok(None));
        decoder.push(&frame[frame.len() - 1..]);
        assert_eq!(decoder.next_message(), Ok(Some(ReceiverMessage::Done)));
        assert_eq!(decoder.next_message::<ReceiverMessage>(), Ok(None));
    }
}
