use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
pub const NAMEPLATE_LEN: usize = 4;
pub const PASSWORD_LEN: usize = 6;
pub const CODE_LEN: usize = NAMEPLATE_LEN + PASSWORD_LEN;

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum CodeError {
    #[error("the code is empty")]
    Empty,
    #[error("a code has {CODE_LEN} characters, this one has {0}")]
    WrongLength(usize),
    #[error("'{0}' is not a valid code character")]
    InvalidChar(char),
}

/// The relay's part of a code: [`NAMEPLATE_LEN`] symbols of [`ALPHABET`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct Nameplate(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("a nameplate is {NAMEPLATE_LEN} characters of 0-9 and A-Z")]
pub struct InvalidNameplate;

impl Nameplate {
    pub fn random() -> Self {
        Self(random_symbols(NAMEPLATE_LEN))
    }
}

impl fmt::Display for Nameplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Nameplate {
    type Err = InvalidNameplate;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        is_canonical(input, NAMEPLATE_LEN)
            .then(|| Self(input.to_owned()))
            .ok_or(InvalidNameplate)
    }
}

impl TryFrom<String> for Nameplate {
    type Error = InvalidNameplate;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Nameplate> for String {
    fn from(nameplate: Nameplate) -> Self {
        nameplate.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Code(String);

impl Code {
    /// Joins a nameplate and a password; `None` unless the password is canonical.
    pub fn new(nameplate: &Nameplate, password: &str) -> Option<Self> {
        is_canonical(password, PASSWORD_LEN).then(|| Self(format!("{nameplate}{password}")))
    }

    /// A fresh code for the nameplate the relay assigned.
    pub fn with_random_password(nameplate: &Nameplate) -> Self {
        Self(format!("{nameplate}{}", random_symbols(PASSWORD_LEN)))
    }

    pub fn nameplate(&self) -> Nameplate {
        Nameplate(self.0[..NAMEPLATE_LEN].to_owned())
    }

    pub fn password(&self) -> &str {
        &self.0[NAMEPLATE_LEN..]
    }

    /// The share link, e.g. `https://fsend.sh/receive#K7Q29XMV4P`.
    pub fn link(&self, site: &str) -> String {
        format!("{}/receive#{self}", site.trim_end_matches('/'))
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Code {
    type Err = CodeError;

    /// Parses [`CODE_LEN`] symbols of [`ALPHABET`]; lowercase letters are upper-cased.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let code = input.to_ascii_uppercase();
        if let Some(c) = code
            .chars()
            .find(|c| !c.is_ascii() || !ALPHABET.contains(&(*c as u8)))
        {
            return Err(CodeError::InvalidChar(c));
        }
        match code.len() {
            0 => Err(CodeError::Empty),
            CODE_LEN => Ok(Self(code)),
            n => Err(CodeError::WrongLength(n)),
        }
    }
}

impl TryFrom<String> for Code {
    type Error = CodeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Code> for String {
    fn from(code: Code) -> Self {
        code.0
    }
}

/// `len` symbols from the operating system's CSPRNG.
pub fn random_symbols(len: usize) -> String {
    // Bytes from 252 up are dropped so that `% 36` stays uniform.
    let mut out = String::with_capacity(len);
    let mut bytes = [0u8; 32];
    while out.len() < len {
        getrandom::fill(&mut bytes).expect("the system random number generator is unavailable");
        out.extend(
            bytes
                .iter()
                .filter(|&&b| b < 252)
                .map(|&b| ALPHABET[usize::from(b % 36)] as char)
                .take(len - out.len()),
        );
    }
    out
}

/// Whether `s` is exactly `len` canonical symbols.
pub fn is_canonical(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| ALPHABET.contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Result<String, CodeError> {
        input.parse::<Code>().map(|c| c.to_string())
    }

    #[test]
    fn parses_codes_in_either_case() {
        assert_eq!(parse("K7Q29XMV4P").unwrap(), "K7Q29XMV4P");
        assert_eq!(parse("ILO29XMV4P").unwrap(), "ILO29XMV4P");
        assert_eq!(parse("k7q29xmv4p").unwrap(), "K7Q29XMV4P");
        assert_eq!(parse("k7Q29xMV4p").unwrap(), "K7Q29XMV4P");
    }

    #[test]
    fn rejects_anything_else() {
        assert_eq!(parse(""), Err(CodeError::Empty));
        assert_eq!(parse("K7Q29XMV4"), Err(CodeError::WrongLength(9)));
        assert_eq!(parse("K7Q29XMV4PP"), Err(CodeError::WrongLength(11)));
        assert_eq!(parse(" K7Q29XMV4P"), Err(CodeError::InvalidChar(' ')));
        assert_eq!(parse("K7Q2-9XMV4P"), Err(CodeError::InvalidChar('-')));
        assert_eq!(parse("K7Q29XMV4é"), Err(CodeError::InvalidChar('é')));
        assert_eq!(parse("K7Q2#9XMV4P"), Err(CodeError::InvalidChar('#')));
        assert_eq!(
            parse("https://fsend.sh/receive#K7Q29XMV4P"),
            Err(CodeError::InvalidChar(':'))
        );
    }

    fn nameplate(s: &str) -> Nameplate {
        s.parse().unwrap()
    }

    #[test]
    fn splits_into_nameplate_and_password() {
        let code = Code::new(&nameplate("K7Q2"), "9XMV4P").unwrap();
        assert_eq!(code.nameplate(), nameplate("K7Q2"));
        assert_eq!(code.password(), "9XMV4P");
        assert_eq!(
            code.link("https://fsend.sh/"),
            "https://fsend.sh/receive#K7Q29XMV4P"
        );
        assert_eq!(Code::new(&nameplate("K7Q2"), "9xmv4p"), None);
        assert_eq!(Code::new(&nameplate("K7Q2"), "9XMV4"), None);

        let fresh = Code::with_random_password(&nameplate("K7Q2"));
        assert_eq!(fresh.nameplate(), nameplate("K7Q2"));
        assert!(is_canonical(fresh.password(), PASSWORD_LEN));
    }

    #[test]
    fn nameplates_are_four_canonical_symbols() {
        for bad in ["", "K7Q", "K7Q2X", "k7q2", "K7-2"] {
            assert_eq!(bad.parse::<Nameplate>(), Err(InvalidNameplate), "{bad}");
        }
        assert!(is_canonical(
            &Nameplate::random().to_string(),
            NAMEPLATE_LEN
        ));
        assert_eq!(
            serde_json::to_string(&nameplate("K7Q2")).unwrap(),
            r#""K7Q2""#
        );
        assert!(serde_json::from_str::<Nameplate>(r#""k7q2""#).is_err());
    }

    #[test]
    fn random_symbols_come_from_the_alphabet() {
        let symbols = random_symbols(64);
        assert_eq!(symbols.len(), 64);
        assert!(symbols.bytes().all(|b| ALPHABET.contains(&b)));
        assert!(is_canonical(&symbols, 64));
    }

    #[test]
    fn serializes_as_a_string() {
        let code = Code::new(&nameplate("K7Q2"), "9XMV4P").unwrap();
        assert_eq!(serde_json::to_string(&code).unwrap(), r#""K7Q29XMV4P""#);
        assert_eq!(
            serde_json::from_str::<Code>(r#""k7q29xmv4p""#).unwrap(),
            code
        );
        assert!(serde_json::from_str::<Code>(r#""K7Q2""#).is_err());
    }
}
