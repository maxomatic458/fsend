use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::Digest as _;

#[cfg(test)]
use super::Node;
use super::{Entry, Offer};

/// Bytes of the longest digest any algorithm produces.
pub const MAX_DIGEST_BYTES: usize = 64;

/// One for the whole offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum Algorithm {
    Sha256,
}

impl Algorithm {
    pub fn digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
        }
    }

    pub fn hasher(self) -> Hasher {
        Hasher(match self {
            Self::Sha256 => State::Sha256(sha2::Sha256::new()),
        })
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sha256 => "SHA-256",
        })
    }
}

/// A file's digest under the offer's [`Algorithm`]. On the wire, lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(type = "string"))]
pub struct Digest(Box<[u8]>);

impl Digest {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<&[u8]> for Digest {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.into())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a digest is 1 to {MAX_DIGEST_BYTES} bytes as lowercase hex")]
pub struct ParseDigestError;

impl FromStr for Digest {
    type Err = ParseDigestError;

    fn from_str(hex: &str) -> Result<Self, Self::Err> {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        let (pairs, odd) = hex.as_bytes().as_chunks::<2>();
        if pairs.is_empty() || pairs.len() > MAX_DIGEST_BYTES || !odd.is_empty() {
            return Err(ParseDigestError);
        }
        pairs
            .iter()
            .map(|&[high, low]| Some(digit(high)? << 4 | digit(low)?))
            .collect::<Option<_>>()
            .map(Self)
            .ok_or(ParseDigestError)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone)]
pub struct Hasher(State);

#[derive(Debug, Clone)]
enum State {
    Sha256(sha2::Sha256),
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        match &mut self.0 {
            State::Sha256(state) => state.update(data),
        }
    }

    pub fn finish(self) -> Digest {
        match self.0 {
            State::Sha256(state) => Digest::from(state.finalize().as_slice()),
        }
    }
}

/// `<digest>  <path>` lines as `sha256sum` prints them; `None` without checksums.
pub fn listing(offer: &Offer) -> Option<String> {
    offer.checksums?;
    let mut out = String::new();
    for entry in &offer.flatten() {
        if let Entry::File { path, .. } = entry {
            out += &format!("{}  {}\n", entry.checksum()?, path.join("/"));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn sha256(data: &[u8]) -> Digest {
        let mut hasher = Algorithm::Sha256.hasher();
        hasher.update(data);
        hasher.finish()
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(sha256(b"").to_string(), EMPTY);
        assert_eq!(sha256(b"abc").to_string(), ABC);
        assert_eq!(
            sha256(b"abc").as_bytes().len(),
            Algorithm::Sha256.digest_len()
        );
        let mut split = Algorithm::Sha256.hasher();
        split.update(b"a");
        split.update(b"bc");
        assert_eq!(split.finish(), sha256(b"abc"));
    }

    #[test]
    fn digests_are_lowercase_hex() {
        crate::testing::pinned(sha256(b"abc"), &format!("\"{ABC}\""));
        crate::testing::pinned(Digest::from(&[0x0f][..]), r#""0f""#);
        let too_long = "ab".repeat(MAX_DIGEST_BYTES + 1);
        for bad in ["", "a", "0g", "AB", too_long.as_str()] {
            assert!(
                serde_json::from_str::<Digest>(&format!("\"{bad}\"")).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_listing_names_every_file() {
        let file = |name: &str, checksum| Node::File {
            name: name.into(),
            size: 0,
            checksum,
        };
        let offer = Offer {
            checksums: Some(Algorithm::Sha256),
            entries: vec![
                file("a", Some(sha256(b"abc"))),
                Node::Dir {
                    name: "d".into(),
                    entries: vec![file("b", Some(sha256(b"")))],
                },
            ],
        };
        assert_eq!(
            listing(&offer).unwrap(),
            format!("{ABC}  a\n{EMPTY}  d/b\n")
        );
        let plain = Offer {
            checksums: None,
            entries: vec![file("a", None)],
        };
        assert_eq!(listing(&plain), None);
    }
}
