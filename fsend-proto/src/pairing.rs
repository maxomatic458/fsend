use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, Key, KeyInit, Nonce},
};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use thiserror::Error;

use std::net::SocketAddr;

use crate::code::Code;
use crate::peer::{Offer, frame, limits};
use crate::relay::MAX_FRAME_BYTES;

/// How to reach a peer, carried in [`Sealed::Connection`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ConnectionInfo {
    /// The session description, with every candidate gathered.
    WebRtc { sdp: String },
    Iroh {
        /// iroh's own encoding of the endpoint's public key.
        node_id: String,
        #[cfg_attr(feature = "ts", ts(type = "Array<string>"))]
        addrs: Vec<SocketAddr>,
        relays: Vec<String>,
    },
}

/// Plaintext of a sealed message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Sealed {
    /// Each side's first sealed message; opening it proves both used the same code.
    Confirm {
        app: String,
    },
    Offer(Offer),
    Accept,
    Reject {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        reason: Option<String>,
    },
    Connection {
        info: ConnectionInfo,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Sender,
    Receiver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PairingError {
    #[error("the codes did not match (a code works only once)")]
    WrongCode,
    #[error("invalid SPAKE2 message")]
    InvalidPake,
    #[error("a sealed message failed to open")]
    Tampered,
    #[error("malformed sealed message")]
    Malformed,
    #[error("a sealed message is too large")]
    TooLarge,
    #[error("unexpected signal")]
    UnexpectedSignal,
}

/// A binary relay frame after `paired`: a kind byte, then the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Pake(Vec<u8>),
    /// A sealed message, or the last part of one.
    Sealed(Vec<u8>),
    /// A part of a sealed message, more of which follows.
    Part(Vec<u8>),
}

impl Signal {
    const PAKE: u8 = 0;
    const SEALED: u8 = 1;
    const PART: u8 = 2;

    pub fn encode(&self) -> Vec<u8> {
        let (kind, body) = match self {
            Self::Pake(body) => (Self::PAKE, body),
            Self::Sealed(body) => (Self::SEALED, body),
            Self::Part(body) => (Self::PART, body),
        };
        [&[kind], body.as_slice()].concat()
    }

    pub fn decode(frame: &[u8]) -> Option<Self> {
        let (&kind, body) = frame.split_first()?;
        match kind {
            Self::PAKE => Some(Self::Pake(body.to_vec())),
            Self::SEALED => Some(Self::Sealed(body.to_vec())),
            Self::PART => Some(Self::Part(body.to_vec())),
            _ => None,
        }
    }
}

/// Bytes of a sealed message in one relay frame, after the kind byte.
const PART_BYTES: usize = MAX_FRAME_BYTES - 1;
/// The largest sealed message: the largest plaintext, and its tag.
const MAX_SEALED_BYTES: usize = limits::MAX_FRAME_BYTES + 16;

/// SPAKE2 keyed by the code's password, bound to its nameplate.
pub struct Pake(Spake2<Ed25519Group>);

impl Pake {
    /// Returns the first message for the peer.
    pub fn start(code: &Code) -> (Self, Vec<u8>) {
        let (pake, message) = Spake2::start_symmetric(
            &Password::new(code.password().as_bytes()),
            &Identity::new(format!("fsend:{}", code.nameplate()).as_bytes()),
        );
        (Self(pake), message)
    }

    pub fn finish(self, peer_message: &[u8], role: Role) -> Result<Channel, PairingError> {
        let key = self
            .0
            .finish(peer_message)
            .map_err(|_| PairingError::InvalidPake)?;
        Ok(Channel::new(&key, role))
    }
}

/// Per-direction keys and counter nonces; takes and returns whole relay frames.
pub struct Channel {
    seal: Direction,
    open: Direction,
    parts: Vec<u8>,
}

struct Direction {
    cipher: Aes256Gcm,
    counter: u64,
}

impl Direction {
    fn new(hkdf: &Hkdf<Sha256>, info: &str) -> Self {
        let mut key = [0; 32];
        hkdf.expand(info.as_bytes(), &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 length");
        Self {
            cipher: Aes256Gcm::new(&Key::<Aes256Gcm>::from(key)),
            counter: 0,
        }
    }

    fn nonce(&self) -> Nonce<Aes256Gcm> {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&self.counter.to_be_bytes());
        Nonce::<Aes256Gcm>::from(nonce)
    }
}

impl Channel {
    fn new(shared_key: &[u8], role: Role) -> Self {
        let hkdf = Hkdf::<Sha256>::new(None, shared_key);
        let to_receiver = Direction::new(&hkdf, "fsend sender to receiver");
        let to_sender = Direction::new(&hkdf, "fsend receiver to sender");
        let (seal, open) = match role {
            Role::Sender => (to_receiver, to_sender),
            Role::Receiver => (to_sender, to_receiver),
        };
        Self {
            seal,
            open,
            parts: Vec::new(),
        }
    }

    /// As many relay frames as its size takes.
    pub fn seal(&mut self, message: &Sealed) -> Result<Vec<Vec<u8>>, PairingError> {
        let plaintext = frame::encode_body(message).map_err(|_| PairingError::TooLarge)?;
        Ok(self.seal_plaintext(&plaintext))
    }

    /// Like [`Channel::seal`], for an already encoded message.
    pub fn seal_plaintext(&mut self, plaintext: &[u8]) -> Vec<Vec<u8>> {
        let ciphertext = self
            .seal
            .cipher
            .encrypt(&self.seal.nonce(), plaintext)
            .expect("AES-GCM encrypts any message this size");
        self.seal.counter += 1;
        let mut parts: Vec<_> = ciphertext.chunks(PART_BYTES).collect();
        let last = parts.pop().expect("a ciphertext holds at least its tag");
        parts
            .into_iter()
            .map(|part| Signal::Part(part.to_vec()).encode())
            .chain([Signal::Sealed(last.to_vec()).encode()])
            .collect()
    }

    /// The message once its last part is in; failing on the confirm means the codes differ.
    pub fn open(&mut self, frame: &[u8]) -> Result<Option<Sealed>, PairingError> {
        let (last, part) = match Signal::decode(frame) {
            Some(Signal::Sealed(part)) => (true, part),
            Some(Signal::Part(part)) => (false, part),
            Some(Signal::Pake(_)) | None => return Err(PairingError::UnexpectedSignal),
        };
        if self.parts.len() + part.len() > MAX_SEALED_BYTES {
            return Err(PairingError::TooLarge);
        }
        self.parts.extend_from_slice(&part);
        if !last {
            return Ok(None);
        }
        let ciphertext = std::mem::take(&mut self.parts);
        let plaintext = self
            .open
            .cipher
            .decrypt(&self.open.nonce(), ciphertext.as_slice())
            .map_err(|_| match self.open.counter {
                0 => PairingError::WrongCode,
                _ => PairingError::Tampered,
            })?;
        self.open.counter += 1;
        frame::decode_body(&plaintext)
            .map(Some)
            .map_err(|_| PairingError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::Node;
    use crate::peer::checksum::{Algorithm, Digest};

    fn channels(receiver_code: &str) -> Result<(Channel, Channel), PairingError> {
        let (sender, sender_msg) = Pake::start(&"K7Q29XMV4P".parse().unwrap());
        let (receiver, receiver_msg) = Pake::start(&receiver_code.parse().unwrap());
        Ok((
            sender.finish(&receiver_msg, Role::Sender)?,
            receiver.finish(&sender_msg, Role::Receiver)?,
        ))
    }

    fn confirm() -> Sealed {
        Sealed::Confirm { app: "a".into() }
    }

    fn connection() -> Sealed {
        Sealed::Connection {
            info: ConnectionInfo::Iroh {
                node_id: "n".into(),
                addrs: vec![],
                relays: vec![],
            },
        }
    }

    /// An offer of `files` files with checksums, which do not compress.
    fn offer(files: u32) -> Sealed {
        Sealed::Offer(Offer {
            checksums: Some(Algorithm::Sha256),
            entries: (0..files)
                .map(|i| {
                    let mut digest = [0; 32];
                    for (j, byte) in digest.iter_mut().enumerate() {
                        *byte = (i.wrapping_mul(2_654_435_761) >> (j % 24)) as u8 ^ j as u8;
                    }
                    Node::File {
                        name: format!("file-{i}.bin"),
                        size: i as u64,
                        checksum: Some(Digest::from(&digest[..])),
                    }
                })
                .collect(),
        })
    }

    fn sealed(channel: &mut Channel, message: &Sealed) -> Vec<u8> {
        let mut frames = channel.seal(message).unwrap();
        assert_eq!(frames.len(), 1);
        frames.pop().unwrap()
    }

    fn open_all(channel: &mut Channel, frames: &[Vec<u8>]) -> Result<Sealed, PairingError> {
        let (last, parts) = frames.split_last().unwrap();
        for part in parts {
            assert_eq!(channel.open(part)?, None);
        }
        Ok(channel
            .open(last)?
            .expect("the last frame completes the message"))
    }

    #[test]
    fn signals_round_trip() {
        for signal in [
            Signal::Pake(vec![1, 2, 3]),
            Signal::Sealed(vec![]),
            Signal::Part(vec![4]),
        ] {
            assert_eq!(Signal::decode(&signal.encode()), Some(signal));
        }
        assert_eq!(Signal::Pake(vec![7]).encode(), [0, 7]);
        assert_eq!(Signal::Sealed(vec![7]).encode(), [1, 7]);
        assert_eq!(Signal::Part(vec![7]).encode(), [2, 7]);
        assert_eq!(Signal::decode(&[]), None);
        assert_eq!(Signal::decode(&[3, 7]), None);
    }

    #[test]
    fn sealed_messages_open_in_order_on_the_other_side() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4P").unwrap();
        let first = sealed(&mut sender, &confirm());
        let second = sealed(&mut sender, &connection());
        assert_ne!(
            first,
            sealed(&mut sender, &confirm()),
            "every message gets a fresh nonce"
        );
        assert_eq!(receiver.open(&first), Ok(Some(confirm())));
        assert_eq!(receiver.open(&second), Ok(Some(connection())));
        // The directions use different keys.
        let echo = sealed(&mut receiver, &confirm());
        assert_eq!(sender.open(&echo), Ok(Some(confirm())));
        assert_eq!(receiver.open(&echo), Err(PairingError::Tampered));
    }

    #[test]
    fn large_messages_span_several_frames() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4P").unwrap();
        assert_eq!(
            receiver.open(&sealed(&mut sender, &confirm())),
            Ok(Some(confirm()))
        );

        let offer = offer(5000);
        let frames = sender.seal(&offer).unwrap();
        assert!(frames.len() > 4, "{} frames", frames.len());
        assert!(frames.iter().all(|frame| frame.len() <= MAX_FRAME_BYTES));
        let (last, parts) = frames.split_last().unwrap();
        assert!(parts.iter().all(|part| part[0] == Signal::PART));
        assert_eq!(last[0], Signal::SEALED);
        assert_eq!(open_all(&mut receiver, &frames), Ok(offer));
        // Later messages open on their own again.
        assert_eq!(
            receiver.open(&sealed(&mut sender, &connection())),
            Ok(Some(connection()))
        );
    }

    #[test]
    fn only_parts_of_a_sealed_message_may_come_between() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4P").unwrap();
        let frames = sender.seal(&offer(5000)).unwrap();
        assert_eq!(receiver.open(&frames[0]), Ok(None));
        assert_eq!(
            receiver.open(&Signal::Pake(vec![1]).encode()),
            Err(PairingError::UnexpectedSignal)
        );
        assert_eq!(receiver.open(&[9, 1]), Err(PairingError::UnexpectedSignal));
    }

    #[test]
    fn an_oversized_message_is_refused_as_it_arrives() {
        let (_, mut receiver) = channels("K7Q29XMV4P").unwrap();
        let part = Signal::Part(vec![0; PART_BYTES]).encode();
        let mut result = Ok(None);
        for _ in 0..=MAX_SEALED_BYTES / PART_BYTES {
            result = receiver.open(&part);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(PairingError::TooLarge));
    }

    #[test]
    fn a_wrong_code_fails_the_first_open() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4Q").unwrap();
        assert_eq!(
            receiver.open(&sealed(&mut sender, &confirm())),
            Err(PairingError::WrongCode)
        );
        assert_eq!(
            sender.open(&sealed(&mut receiver, &confirm())),
            Err(PairingError::WrongCode)
        );
    }

    #[test]
    fn tampered_and_replayed_messages_are_rejected() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4P").unwrap();
        let first = sealed(&mut sender, &confirm());
        assert_eq!(receiver.open(&first), Ok(Some(confirm())));
        assert_eq!(receiver.open(&first), Err(PairingError::Tampered));

        let mut second = sealed(&mut sender, &confirm());
        *second.last_mut().unwrap() ^= 1;
        assert_eq!(receiver.open(&second), Err(PairingError::Tampered));
    }

    #[test]
    fn a_malformed_spake2_message_is_rejected() {
        let (pake, _) = Pake::start(&"K7Q29XMV4P".parse().unwrap());
        assert!(matches!(
            pake.finish(&[7], Role::Sender),
            Err(PairingError::InvalidPake)
        ));
    }

    #[test]
    fn sealing_keeps_its_wire_format() {
        let mut sender = Channel::new(&[7; 32], Role::Sender);
        let mut receiver = Channel::new(&[7; 32], Role::Receiver);
        let frame = sealed(&mut sender, &confirm());
        assert_eq!(hex(&frame), GOLDEN);
        assert_eq!(receiver.open(&frame), Ok(Some(confirm())));
    }

    /// A sealed frame under HKDF-SHA256(key [7; 32], "fsend sender to receiver"), nonce 0.
    const GOLDEN: &str = "01223bf943c0907aa1f73fc463b5135699d6278a991cc386ac9b6d05d688f2479d10d120efe1eb19d30ab77c6916";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn unknown_sealed_types_are_malformed() {
        let mut sender = Channel::new(&[7; 32], Role::Sender);
        let mut receiver = Channel::new(&[7; 32], Role::Receiver);
        assert_eq!(
            receiver.open(&sealed(&mut sender, &confirm())),
            Ok(Some(confirm()))
        );
        let frames = sender.seal_plaintext(b"\x00{\"type\":\"files\",\"x\":1}");
        assert_eq!(
            open_all(&mut receiver, &frames),
            Err(PairingError::Malformed)
        );
    }
}
