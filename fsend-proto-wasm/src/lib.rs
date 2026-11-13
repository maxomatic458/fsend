use fsend_proto::{
    code::{Code, CodeError},
    pairing::{self, PairingError, Signal},
    peer::{
        self, ReceiverMessage, SenderMessage, block, frame, limits, offer,
        resume::{self, Fingerprint},
    },
    relay::ServerMessage,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(untagged)]
enum ParsedCode {
    Valid { code: String },
    Invalid { error: CodeError, message: String },
}

/// Parses a code into a [`ParsedCode`].
#[wasm_bindgen(js_name = parseCode)]
pub fn parse_code(input: &str) -> String {
    let parsed = match input.parse::<Code>() {
        Ok(code) => ParsedCode::Valid {
            code: code.to_string(),
        },
        Err(error) => ParsedCode::Invalid {
            message: error.to_string(),
            error,
        },
    };
    serde_json::to_string(&parsed).expect("parsed codes serialize")
}

#[wasm_bindgen(js_name = codeLink)]
pub fn code_link(site: &str, code: &str) -> Result<String, JsError> {
    Ok(code.parse::<Code>()?.link(site))
}

/// A fresh code for the nameplate the relay assigned, if it is valid.
#[wasm_bindgen(js_name = newCode)]
pub fn new_code(nameplate: &str) -> Option<String> {
    Code::with_random_password(nameplate).map(|code| code.to_string())
}

/// The JSON `ServerMessage` in a relay text frame; throws on an invalid one.
#[wasm_bindgen(js_name = parseServerMessage)]
pub fn parse_server_message(text: &str) -> Result<String, JsError> {
    Ok(json(&serde_json::from_str::<ServerMessage>(text)?))
}

#[wasm_bindgen]
#[derive(Clone, Copy)]
pub enum Role {
    Sender,
    Receiver,
}

/// Thrown as an `Error` whose message is a JSON `PairingError`.
fn thrown(error: PairingError) -> JsError {
    JsError::new(&serde_json::to_string(&error).expect("pairing errors serialize"))
}

// `Pake` and `Channel` take and return whole binary relay frames.

#[wasm_bindgen]
pub struct Pake {
    pake: pairing::Pake,
    message: Vec<u8>,
}

#[wasm_bindgen]
impl Pake {
    pub fn start(code: &str) -> Result<Pake, JsError> {
        let (pake, message) = pairing::Pake::start(&code.parse()?);
        Ok(Self {
            pake,
            message: Signal::Pake(message).encode(),
        })
    }

    /// The first frame for the peer.
    #[wasm_bindgen(getter)]
    pub fn message(&self) -> Vec<u8> {
        self.message.clone()
    }

    pub fn finish(self, frame: &[u8], role: Role) -> Result<Channel, JsError> {
        let Some(Signal::Pake(message)) = Signal::decode(frame) else {
            return Err(thrown(PairingError::UnexpectedSignal));
        };
        let role = match role {
            Role::Sender => pairing::Role::Sender,
            Role::Receiver => pairing::Role::Receiver,
        };
        self.pake
            .finish(&message, role)
            .map(Channel)
            .map_err(thrown)
    }
}

#[wasm_bindgen]
pub struct Channel(pairing::Channel);

#[wasm_bindgen]
impl Channel {
    /// `message` is a JSON `Sealed`.
    pub fn seal(&mut self, message: &str) -> Result<Vec<u8>, JsError> {
        let sealed = self.0.seal(&serde_json::from_str(message)?);
        Ok(Signal::Sealed(sealed).encode())
    }

    /// The JSON `Sealed` in the peer's next frame.
    pub fn open(&mut self, frame: &[u8]) -> Result<String, JsError> {
        let Some(Signal::Sealed(sealed)) = Signal::decode(frame) else {
            return Err(thrown(PairingError::UnexpectedSignal));
        };
        let message = self.0.open(&sealed).map_err(thrown)?;
        Ok(serde_json::to_string(&message).expect("sealed messages serialize"))
    }
}

/// JSON text of a value that always serializes.
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("protocol values serialize")
}

/// One control frame of `message`, a JSON `SenderMessage` or `ReceiverMessage` as `from` sends.
#[wasm_bindgen(js_name = encodeFrame)]
pub fn encode_frame(message: &str, from: Role) -> Result<Vec<u8>, JsError> {
    Ok(match from {
        Role::Sender => frame::encode(&serde_json::from_str::<SenderMessage>(message)?)?,
        Role::Receiver => frame::encode(&serde_json::from_str::<ReceiverMessage>(message)?)?,
    })
}

/// Reassembles the control messages `from` sends from bytes split anywhere.
#[wasm_bindgen]
pub struct FrameDecoder {
    decoder: frame::FrameDecoder,
    from: Role,
}

#[wasm_bindgen]
impl FrameDecoder {
    #[wasm_bindgen(constructor)]
    pub fn new(from: Role) -> Self {
        Self {
            decoder: frame::FrameDecoder::default(),
            from,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.decoder.push(data);
    }

    /// The next message as a JSON `SenderMessage` or `ReceiverMessage`, if one is complete.
    #[wasm_bindgen(js_name = nextMessage)]
    pub fn next_message(&mut self) -> Result<Option<String>, JsError> {
        Ok(match self.from {
            Role::Sender => self
                .decoder
                .next_message::<SenderMessage>()?
                .map(|m| json(&m)),
            Role::Receiver => self
                .decoder
                .next_message::<ReceiverMessage>()?
                .map(|m| json(&m)),
        })
    }
}

/// A data stream block of `data`, 1 to `MAX_BLOCK_BYTES` file bytes, compressed if `compress` and that helps.
#[wasm_bindgen(js_name = encodeBlock)]
pub fn encode_block(data: &[u8], compress: bool) -> Result<Vec<u8>, JsError> {
    if !(1..=limits::MAX_BLOCK_BYTES).contains(&data.len()) {
        return Err(JsError::new("a block holds 1 to MAX_BLOCK_BYTES bytes"));
    }
    Ok(block::encode(data, compress))
}

/// Reassembles the file bytes of blocks from data stream bytes split anywhere.
#[wasm_bindgen]
#[derive(Default)]
pub struct BlockDecoder(block::BlockDecoder);

#[wasm_bindgen]
impl BlockDecoder {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        self.0.push(data);
    }

    /// Whether no partial block is buffered.
    #[wasm_bindgen(js_name = isEmpty)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The file bytes of the first buffered block, in proportion to how much of it arrived.
    pub fn partial(&self) -> f64 {
        self.0.partial() as f64
    }

    /// The file bytes of the next complete block; throws on a malformed one.
    #[wasm_bindgen(js_name = nextBlock)]
    pub fn next_block(&mut self) -> Result<Option<Vec<u8>>, JsError> {
        Ok(self.0.next_block()?)
    }
}

/// Turns counts of data stream bytes into progress per top-level entry, in stream order.
#[wasm_bindgen]
pub struct Progress(block::Progress);

#[wasm_bindgen]
impl Progress {
    /// For the JSON `AcceptedFile[]` `files` of the JSON `Entry[]` `entries`, with `top_of` from `topEntries`.
    #[wasm_bindgen(constructor)]
    pub fn new(entries: &str, files: &str, top_of: &[u32]) -> Result<Progress, JsError> {
        let entries: Vec<_> = serde_json::from_str(entries)?;
        let files: Vec<_> = serde_json::from_str(files)?;
        let top_of: Vec<usize> = top_of.iter().map(|&top| top as usize).collect();
        Ok(Self(block::Progress::new(&entries, &files, &top_of)))
    }

    /// `[top, bytes, top, bytes, ...]`: the bytes of each top-level entry among the next `n`.
    pub fn advance(&mut self, n: f64) -> Vec<f64> {
        self.0
            .advance(n as u64)
            .into_iter()
            .flat_map(|(top, bytes)| [top as f64, bytes as f64])
            .collect()
    }
}

/// The highest protocol version both sides speak.
#[wasm_bindgen]
pub fn negotiate(version: u32, min_version: u32) -> Option<u32> {
    peer::negotiate(version, min_version)
}

/// Checks `entries`, a JSON `Entry[]`, against the offer rules.
#[wasm_bindgen(js_name = validateOffer)]
pub fn validate_offer(entries: &str) -> Result<(), JsError> {
    Ok(offer::validate(&serde_json::from_str::<Vec<_>>(entries)?)?)
}

/// The control frame of an offer of `entries`, a JSON `Entry[]`, once they are valid.
#[wasm_bindgen(js_name = encodeOffer)]
pub fn encode_offer(entries: &str) -> Result<Vec<u8>, JsError> {
    Ok(offer::encode(&serde_json::from_str::<Vec<_>>(entries)?)?)
}

/// Checks `files`, a JSON `AcceptedFile[]`, against the offered `entries`.
#[wasm_bindgen(js_name = checkAccept)]
pub fn check_accept(entries: &str, files: &str) -> Result<(), JsError> {
    let entries: Vec<_> = serde_json::from_str(entries)?;
    Ok(offer::check_accept(
        &entries,
        &serde_json::from_str::<Vec<_>>(files)?,
    )?)
}

/// Checks `files`, a JSON `CheckedFile[]`, against the offered `entries`.
#[wasm_bindgen(js_name = checkLengths)]
pub fn check_lengths(entries: &str, files: &str) -> Result<(), JsError> {
    let entries: Vec<_> = serde_json::from_str(entries)?;
    Ok(offer::check_lengths(
        &entries,
        &serde_json::from_str::<Vec<_>>(files)?,
    )?)
}

/// JSON `[number, number][]`: the byte ranges of a file's first `len` bytes that are fingerprinted.
#[wasm_bindgen(js_name = fingerprintRanges)]
pub fn fingerprint_ranges(len: f64) -> String {
    json(&Fingerprint::ranges(len as u64))
}

/// The JSON `Fingerprint` of the bytes in the ranges from [`fingerprint_ranges`], concatenated.
#[wasm_bindgen]
pub fn fingerprint(sampled: &[u8]) -> String {
    json(&Fingerprint::of(sampled))
}

/// JSON `CheckedFile[]`: the local files, given as a JSON `Local[]`, to fingerprint.
#[wasm_bindgen(js_name = filesToCheck)]
pub fn files_to_check(entries: &str, local: &str) -> Result<String, JsError> {
    let entries: Vec<_> = serde_json::from_str(entries)?;
    Ok(json(&resume::to_check(
        &entries,
        &serde_json::from_str::<Vec<_>>(local)?,
    )))
}

/// The JSON `Plan` for `entries` with `local` present, given the ids whose fingerprints matched.
#[wasm_bindgen(js_name = planReceive)]
pub fn plan_receive(entries: &str, local: &str, verified: &[u32]) -> Result<String, JsError> {
    let entries: Vec<_> = serde_json::from_str(entries)?;
    Ok(json(&resume::plan(
        &entries,
        &serde_json::from_str::<Vec<_>>(local)?,
        verified,
    )))
}

/// The JSON `AcceptedFile[]` for a JSON `Plan` under a JSON `ConflictPolicy`.
#[wasm_bindgen(js_name = acceptPlan)]
pub fn accept_plan(plan: &str, policy: &str) -> Result<String, JsError> {
    let plan: resume::Plan = serde_json::from_str(plan)?;
    Ok(json(&plan.accept(serde_json::from_str(policy)?)))
}

/// JSON `[TopEntry[], number[]]`: the top-level entries of a valid offer, and the one each entry lies in.
#[wasm_bindgen(js_name = topEntries)]
pub fn top_entries(entries: &str) -> Result<String, JsError> {
    Ok(json(&offer::top_entries(&serde_json::from_str::<Vec<_>>(
        entries,
    )?)))
}

/// JSON `number[]`: bytes of each top-level entry already present once `files` are accepted.
#[wasm_bindgen(js_name = presentBytes)]
pub fn present_bytes(entries: &str, files: &str) -> Result<String, JsError> {
    let entries: Vec<_> = serde_json::from_str(entries)?;
    Ok(json(&offer::present(
        &entries,
        &serde_json::from_str::<Vec<_>>(files)?,
    )))
}

#[cfg(test)]
mod bindings {
    use std::{env, fs, path::Path};

    use fsend_proto::{
        code::NAMEPLATE_LEN,
        pairing::{ConnectionInfo, Sealed},
        peer::{
            AcceptedFile, Cancel, CheckedFile, Entry, ErrorReport, Hello,
            offer::TopEntry,
            resume::{Conflict, ConflictPolicy, Local, Plan},
        },
        relay::{ClientMessage, ErrorCode, Transport, VERSION},
    };
    use ts_rs::{Config, TS};

    use super::*;

    const OUT: &str = "../fsend-site/src/lib/proto/generated/protocol.ts";

    #[test]
    fn typescript_bindings_are_fresh() {
        let cfg = Config::new().with_large_int("number");
        let decls = [
            ClientMessage::decl(&cfg),
            ServerMessage::decl(&cfg),
            ErrorCode::decl(&cfg),
            Transport::decl(&cfg),
            Sealed::decl(&cfg),
            ConnectionInfo::decl(&cfg),
            PairingError::decl(&cfg),
            ParsedCode::decl(&cfg),
            CodeError::decl(&cfg),
            SenderMessage::decl(&cfg),
            ReceiverMessage::decl(&cfg),
            Hello::decl(&cfg),
            Cancel::decl(&cfg),
            ErrorReport::decl(&cfg),
            Entry::decl(&cfg),
            AcceptedFile::decl(&cfg),
            CheckedFile::decl(&cfg),
            Fingerprint::decl(&cfg),
            peer::ErrorCode::decl(&cfg),
            TopEntry::decl(&cfg),
            Local::decl(&cfg),
            ConflictPolicy::decl(&cfg),
            Conflict::decl(&cfg),
            Plan::decl(&cfg),
        ];
        let header = format!(
            "// Generated by `cargo test -p fsend-proto-wasm`. Do not edit.\n\n\
             export const RELAY_VERSION = {VERSION};\n\
             export const CODE_LENGTH = {};\n\
             export const NAMEPLATE_LENGTH = {NAMEPLATE_LEN};\n\
             export const PEER_VERSION = {};\n\
             export const PEER_MIN_VERSION = {};\n\
             export const RECV_WINDOW = {};\n\
             export const MIN_RECV_WINDOW = {};\n\
             export const ACK_STEP = {};\n\
             export const ACK_INTERVAL_MS = {};\n\
             export const HELLO_TIMEOUT_MS = {};\n\
             export const IDLE_TIMEOUT_MS = {};\n\
             export const GOODBYE_TIMEOUT_MS = {};\n\
             export const MAX_BLOCK_BYTES = {};\n",
            fsend_proto::code::CODE_LEN,
            peer::VERSION,
            peer::MIN_VERSION,
            limits::RECV_WINDOW,
            limits::MIN_RECV_WINDOW,
            limits::ACK_STEP,
            limits::ACK_INTERVAL.as_millis(),
            limits::HELLO_TIMEOUT.as_millis(),
            limits::IDLE_TIMEOUT.as_millis(),
            limits::GOODBYE_TIMEOUT.as_millis(),
            limits::MAX_BLOCK_BYTES,
        );
        let expected = decls
            .iter()
            .fold(header, |out, decl| out + "\nexport " + decl + "\n");

        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(OUT);
        if env::var_os("UPDATE_BINDINGS").is_some() {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &expected).unwrap();
        }
        let actual = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            actual == expected,
            "{OUT} is stale; regenerate it with `UPDATE_BINDINGS=1 cargo test -p fsend-proto-wasm`"
        );
    }
}
