use std::{collections::VecDeque, mem};

use serde::Serialize;
use thiserror::Error;

use crate::{
    code::{self, Code, PASSWORD_LEN},
    pairing::{Channel, Pake, Role, Sealed, Signal, start_pake},
    relay::{ClientMessage, ErrorCode, ServerMessage, Transport, VERSION},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Action {
    /// A text frame for the relay.
    Send {
        text: String,
    },
    /// A binary frame for the relay.
    SendBinary {
        #[cfg_attr(feature = "ts", ts(type = "Uint8Array"))]
        data: Vec<u8>,
    },
    Event {
        event: Event,
    },
    /// Terminal; nothing useful follows.
    Failed {
        error: RendezvousError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Event {
    /// Sender only: the code to hand to the receiver.
    Code {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        code: Code,
        expires_in: u32,
    },
    /// Both sides proved they hold the same code.
    Paired {
        transport: Transport,
        peer_app: String,
    },
    Sealed {
        message: Sealed,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RendezvousError {
    #[error("the code is wrong")]
    WrongCode,
    #[error("relay error ({code:?}): {message}")]
    Relay { code: ErrorCode, message: String },
    #[error("the relay connection closed")]
    Closed,
    #[error("protocol error: {message}")]
    Protocol { message: String },
}

fn protocol(message: &str) -> RendezvousError {
    RendezvousError::Protocol {
        message: message.to_owned(),
    }
}

enum State {
    /// Sender: waiting for `created`.
    Creating {
        password: String,
    },
    /// Waiting for `paired`.
    Waiting {
        nameplate: String,
        password: String,
    },
    /// Our SPAKE2 message is out, waiting for the peer's.
    Pake {
        pake: Pake,
        transport: Transport,
    },
    /// Keys derived, waiting for the peer's confirm.
    Confirming {
        channel: Channel,
        transport: Transport,
    },
    Paired {
        channel: Channel,
    },
    Failed,
}

pub struct Rendezvous {
    role: Role,
    app: String,
    state: State,
    actions: VecDeque<Action>,
}

impl Rendezvous {
    /// Creates a session; the code follows as [`Event::Code`].
    pub fn sender(transports: Vec<Transport>, app: impl Into<String>) -> Self {
        let state = State::Creating {
            password: code::random_symbols(PASSWORD_LEN),
        };
        let mut this = Self::new(Role::Sender, app.into(), state);
        this.send(&ClientMessage::Create {
            version: VERSION,
            transports,
        });
        this
    }

    pub fn receiver(code: &Code, transports: Vec<Transport>, app: impl Into<String>) -> Self {
        let state = State::Waiting {
            nameplate: code.nameplate().to_owned(),
            password: code.password().to_owned(),
        };
        let mut this = Self::new(Role::Receiver, app.into(), state);
        this.send(&ClientMessage::Join {
            version: VERSION,
            nameplate: code.nameplate().to_owned(),
            transports,
        });
        this
    }

    fn new(role: Role, app: String, state: State) -> Self {
        Self {
            role,
            app,
            state,
            actions: VecDeque::new(),
        }
    }

    pub fn poll(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }

    /// A text frame from the relay.
    pub fn text(&mut self, frame: &str) {
        self.advance(|this, state| this.on_text(state, frame));
    }

    /// A binary frame from the relay.
    pub fn binary(&mut self, frame: &[u8]) {
        self.advance(|this, state| {
            let signal = Signal::decode(frame).ok_or_else(|| protocol("malformed signal"))?;
            this.on_signal(state, signal)
        });
    }

    fn advance(&mut self, step: impl FnOnce(&mut Self, State) -> Result<State, RendezvousError>) {
        if matches!(self.state, State::Failed) {
            return;
        }
        let state = mem::replace(&mut self.state, State::Failed);
        match step(self, state) {
            Ok(state) => self.state = state,
            Err(error) => self.fail(error),
        }
    }

    /// The relay connection closed.
    pub fn closed(&mut self) {
        if !matches!(self.state, State::Failed) {
            self.fail(RendezvousError::Closed);
        }
    }

    /// Seals `message` for the peer; only valid once paired.
    pub fn send_sealed(&mut self, message: Sealed) {
        match &mut self.state {
            State::Paired { channel } => {
                let sealed = channel.seal(&message);
                self.signal(Signal::Sealed(sealed));
            }
            State::Failed => {}
            _ => self.fail(protocol("sent a sealed message before pairing")),
        }
    }

    fn on_text(&mut self, state: State, frame: &str) -> Result<State, RendezvousError> {
        let message: ServerMessage =
            serde_json::from_str(frame).map_err(|_| protocol("malformed relay frame"))?;
        match (state, message) {
            (state, ServerMessage::Unknown) => Ok(state),
            (_, ServerMessage::Error { code, message }) => {
                Err(RendezvousError::Relay { code, message })
            }
            (
                State::Creating { password },
                ServerMessage::Created {
                    nameplate,
                    expires_in,
                },
            ) => {
                let code = Code::new(&nameplate, &password)
                    .ok_or_else(|| protocol("the relay assigned an invalid nameplate"))?;
                self.emit(Event::Code { code, expires_in });
                Ok(State::Waiting {
                    nameplate,
                    password,
                })
            }
            (
                State::Waiting {
                    nameplate,
                    password,
                },
                ServerMessage::Paired { transport },
            ) => {
                let (pake, outbound) = start_pake(&nameplate, &password);
                self.signal(Signal::Pake(outbound));
                Ok(State::Pake { pake, transport })
            }
            _ => Err(protocol("unexpected message from the relay")),
        }
    }

    fn on_signal(&mut self, state: State, signal: Signal) -> Result<State, RendezvousError> {
        match (state, signal) {
            (State::Pake { pake, transport }, Signal::Pake(inbound)) => {
                let key = pake
                    .finish(&inbound)
                    .map_err(|_| protocol("invalid SPAKE2 message"))?;
                let mut channel = Channel::new(&key, self.role);
                let confirm = channel.seal(&Sealed::Confirm {
                    app: self.app.clone(),
                });
                self.signal(Signal::Sealed(confirm));
                Ok(State::Confirming { channel, transport })
            }
            (
                State::Confirming {
                    mut channel,
                    transport,
                },
                Signal::Sealed(sealed),
            ) => {
                let plaintext = channel.open(&sealed).ok_or(RendezvousError::WrongCode)?;
                let Sealed::Confirm { app } = parse_sealed(&plaintext)? else {
                    return Err(protocol("expected a confirm message"));
                };
                self.emit(Event::Paired {
                    transport,
                    peer_app: app,
                });
                Ok(State::Paired { channel })
            }
            (State::Paired { mut channel }, Signal::Sealed(sealed)) => {
                let plaintext = channel
                    .open(&sealed)
                    .ok_or_else(|| protocol("a sealed message failed to open"))?;
                match parse_sealed(&plaintext)? {
                    Sealed::Unknown => {}
                    Sealed::Confirm { .. } => return Err(protocol("repeated confirm message")),
                    message => self.emit(Event::Sealed { message }),
                }
                Ok(State::Paired { channel })
            }
            _ => Err(protocol("unexpected signal")),
        }
    }

    fn send(&mut self, message: &ClientMessage) {
        let text = serde_json::to_string(message).expect("client messages serialize");
        self.actions.push_back(Action::Send { text });
    }

    fn signal(&mut self, signal: Signal) {
        self.actions.push_back(Action::SendBinary {
            data: signal.encode(),
        });
    }

    fn emit(&mut self, event: Event) {
        self.actions.push_back(Action::Event { event });
    }

    fn fail(&mut self, error: RendezvousError) {
        self.state = State::Failed;
        self.actions.push_back(Action::Failed { error });
    }
}

fn parse_sealed(plaintext: &[u8]) -> Result<Sealed, RendezvousError> {
    serde_json::from_slice(plaintext).map_err(|_| protocol("malformed sealed message"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::ConnectionInfo;

    const SENDER_APP: &str = "sender-app";
    const RECEIVER_APP: &str = "receiver-app";

    fn info(id: &str) -> Sealed {
        Sealed::Connection {
            info: ConnectionInfo::Iroh {
                node_id: id.into(),
                addrs: vec![],
            },
        }
    }

    #[derive(Debug, PartialEq)]
    enum Frame {
        Text(ClientMessage),
        Binary(Vec<u8>),
    }

    /// Drains `side`, returning its relay frames and its other actions.
    fn drain(side: &mut Rendezvous) -> (Vec<Frame>, Vec<Action>) {
        let mut frames = Vec::new();
        let mut rest = Vec::new();
        while let Some(action) = side.poll() {
            match action {
                Action::Send { text } => {
                    frames.push(Frame::Text(serde_json::from_str(&text).unwrap()))
                }
                Action::SendBinary { data } => frames.push(Frame::Binary(data)),
                other => rest.push(other),
            }
        }
        (frames, rest)
    }

    fn frame(message: &ServerMessage) -> String {
        serde_json::to_string(message).unwrap()
    }

    fn event(action: &Action) -> &Event {
        match action {
            Action::Event { event } => event,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    /// Plays the relay for two clients until neither has anything to send,
    /// returning everything else they produced.
    struct Pair {
        sender: Rendezvous,
        receiver: Rendezvous,
        sender_out: Vec<Action>,
        receiver_out: Vec<Action>,
    }

    impl Pair {
        fn new(password_override: Option<&str>) -> Self {
            let mut sender =
                Rendezvous::sender(vec![Transport::Iroh, Transport::Webrtc], SENDER_APP);
            let (frames, _) = drain(&mut sender);
            assert!(matches!(
                frames[..],
                [Frame::Text(ClientMessage::Create { version: 1, .. })]
            ));
            sender.text(&frame(&ServerMessage::Created {
                nameplate: "K7Q2".into(),
                expires_in: 300,
            }));
            let (_, out) = drain(&mut sender);
            let Event::Code { code, expires_in } = event(&out[0]) else {
                panic!("expected the code first");
            };
            assert_eq!((code.nameplate(), *expires_in), ("K7Q2", 300));

            let code = match password_override {
                Some(password) => Code::new("K7Q2", password).unwrap(),
                None => code.clone(),
            };
            let mut receiver = Rendezvous::receiver(&code, vec![Transport::Webrtc], RECEIVER_APP);
            let (frames, _) = drain(&mut receiver);
            assert_eq!(
                frames,
                [Frame::Text(ClientMessage::Join {
                    version: 1,
                    nameplate: "K7Q2".into(),
                    transports: vec![Transport::Webrtc],
                })]
            );
            let paired = frame(&ServerMessage::Paired {
                transport: Transport::Webrtc,
            });
            sender.text(&paired);
            receiver.text(&paired);
            let mut pair = Self {
                sender,
                receiver,
                sender_out: Vec::new(),
                receiver_out: Vec::new(),
            };
            pair.run();
            pair
        }

        fn run(&mut self) {
            loop {
                let (to_receiver, sender_out) = drain(&mut self.sender);
                let (to_sender, receiver_out) = drain(&mut self.receiver);
                self.sender_out.extend(sender_out);
                self.receiver_out.extend(receiver_out);
                if to_receiver.is_empty() && to_sender.is_empty() {
                    return;
                }
                for (frames, peer) in [
                    (to_receiver, &mut self.receiver),
                    (to_sender, &mut self.sender),
                ] {
                    for frame in frames {
                        let Frame::Binary(data) = frame else {
                            panic!("only signals after pairing, got {frame:?}");
                        };
                        peer.binary(&data);
                    }
                }
            }
        }
    }

    #[test]
    fn both_sides_pair_and_exchange_sealed_messages() {
        let mut pair = Pair::new(None);
        assert_eq!(
            pair.sender_out,
            [Action::Event {
                event: Event::Paired {
                    transport: Transport::Webrtc,
                    peer_app: RECEIVER_APP.into()
                }
            }]
        );
        assert_eq!(
            pair.receiver_out,
            [Action::Event {
                event: Event::Paired {
                    transport: Transport::Webrtc,
                    peer_app: SENDER_APP.into()
                }
            }]
        );

        pair.sender.send_sealed(info("s"));
        pair.receiver.send_sealed(info("r"));
        pair.sender.send_sealed(Sealed::Unknown);
        pair.receiver_out.clear();
        pair.sender_out.clear();
        pair.run();
        assert_eq!(
            pair.receiver_out,
            [Action::Event {
                event: Event::Sealed { message: info("s") }
            }]
        );
        assert_eq!(
            pair.sender_out,
            [Action::Event {
                event: Event::Sealed { message: info("r") }
            }]
        );
    }

    #[test]
    fn a_wrong_code_fails_on_both_sides() {
        let pair = Pair::new(Some("000000"));
        let wrong = [Action::Failed {
            error: RendezvousError::WrongCode,
        }];
        assert_eq!(pair.sender_out, wrong);
        assert_eq!(pair.receiver_out, wrong);
    }

    fn receiver() -> Rendezvous {
        let code: Code = "K7Q29XMV4P".parse().unwrap();
        let mut receiver = Rendezvous::receiver(&code, vec![Transport::Webrtc], RECEIVER_APP);
        drain(&mut receiver);
        receiver
    }

    fn failure(side: &mut Rendezvous) -> RendezvousError {
        let (_, out) = drain(side);
        match out.as_slice() {
            [.., Action::Failed { error }] => error.clone(),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn relay_errors_are_reported() {
        let mut side = receiver();
        side.text(r#"{"type":"error","code":"version","message":"unsupported version"}"#);
        assert_eq!(
            failure(&mut side),
            RendezvousError::Relay {
                code: ErrorCode::Version,
                message: "unsupported version".into()
            }
        );
        // Nothing after a failure.
        side.text(r#"{"type":"paired","transport":"webrtc"}"#);
        side.closed();
        assert_eq!(side.poll(), None);
    }

    #[test]
    fn unknown_messages_are_ignored() {
        let mut side = receiver();
        side.text(r#"{"type":"motd","text":"hello"}"#);
        side.text(r#"{"type":"paired","transport":"webrtc","hint":"x"}"#);
        let (frames, out) = drain(&mut side);
        assert!(out.is_empty());
        assert!(matches!(frames[..], [Frame::Binary(_)]));
    }

    #[test]
    fn hostile_frames_are_protocol_errors() {
        let cases = [
            "not json",
            r#"{"type":"created","nameplate":"K7Q2","expires_in":1}"#,
            r#"{"type":"paired","transport":"quic"}"#,
        ];
        for frame in cases {
            let mut side = receiver();
            side.text(frame);
            assert!(
                matches!(failure(&mut side), RendezvousError::Protocol { .. }),
                "{frame}"
            );
        }

        // A signal before `paired`.
        let mut side = receiver();
        side.binary(&[0, 7]);
        assert!(matches!(
            failure(&mut side),
            RendezvousError::Protocol { .. }
        ));

        let after_paired: [&[u8]; 4] = [
            &[],
            // An unknown kind.
            &[2, 7],
            // A sealed message where the SPAKE2 one belongs.
            &[1, 7],
            // A SPAKE2 message of the wrong size.
            &[0, 7],
        ];
        for data in after_paired {
            let mut side = receiver();
            side.text(r#"{"type":"paired","transport":"webrtc"}"#);
            side.binary(data);
            assert!(
                matches!(failure(&mut side), RendezvousError::Protocol { .. }),
                "{data:?}"
            );
        }
    }

    #[test]
    fn the_sender_rejects_an_invalid_nameplate() {
        let mut sender = Rendezvous::sender(vec![Transport::Iroh], SENDER_APP);
        drain(&mut sender);
        sender.text(r#"{"type":"created","nameplate":"k7-2","expires_in":1}"#);
        assert!(matches!(
            failure(&mut sender),
            RendezvousError::Protocol { .. }
        ));
    }

    #[test]
    fn tampered_messages_after_pairing_are_rejected() {
        let mut pair = Pair::new(None);
        pair.sender.send_sealed(info("s"));
        let (frames, _) = drain(&mut pair.sender);
        let [Frame::Binary(data)] = &frames[..] else {
            panic!("expected one signal, got {frames:?}")
        };
        let mut data = data.clone();
        // The first ciphertext byte, after the kind byte.
        data[1] ^= 1;
        pair.receiver.binary(&data);
        assert!(matches!(
            failure(&mut pair.receiver),
            RendezvousError::Protocol { .. }
        ));
    }

    #[test]
    fn closing_or_sending_early_fails() {
        let mut side = receiver();
        side.send_sealed(info("early"));
        assert!(matches!(
            failure(&mut side),
            RendezvousError::Protocol { .. }
        ));

        let mut side = receiver();
        side.closed();
        assert_eq!(failure(&mut side), RendezvousError::Closed);
    }

    #[test]
    fn actions_serialize_for_the_web_app() {
        let code: Code = "K7Q29XMV4P".parse().unwrap();
        let action = Action::Event {
            event: Event::Code {
                code,
                expires_in: 300,
            },
        };
        assert_eq!(
            serde_json::to_string(&action).unwrap(),
            r#"{"type":"event","event":{"type":"code","code":"K7Q29XMV4P","expires_in":300}}"#
        );
        let failed = Action::Failed {
            error: RendezvousError::Relay {
                code: ErrorCode::NotFound,
                message: "m".into(),
            },
        };
        assert_eq!(
            serde_json::to_string(&failed).unwrap(),
            r#"{"type":"failed","error":{"type":"relay","code":"not_found","message":"m"}}"#
        );
    }
}
