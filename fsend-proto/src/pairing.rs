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
        /// URLs of the iroh relays it is reachable through.
        relays: Vec<String>,
    },
}

/// Plaintext of a sealed signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Sealed {
    /// Each side's first sealed message; opening it proves both used the same code.
    Confirm {
        app: String,
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
    #[error("the codes do not match")]
    WrongCode,
    #[error("invalid SPAKE2 message")]
    InvalidPake,
    #[error("a sealed message failed to open")]
    Tampered,
    #[error("malformed sealed message")]
    Malformed,
    #[error("unexpected signal")]
    UnexpectedSignal,
}

/// A binary relay frame after `paired`: a kind byte, then the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Pake(Vec<u8>),
    Sealed(Vec<u8>),
}

impl Signal {
    const PAKE: u8 = 0;
    const SEALED: u8 = 1;

    pub fn encode(&self) -> Vec<u8> {
        let (kind, body) = match self {
            Self::Pake(body) => (Self::PAKE, body),
            Self::Sealed(body) => (Self::SEALED, body),
        };
        [&[kind], body.as_slice()].concat()
    }

    pub fn decode(frame: &[u8]) -> Option<Self> {
        let (&kind, body) = frame.split_first()?;
        match kind {
            Self::PAKE => Some(Self::Pake(body.to_vec())),
            Self::SEALED => Some(Self::Sealed(body.to_vec())),
            _ => None,
        }
    }
}

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

/// Seals and opens messages with per-direction keys and counter nonces.
pub struct Channel {
    seal: Direction,
    open: Direction,
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
        match role {
            Role::Sender => Self {
                seal: to_receiver,
                open: to_sender,
            },
            Role::Receiver => Self {
                seal: to_sender,
                open: to_receiver,
            },
        }
    }

    pub fn seal(&mut self, message: &Sealed) -> Vec<u8> {
        self.seal_bytes(&serde_json::to_vec(message).expect("sealed messages serialize"))
    }

    fn seal_bytes(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let ciphertext = self
            .seal
            .cipher
            .encrypt(&self.seal.nonce(), plaintext)
            .expect("AES-GCM encrypts any message this size");
        self.seal.counter += 1;
        ciphertext
    }

    /// Opens the peer's next message; failing on the first (the confirm) means the codes differ.
    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Sealed, PairingError> {
        let plaintext = self
            .open
            .cipher
            .decrypt(&self.open.nonce(), ciphertext)
            .map_err(|_| match self.open.counter {
                0 => PairingError::WrongCode,
                _ => PairingError::Tampered,
            })?;
        self.open.counter += 1;
        serde_json::from_slice(&plaintext).map_err(|_| PairingError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::pinned;

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

    #[test]
    fn signals_round_trip() {
        for signal in [Signal::Pake(vec![1, 2, 3]), Signal::Sealed(vec![])] {
            assert_eq!(Signal::decode(&signal.encode()), Some(signal));
        }
        assert_eq!(Signal::Pake(vec![7]).encode(), [0, 7]);
        assert_eq!(Signal::Sealed(vec![7]).encode(), [1, 7]);
        assert_eq!(Signal::decode(&[]), None);
        assert_eq!(Signal::decode(&[2, 7]), None);
    }

    #[test]
    fn sealed_messages_open_in_order_on_the_other_side() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4P").unwrap();
        let first = sender.seal(&confirm());
        let second = sender.seal(&connection());
        assert_ne!(
            first,
            sender.seal(&confirm()),
            "every message gets a fresh nonce"
        );
        assert_eq!(receiver.open(&first), Ok(confirm()));
        assert_eq!(receiver.open(&second), Ok(connection()));
        // The directions use different keys.
        let echo = receiver.seal(&confirm());
        assert_eq!(sender.open(&echo), Ok(confirm()));
        assert_eq!(receiver.open(&echo), Err(PairingError::Tampered));
    }

    #[test]
    fn a_wrong_code_fails_the_first_open() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4Q").unwrap();
        assert_eq!(
            receiver.open(&sender.seal(&confirm())),
            Err(PairingError::WrongCode)
        );
        assert_eq!(
            sender.open(&receiver.seal(&confirm())),
            Err(PairingError::WrongCode)
        );
    }

    #[test]
    fn tampered_and_replayed_messages_are_rejected() {
        let (mut sender, mut receiver) = channels("K7Q29XMV4P").unwrap();
        let first = sender.seal(&confirm());
        assert_eq!(receiver.open(&first), Ok(confirm()));
        assert_eq!(receiver.open(&first), Err(PairingError::Tampered));

        let mut second = sender.seal(&confirm());
        second[0] ^= 1;
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
        let sealed = sender.seal(&confirm());
        assert_eq!(hex(&sealed), GOLDEN);
        assert_eq!(receiver.open(&sealed), Ok(confirm()));
    }

    /// AES-256-GCM under HKDF-SHA256(key [7; 32], "fsend sender to receiver"), nonce 0.
    const GOLDEN: &str =
        "5962af4ec9853db9ef7ec862bd1c4d86992984da0dc3d4b4832e4689305f7c93b9b5b71d3f8730bb86bd6d24";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn errors_serialize_for_the_web_app() {
        assert_eq!(
            serde_json::to_string(&PairingError::WrongCode).unwrap(),
            r#""wrong_code""#
        );
    }

    #[test]
    fn sealed_messages_keep_their_wire_format() {
        pinned(
            Sealed::Confirm {
                app: "fsend-cli 0.4.0".into(),
            },
            r#"{"type":"confirm","app":"fsend-cli 0.4.0"}"#,
        );
        pinned(
            Sealed::Connection {
                info: ConnectionInfo::WebRtc { sdp: "v=0".into() },
            },
            r#"{"type":"connection","info":{"type":"web_rtc","sdp":"v=0"}}"#,
        );
        pinned(
            Sealed::Connection {
                info: ConnectionInfo::Iroh {
                    node_id: "n".into(),
                    addrs: vec!["10.0.0.1:1234".parse().unwrap(), "[::1]:5".parse().unwrap()],
                    relays: vec!["https://relay.example/".into()],
                },
            },
            r#"{"type":"connection","info":{"type":"iroh","node_id":"n","addrs":["10.0.0.1:1234","[::1]:5"],"relays":["https://relay.example/"]}}"#,
        );
        // Addresses must be addresses.
        assert!(
            serde_json::from_str::<ConnectionInfo>(
                r#"{"type":"iroh","node_id":"n","addrs":["somewhere"],"relays":[]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_sealed_types_are_malformed() {
        let mut sender = Channel::new(&[7; 32], Role::Sender);
        let mut receiver = Channel::new(&[7; 32], Role::Receiver);
        assert_eq!(receiver.open(&sender.seal(&confirm())), Ok(confirm()));
        let sealed = sender.seal_bytes(br#"{"type":"files","x":1}"#);
        assert_eq!(receiver.open(&sealed), Err(PairingError::Malformed));
    }
}
