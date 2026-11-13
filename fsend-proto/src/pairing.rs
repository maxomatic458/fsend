use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, Key, KeyInit, Nonce},
};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

/// How to reach a peer, carried in [`Sealed::Connection`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ConnectionInfo {
    WebRtc {
        sdp: String,
        ice_candidates: Vec<String>,
    },
    Iroh {
        node_id: String,
        addrs: Vec<String>,
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
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Sender,
    Receiver,
}

/// A binary relay frame after `paired`: a kind byte, then the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signal {
    Pake(Vec<u8>),
    Sealed(Vec<u8>),
}

impl Signal {
    const PAKE: u8 = 0;
    const SEALED: u8 = 1;

    pub(crate) fn encode(&self) -> Vec<u8> {
        let (kind, body) = match self {
            Self::Pake(body) => (Self::PAKE, body),
            Self::Sealed(body) => (Self::SEALED, body),
        };
        [&[kind], body.as_slice()].concat()
    }

    pub(crate) fn decode(frame: &[u8]) -> Option<Self> {
        let (&kind, body) = frame.split_first()?;
        match kind {
            Self::PAKE => Some(Self::Pake(body.to_vec())),
            Self::SEALED => Some(Self::Sealed(body.to_vec())),
            _ => None,
        }
    }
}

pub(crate) type Pake = Spake2<Ed25519Group>;

/// Starts SPAKE2 for the code's password, returning the outbound message.
pub(crate) fn start_pake(nameplate: &str, password: &str) -> (Pake, Vec<u8>) {
    Pake::start_symmetric(
        &Password::new(password.as_bytes()),
        &Identity::new(format!("fsend:{nameplate}").as_bytes()),
    )
}

/// Seals and opens messages with per-direction keys and counter nonces.
pub(crate) struct Channel {
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

    fn next_nonce(&mut self) -> Nonce<Aes256Gcm> {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter += 1;
        Nonce::<Aes256Gcm>::from(nonce)
    }
}

impl Channel {
    pub(crate) fn new(shared_key: &[u8], role: Role) -> Self {
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

    pub(crate) fn seal(&mut self, message: &Sealed) -> Vec<u8> {
        let plaintext = serde_json::to_vec(message).expect("sealed messages serialize");
        let nonce = self.seal.next_nonce();
        self.seal
            .cipher
            .encrypt(&nonce, plaintext.as_slice())
            .expect("AES-GCM encrypts any message this size")
    }

    /// The plaintext, or `None` if the message was not sealed with the peer's key.
    pub(crate) fn open(&mut self, ciphertext: &[u8]) -> Option<Vec<u8>> {
        let nonce = self.open.next_nonce();
        self.open.cipher.decrypt(&nonce, ciphertext).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::pinned;

    fn channels(sender_password: &str, receiver_password: &str) -> (Channel, Channel) {
        let (a, a_msg) = start_pake("K7Q2", sender_password);
        let (b, b_msg) = start_pake("K7Q2", receiver_password);
        (
            Channel::new(&a.finish(&b_msg).unwrap(), Role::Sender),
            Channel::new(&b.finish(&a_msg).unwrap(), Role::Receiver),
        )
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
        let (mut sender, mut receiver) = channels("9XMV4P", "9XMV4P");
        let hello = Sealed::Confirm { app: "a".into() };
        let first = sender.seal(&hello);
        let second = sender.seal(&hello);
        assert_ne!(first, second, "every message gets a fresh nonce");
        assert_eq!(
            receiver.open(&first).unwrap(),
            serde_json::to_vec(&hello).unwrap()
        );
        assert!(receiver.open(&second).is_some());
        // The directions use different keys.
        let echo = receiver.seal(&hello);
        assert!(sender.open(&echo).is_some());
        assert!(receiver.open(&echo).is_none());
    }

    #[test]
    fn a_wrong_password_cannot_open_anything() {
        let (mut sender, mut receiver) = channels("9XMV4P", "9XMV4Q");
        assert!(receiver.open(&sender.seal(&Sealed::Unknown)).is_none());
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
                info: ConnectionInfo::WebRtc {
                    sdp: "v=0".into(),
                    ice_candidates: vec![],
                },
            },
            r#"{"type":"connection","info":{"type":"web_rtc","sdp":"v=0","ice_candidates":[]}}"#,
        );
        pinned(
            Sealed::Connection {
                info: ConnectionInfo::Iroh {
                    node_id: "n".into(),
                    addrs: vec!["10.0.0.1:1234".into()],
                },
            },
            r#"{"type":"connection","info":{"type":"iroh","node_id":"n","addrs":["10.0.0.1:1234"]}}"#,
        );
        assert_eq!(
            serde_json::from_str::<Sealed>(r#"{"type":"files","x":1}"#).unwrap(),
            Sealed::Unknown
        );
    }
}
