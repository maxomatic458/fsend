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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Code(String);

impl Code {
    /// Joins a nameplate and a password, both already canonical.
    pub fn new(nameplate: &str, password: &str) -> Option<Self> {
        (is_canonical(nameplate, NAMEPLATE_LEN) && is_canonical(password, PASSWORD_LEN))
            .then(|| Self(format!("{nameplate}{password}")))
    }

    pub fn nameplate(&self) -> &str {
        &self.0[..NAMEPLATE_LEN]
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

    #[test]
    fn splits_into_nameplate_and_password() {
        let code = Code::new("K7Q2", "9XMV4P").unwrap();
        assert_eq!((code.nameplate(), code.password()), ("K7Q2", "9XMV4P"));
        assert_eq!(
            code.link("https://fsend.sh/"),
            "https://fsend.sh/receive#K7Q29XMV4P"
        );
        assert_eq!(Code::new("K7Q", "29XMV4P"), None);
        assert_eq!(Code::new("k7q2", "9XMV4P"), None);
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
        let code = Code::new("K7Q2", "9XMV4P").unwrap();
        assert_eq!(serde_json::to_string(&code).unwrap(), r#""K7Q29XMV4P""#);
        assert_eq!(
            serde_json::from_str::<Code>(r#""k7q29xmv4p""#).unwrap(),
            code
        );
        assert!(serde_json::from_str::<Code>(r#""K7Q2""#).is_err());
    }
}
