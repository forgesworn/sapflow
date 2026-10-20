//! The server side of one RTMP connection after the handshake, for
//! publishers only.
//!
//! Sans-IO: [`Session::feed`] takes bytes from the socket,
//! [`Session::poll`] yields events one at a time, [`Session::take_output`]
//! returns bytes to send. A publish request stops the session until the
//! caller answers it with [`Session::accept_publish`] or
//! [`Session::refuse_publish`], so no media is handed out before the
//! credentials are checked.

use super::amf0::{self, Amf0Error, Value};
use super::chunk::{ChunkError, ChunkReader, ChunkWriter, Read};

/// Bytes a connection may hold (unparsed input plus partial messages)
/// before its publish is accepted. Commands are small.
pub const BUDGET_BEFORE_PUBLISH: usize = 1 << 20;
/// The same once publishing: room for the largest message (16 MiB) and
/// a large chunk in flight.
pub const BUDGET_PUBLISHING: usize = 40 << 20;
/// Most `createStream` calls per connection. A publisher needs one.
pub const MAX_STREAMS: u32 = 16;

/// Our chunk size, announced first thing.
const OUT_CHUNK_SIZE: u32 = 4096;
/// What we ask the client to acknowledge after, and its bandwidth limit:
/// the values rml_rtmp sent, so clients see what they saw before.
const WINDOW_ACK_SIZE: u32 = 1_073_741_824;
const PEER_BANDWIDTH: u32 = 2_500_000;
/// Smallest acknowledgement window we honour from a client.
const MIN_PEER_WINDOW: u32 = 4096;

const CSID_CONTROL: u8 = 2;
const CSID_COMMAND: u8 = 3;

const SET_CHUNK_SIZE: u8 = 1;
const ABORT: u8 = 2;
const ACKNOWLEDGEMENT: u8 = 3;
const USER_CONTROL: u8 = 4;
const WINDOW_ACK: u8 = 5;
const SET_PEER_BANDWIDTH: u8 = 6;
const AUDIO: u8 = 8;
const VIDEO: u8 = 9;
const AMF3_DATA: u8 = 15;
const AMF3_COMMAND: u8 = 17;
const AMF0_DATA: u8 = 18;
const AMF0_COMMAND: u8 = 20;

const STREAM_BEGIN: u16 = 0;
const PING_REQUEST: u16 = 6;
const PING_RESPONSE: u16 = 7;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The client asked to publish `key` (the stream key, query included)
    /// in `app`. Answer before polling again.
    Publish { app: String, key: String },
    /// `onMetaData` arrived; `audio` is whether it names an audio codec.
    Metadata { audio: bool },
    /// An FLV video tag body and its timestamp in milliseconds.
    Video { timestamp: u32, data: Vec<u8> },
    /// An FLV audio tag body and its timestamp in milliseconds.
    Audio { timestamp: u32, data: Vec<u8> },
    /// The publisher said it has finished.
    Finished,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    Chunk(ChunkError),
    Amf0(Amf0Error),
    /// A protocol control message too short for its type.
    Malformed(u8),
    /// `connect` without an `app` string.
    ConnectWithoutApp,
    /// `publish` before `connect`, on a stream never created, or without
    /// a stream key.
    BadPublish,
    /// A second `publish` on one connection.
    SecondPublish,
    TooManyStreams,
    /// More unparsed input than the budget allows.
    OverBudget,
    /// `poll` was called while a publish request was unanswered.
    DecisionPending,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chunk(e) => write!(f, "{e}"),
            Self::Amf0(e) => write!(f, "{e}"),
            Self::Malformed(t) => write!(f, "malformed message of type {t}"),
            Self::ConnectWithoutApp => f.write_str("connect without an app"),
            Self::BadPublish => f.write_str("invalid publish"),
            Self::SecondPublish => f.write_str("second publish on one connection"),
            Self::TooManyStreams => f.write_str("too many streams"),
            Self::OverBudget => f.write_str("too much buffered input"),
            Self::DecisionPending => f.write_str("publish request not answered"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<ChunkError> for SessionError {
    fn from(e: ChunkError) -> Self {
        Self::Chunk(e)
    }
}

impl From<Amf0Error> for SessionError {
    fn from(e: Amf0Error) -> Self {
        Self::Amf0(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Pending(u32),
    Publishing(u32),
    /// Finished or refused: nothing more is handed out.
    Done,
}

pub struct Session {
    reader: ChunkReader,
    writer: ChunkWriter,
    input: Vec<u8>,
    pos: usize,
    budget: usize,
    out: Vec<u8>,
    app: Option<String>,
    created: u32,
    state: State,
    peer_window: Option<u32>,
    received: u64,
    since_ack: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// A session whose opening messages (chunk size, window, bandwidth)
    /// are already waiting in [`Session::take_output`].
    pub fn new() -> Self {
        let mut s = Self {
            reader: ChunkReader::new(BUDGET_BEFORE_PUBLISH),
            writer: ChunkWriter::default(),
            input: Vec::new(),
            pos: 0,
            budget: BUDGET_BEFORE_PUBLISH,
            out: Vec::new(),
            app: None,
            created: 0,
            state: State::Idle,
            peer_window: None,
            received: 0,
            since_ack: 0,
        };
        s.control(SET_CHUNK_SIZE, &OUT_CHUNK_SIZE.to_be_bytes());
        s.writer.set_chunk_size(OUT_CHUNK_SIZE);
        s.control(WINDOW_ACK, &WINDOW_ACK_SIZE.to_be_bytes());
        s.user_control(STREAM_BEGIN, &0u32.to_be_bytes());
        let mut bw = PEER_BANDWIDTH.to_be_bytes().to_vec();
        bw.push(2); // dynamic
        s.control(SET_PEER_BANDWIDTH, &bw);
        s.command(
            0,
            &[
                Value::String("onBWDone".into()),
                Value::Number(0.0),
                Value::Null,
                Value::Number(8192.0),
            ],
        );
        s
    }

    /// Bytes to send to the client, in order.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Takes bytes from the socket. Feed a read at a time and poll until
    /// `None` in between: unparsed input counts against the budget.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        let n = bytes.len() as u64;
        self.received = self.received.wrapping_add(n);
        if let Some(window) = self.peer_window {
            self.since_ack = self.since_ack.saturating_add(n);
            if self.since_ack >= u64::from(window) {
                self.since_ack = 0;
                // The sequence number is the byte count so far, mod 2^32.
                let seq = self.received as u32;
                self.control(ACKNOWLEDGEMENT, &seq.to_be_bytes());
            }
        }
        if self.pos > 0 {
            self.input.drain(..self.pos);
            self.pos = 0;
        }
        let pending = self
            .input
            .len()
            .saturating_add(bytes.len())
            .saturating_add(self.reader.buffered());
        if pending > self.budget {
            return Err(SessionError::OverBudget);
        }
        self.input.extend_from_slice(bytes);
        Ok(())
    }

    /// The next event, or `None` when more input is needed.
    pub fn poll(&mut self) -> Result<Option<Event>, SessionError> {
        loop {
            match self.state {
                State::Pending(_) => return Err(SessionError::DecisionPending),
                State::Done => return Ok(None),
                State::Idle | State::Publishing(_) => {}
            }
            let rest = self.input.get(self.pos..).unwrap_or_default();
            match self.reader.read(rest)? {
                Read::NeedMore => return Ok(None),
                Read::Chunk { consumed, message } => {
                    self.pos = self.pos.saturating_add(consumed);
                    if let Some(m) = message
                        && let Some(ev) =
                            self.message(m.type_id, m.stream_id, m.timestamp, m.payload)?
                    {
                        return Ok(Some(ev));
                    }
                }
            }
        }
    }

    /// Accepts the pending publish request: the client may send media.
    pub fn accept_publish(&mut self) {
        let State::Pending(id) = self.state else {
            return;
        };
        self.state = State::Publishing(id);
        self.budget = BUDGET_PUBLISHING;
        self.reader.set_budget(BUDGET_PUBLISHING);
        self.user_control(STREAM_BEGIN, &id.to_be_bytes());
        self.on_status(
            id,
            "status",
            "NetStream.Publish.Start",
            "Started publishing.",
        );
    }

    /// Refuses the pending publish request with an `onStatus` error
    /// (`code` such as `NetStream.Publish.Denied` or
    /// `NetStream.Publish.BadName`). The caller should then close.
    pub fn refuse_publish(&mut self, code: &str, description: &str) {
        let State::Pending(id) = self.state else {
            return;
        };
        self.state = State::Done;
        self.on_status(id, "error", code, description);
    }

    fn message(
        &mut self,
        type_id: u8,
        stream_id: u32,
        timestamp: u32,
        payload: Vec<u8>,
    ) -> Result<Option<Event>, SessionError> {
        let publishing = matches!(self.state, State::Publishing(id) if id == stream_id);
        match type_id {
            SET_CHUNK_SIZE => {
                let size = be32(&payload).ok_or(SessionError::Malformed(type_id))?;
                self.reader.set_chunk_size(size)?;
            }
            ABORT => {
                let csid = be32(&payload).ok_or(SessionError::Malformed(type_id))?;
                self.reader.abort(csid);
            }
            WINDOW_ACK => {
                let size = be32(&payload).ok_or(SessionError::Malformed(type_id))?;
                self.peer_window = Some(size.max(MIN_PEER_WINDOW));
            }
            USER_CONTROL => {
                if let [e0, e1, a, b, c, d, ..] = payload[..]
                    && u16::from_be_bytes([e0, e1]) == PING_REQUEST
                {
                    let mut body = PING_RESPONSE.to_be_bytes().to_vec();
                    body.extend_from_slice(&[a, b, c, d]);
                    self.control(USER_CONTROL, &body);
                }
            }
            ACKNOWLEDGEMENT | SET_PEER_BANDWIDTH => {}
            AUDIO if publishing => {
                return Ok(Some(Event::Audio {
                    timestamp,
                    data: payload,
                }));
            }
            VIDEO if publishing => {
                return Ok(Some(Event::Video {
                    timestamp,
                    data: payload,
                }));
            }
            AMF0_DATA | AMF3_DATA if publishing => {
                let body = if type_id == AMF3_DATA {
                    payload.get(1..).unwrap_or_default()
                } else {
                    &payload[..]
                };
                // Bad metadata is not worth ending a show over.
                if let Ok(values) = amf0::decode_all(body) {
                    return Ok(metadata(&values));
                }
            }
            AMF0_COMMAND | AMF3_COMMAND => {
                let body = if type_id == AMF3_COMMAND {
                    payload.get(1..).unwrap_or_default()
                } else {
                    &payload[..]
                };
                let values = amf0::decode_all(body)?;
                return self.command_in(stream_id, &values);
            }
            _ => {}
        }
        Ok(None)
    }

    fn command_in(
        &mut self,
        stream_id: u32,
        values: &[Value],
    ) -> Result<Option<Event>, SessionError> {
        let name = values.first().and_then(Value::as_str).unwrap_or_default();
        let txn = values.get(1).and_then(Value::as_number).unwrap_or(0.0);
        let object = values.get(2);
        let args = values.get(3..).unwrap_or_default();
        let first_number = args.first().and_then(Value::as_number);
        match name {
            "connect" => {
                let app = object
                    .and_then(|o| o.get("app"))
                    .and_then(Value::as_str)
                    .ok_or(SessionError::ConnectWithoutApp)?;
                let app = app.strip_suffix('/').unwrap_or(app).to_owned();
                let encoding = object
                    .and_then(|o| o.get("objectEncoding"))
                    .and_then(Value::as_number)
                    .unwrap_or(0.0);
                self.app = Some(app);
                self.command(
                    0,
                    &[
                        Value::String("_result".into()),
                        Value::Number(txn),
                        Value::Object(vec![
                            ("fmsVer".into(), Value::String("FMS/3,0,1,123".into())),
                            ("capabilities".into(), Value::Number(31.0)),
                        ]),
                        Value::Object(vec![
                            ("level".into(), Value::String("status".into())),
                            (
                                "code".into(),
                                Value::String("NetConnection.Connect.Success".into()),
                            ),
                            (
                                "description".into(),
                                Value::String("Connection succeeded.".into()),
                            ),
                            ("objectEncoding".into(), Value::Number(encoding)),
                        ]),
                    ],
                );
            }
            "createStream" => {
                if self.created >= MAX_STREAMS {
                    return Err(SessionError::TooManyStreams);
                }
                self.created += 1;
                // ffmpeg reads the stream id at a fixed offset: the
                // command object must be null.
                self.command(
                    0,
                    &[
                        Value::String("_result".into()),
                        Value::Number(txn),
                        Value::Null,
                        Value::Number(f64::from(self.created)),
                    ],
                );
            }
            "publish" => {
                if self.state != State::Idle {
                    return Err(SessionError::SecondPublish);
                }
                let app = self.app.clone().ok_or(SessionError::BadPublish)?;
                if stream_id == 0 || stream_id > self.created {
                    return Err(SessionError::BadPublish);
                }
                let key = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or(SessionError::BadPublish)?
                    .to_owned();
                self.state = State::Pending(stream_id);
                return Ok(Some(Event::Publish { app, key }));
            }
            "deleteStream" | "closeStream" | "FCUnpublish" => {
                if let State::Publishing(id) = self.state {
                    let ours = match name {
                        "deleteStream" => first_number == Some(f64::from(id)),
                        "closeStream" => stream_id == id || first_number == Some(f64::from(id)),
                        _ => true,
                    };
                    if ours {
                        self.state = State::Done;
                        return Ok(Some(Event::Finished));
                    }
                }
            }
            "play" | "play2" => {
                self.on_status(
                    stream_id,
                    "error",
                    "NetStream.Play.Failed",
                    "no RTMP playback",
                );
            }
            // releaseStream, FCPublish, _checkbw and anything else: no
            // reply is needed by the publishers we serve.
            _ => {}
        }
        Ok(None)
    }

    fn control(&mut self, type_id: u8, body: &[u8]) {
        self.writer
            .write(&mut self.out, CSID_CONTROL, type_id, 0, 0, body);
    }

    fn user_control(&mut self, event: u16, data: &[u8]) {
        let mut body = event.to_be_bytes().to_vec();
        body.extend_from_slice(data);
        self.control(USER_CONTROL, &body);
    }

    fn command(&mut self, stream_id: u32, values: &[Value]) {
        let mut body = Vec::new();
        for v in values {
            amf0::encode(v, &mut body);
        }
        self.writer.write(
            &mut self.out,
            CSID_COMMAND,
            AMF0_COMMAND,
            stream_id,
            0,
            &body,
        );
    }

    fn on_status(&mut self, stream_id: u32, level: &str, code: &str, description: &str) {
        self.command(
            stream_id,
            &[
                Value::String("onStatus".into()),
                Value::Number(0.0),
                Value::Null,
                Value::Object(vec![
                    ("level".into(), Value::String(level.into())),
                    ("code".into(), Value::String(code.into())),
                    ("description".into(), Value::String(description.into())),
                ]),
            ],
        );
    }
}

fn be32(b: &[u8]) -> Option<u32> {
    let [a, b, c, d, ..] = *b else {
        return None;
    };
    Some(u32::from_be_bytes([a, b, c, d]))
}

/// `[@setDataFrame,] onMetaData, {...}`: whether it names an audio codec
/// (a number from OBS and ffmpeg, a string such as "mp4a" from some).
fn metadata(values: &[Value]) -> Option<Event> {
    let values = match values.first().and_then(Value::as_str) {
        Some("@setDataFrame") => values.get(1..)?,
        _ => values,
    };
    let [name, props, ..] = values else {
        return None;
    };
    if name.as_str() != Some("onMetaData") {
        return None;
    }
    props.properties()?;
    let audio = matches!(
        props.get("audiocodecid"),
        Some(Value::Number(_) | Value::String(_))
    );
    Some(Event::Metadata { audio })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::chunk::Message;
    use super::*;

    /// A client: writes commands and media as chunks, reads replies.
    pub(crate) struct Client {
        pub writer: ChunkWriter,
        reader: ChunkReader,
    }

    impl Client {
        pub fn new() -> Self {
            Self {
                writer: ChunkWriter::default(),
                reader: ChunkReader::new(1 << 24),
            }
        }

        pub fn command(&self, stream_id: u32, values: &[Value]) -> Vec<u8> {
            let mut body = Vec::new();
            for v in values {
                amf0::encode(v, &mut body);
            }
            let mut out = Vec::new();
            self.writer
                .write(&mut out, 3, AMF0_COMMAND, stream_id, 0, &body);
            out
        }

        pub fn message(&self, type_id: u8, stream_id: u32, ts: u32, body: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            self.writer.write(&mut out, 6, type_id, stream_id, ts, body);
            out
        }

        /// Decodes the server's replies.
        pub fn replies(&mut self, mut bytes: &[u8]) -> Vec<Message> {
            let mut out = Vec::new();
            loop {
                match self.reader.read(bytes).expect("server writes valid chunks") {
                    Read::NeedMore => break,
                    Read::Chunk { consumed, message } => {
                        bytes = &bytes[consumed..];
                        if let Some(m) = message {
                            if m.type_id == SET_CHUNK_SIZE {
                                let size = be32(&m.payload).expect("4 bytes");
                                self.reader.set_chunk_size(size).expect("valid");
                            }
                            out.push(m);
                        }
                    }
                }
            }
            assert!(bytes.is_empty(), "replies end on a chunk boundary");
            out
        }
    }

    fn s(v: &str) -> Value {
        Value::String(v.into())
    }

    fn n(v: f64) -> Value {
        Value::Number(v)
    }

    fn connect(app: &str) -> Vec<Value> {
        vec![
            s("connect"),
            n(1.0),
            Value::Object(vec![
                ("app".into(), s(app)),
                ("tcUrl".into(), s("rtmp://h/live")),
            ]),
        ]
    }

    fn all(sess: &mut Session) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some(ev) = sess.poll().expect("valid session") {
            out.push(ev);
        }
        out
    }

    fn commands(msgs: &[Message]) -> Vec<Vec<Value>> {
        msgs.iter()
            .filter(|m| m.type_id == AMF0_COMMAND)
            .map(|m| amf0::decode_all(&m.payload).expect("valid AMF0"))
            .collect()
    }

    /// Runs connect, createStream and publish; returns the session
    /// waiting on the publish decision.
    fn up_to_publish(client: &mut Client) -> Session {
        let mut sess = Session::new();
        let opening = client.replies(&sess.take_output());
        let types: Vec<_> = opening.iter().map(|m| m.type_id).collect();
        assert_eq!(
            types,
            vec![
                SET_CHUNK_SIZE,
                WINDOW_ACK,
                USER_CONTROL,
                SET_PEER_BANDWIDTH,
                AMF0_COMMAND
            ]
        );

        sess.feed(&client.command(0, &connect("live/")))
            .expect("feed");
        assert_eq!(all(&mut sess), vec![]);
        let r = commands(&client.replies(&sess.take_output()));
        assert_eq!(r[0][0], s("_result"));
        assert_eq!(r[0][1], n(1.0));
        assert_eq!(
            r[0][3].get("code"),
            Some(&s("NetConnection.Connect.Success"))
        );

        for c in [
            vec![s("releaseStream"), n(2.0), Value::Null, s("event-a?x=1")],
            vec![s("FCPublish"), n(3.0), Value::Null, s("event-a?x=1")],
            vec![s("_checkbw"), n(4.0), Value::Null],
            vec![s("createStream"), n(5.0), Value::Null],
        ] {
            sess.feed(&client.command(0, &c)).expect("feed");
        }
        assert_eq!(all(&mut sess), vec![]);
        let out = sess.take_output();
        let r = commands(&client.replies(&out));
        assert_eq!(r, vec![vec![s("_result"), n(5.0), Value::Null, n(1.0)]]);

        let publish = client.command(
            1,
            &[
                s("publish"),
                n(6.0),
                Value::Null,
                s("event-a?x=1"),
                s("live"),
            ],
        );
        sess.feed(&publish).expect("feed");
        assert_eq!(
            sess.poll(),
            Ok(Some(Event::Publish {
                app: "live".into(),
                key: "event-a?x=1".into()
            }))
        );
        assert_eq!(sess.poll(), Err(SessionError::DecisionPending));
        sess
    }

    #[test]
    fn create_stream_result_matches_ffmpegs_fixed_offset() {
        let mut body = Vec::new();
        for v in [s("_result"), n(5.0), Value::Null, n(1.0)] {
            amf0::encode(&v, &mut body);
        }
        // ffmpeg: stream_id = av_int2double(AV_RB64(pkt->data + 21)).
        assert_eq!(body[20], 0);
        assert_eq!(
            f64::from_be_bytes(body[21..29].try_into().expect("8 bytes")),
            1.0
        );
    }

    #[test]
    fn publish_accepted_then_media_then_finished() {
        let mut client = Client::new();
        let mut sess = up_to_publish(&mut client);
        sess.accept_publish();
        let r = client.replies(&sess.take_output());
        assert_eq!(r[0].type_id, USER_CONTROL);
        assert_eq!(r[0].payload, vec![0, 0, 0, 0, 0, 1]);
        let st = commands(&r[1..]);
        assert_eq!(r[1].stream_id, 1);
        assert_eq!(st[0][0], s("onStatus"));
        assert_eq!(st[0][3].get("code"), Some(&s("NetStream.Publish.Start")));
        assert_eq!(st[0][3].get("level"), Some(&s("status")));

        let mut meta = Vec::new();
        for v in [
            s("@setDataFrame"),
            s("onMetaData"),
            Value::EcmaArray(vec![("audiocodecid".into(), n(10.0))]),
        ] {
            amf0::encode(&v, &mut meta);
        }
        let mut b = client.message(AMF0_DATA, 1, 0, &meta);
        b.extend(client.message(VIDEO, 1, 0, &[0x17, 0, 0, 0, 0]));
        b.extend(client.message(AUDIO, 1, 21, &[0xaf, 0]));
        // On another stream id: ignored.
        b.extend(client.message(VIDEO, 2, 0, &[0x17]));
        b.extend(client.command(
            0,
            &[s("FCUnpublish"), n(7.0), Value::Null, s("event-a?x=1")],
        ));
        b.extend(client.message(VIDEO, 1, 99, &[0x17]));
        sess.feed(&b).expect("feed");
        assert_eq!(
            all(&mut sess),
            vec![
                Event::Metadata { audio: true },
                Event::Video {
                    timestamp: 0,
                    data: vec![0x17, 0, 0, 0, 0]
                },
                Event::Audio {
                    timestamp: 21,
                    data: vec![0xaf, 0]
                },
                Event::Finished,
            ]
        );
    }

    #[test]
    fn publish_refused() {
        let mut client = Client::new();
        let mut sess = up_to_publish(&mut client);
        sess.refuse_publish("NetStream.Publish.Denied", "refused");
        let r = commands(&client.replies(&sess.take_output()));
        assert_eq!(r[0][0], s("onStatus"));
        assert_eq!(r[0][3].get("level"), Some(&s("error")));
        assert_eq!(r[0][3].get("code"), Some(&s("NetStream.Publish.Denied")));
        // Nothing more is handed out.
        sess.feed(&client.message(VIDEO, 1, 0, &[0x17]))
            .expect("feed");
        assert_eq!(sess.poll(), Ok(None));
    }

    #[test]
    fn media_before_publish_is_ignored_and_play_is_refused() {
        let mut client = Client::new();
        let mut sess = Session::new();
        client.replies(&sess.take_output());
        sess.feed(&client.command(0, &connect("live")))
            .expect("feed");
        sess.feed(&client.message(VIDEO, 1, 0, &[0x17]))
            .expect("feed");
        sess.feed(&client.command(0, &[s("createStream"), n(2.0), Value::Null]))
            .expect("feed");
        sess.feed(&client.command(1, &[s("play"), n(3.0), Value::Null, s("event-a")]))
            .expect("feed");
        assert_eq!(all(&mut sess), vec![]);
        let r = commands(&client.replies(&sess.take_output()));
        let last = r.last().expect("replies");
        assert_eq!(last[3].get("code"), Some(&s("NetStream.Play.Failed")));
    }

    #[test]
    fn protocol_errors() {
        let client = Client::new();
        let mut sess = Session::new();
        sess.feed(&client.command(0, &[s("connect"), n(1.0), Value::Null]))
            .expect("feed");
        assert_eq!(sess.poll(), Err(SessionError::ConnectWithoutApp));

        // Publish before connect.
        let mut sess = Session::new();
        sess.feed(&client.command(1, &[s("publish"), n(1.0), Value::Null, s("k")]))
            .expect("feed");
        assert_eq!(sess.poll(), Err(SessionError::BadPublish));

        // Publish on a stream never created.
        let mut sess = Session::new();
        sess.feed(&client.command(0, &connect("live")))
            .expect("feed");
        sess.feed(&client.command(1, &[s("publish"), n(2.0), Value::Null, s("k")]))
            .expect("feed");
        assert_eq!(sess.poll(), Err(SessionError::BadPublish));

        // A second publish.
        let mut client = Client::new();
        let mut sess = up_to_publish(&mut client);
        sess.accept_publish();
        let publish = client.command(1, &[s("publish"), n(7.0), Value::Null, s("k")]);
        sess.feed(&publish).expect("feed");
        assert_eq!(sess.poll(), Err(SessionError::SecondPublish));

        // Too many streams.
        let mut sess = Session::new();
        for i in 0..=MAX_STREAMS {
            sess.feed(&client.command(0, &[s("createStream"), n(f64::from(i)), Value::Null]))
                .expect("feed");
        }
        assert_eq!(sess.poll(), Err(SessionError::TooManyStreams));

        // Bad chunk size, short control messages, bad AMF0.
        for (t, body) in [
            (SET_CHUNK_SIZE, vec![0, 0, 0, 0]),
            (SET_CHUNK_SIZE, vec![0x80, 0, 0, 1]),
            (SET_CHUNK_SIZE, vec![1]),
            (WINDOW_ACK, vec![]),
            (AMF0_COMMAND, vec![0x02, 0xff]),
        ] {
            let mut sess = Session::new();
            sess.feed(&client.message(t, 0, 0, &body)).expect("feed");
            assert!(sess.poll().is_err(), "type {t} body {body:?}");
        }
    }

    #[test]
    fn chunk_size_change_applies_to_the_next_chunk() {
        let mut client = Client::new();
        let mut sess = up_to_publish(&mut client);
        sess.accept_publish();
        client.replies(&sess.take_output());
        let mut b = client.message(SET_CHUNK_SIZE, 0, 0, &3u32.to_be_bytes());
        client.writer.set_chunk_size(3);
        b.extend(client.message(VIDEO, 1, 40, &[0x27, 1, 0, 0, 0, 9, 9]));
        let mut sess2 = sess;
        sess2.feed(&b).expect("feed");
        assert_eq!(
            all(&mut sess2),
            vec![Event::Video {
                timestamp: 40,
                data: vec![0x27, 1, 0, 0, 0, 9, 9]
            }]
        );
    }

    #[test]
    fn ping_window_and_acknowledgements() {
        let mut client = Client::new();
        let mut sess = Session::new();
        client.replies(&sess.take_output());
        let mut b = client.message(USER_CONTROL, 0, 0, &[0, 6, 1, 2, 3, 4]);
        b.extend(client.message(WINDOW_ACK, 0, 0, &5000u32.to_be_bytes()));
        sess.feed(&b).expect("feed");
        assert_eq!(all(&mut sess), vec![]);
        let r = client.replies(&sess.take_output());
        assert_eq!(r[0].type_id, USER_CONTROL);
        assert_eq!(r[0].payload, vec![0, 7, 1, 2, 3, 4]);
        // 5000 more bytes (ignored messages) earn one acknowledgement.
        let filler = client.message(ACKNOWLEDGEMENT, 0, 0, &[0, 0, 0, 0]);
        let mut sent = b.len();
        let mut acks = Vec::new();
        while sent < 5000 + b.len() {
            sess.feed(&filler).expect("feed");
            sent += filler.len();
            all(&mut sess);
            acks.extend(
                client
                    .replies(&sess.take_output())
                    .into_iter()
                    .filter(|m| m.type_id == ACKNOWLEDGEMENT),
            );
        }
        assert_eq!(acks.len(), 1);
        let seq = be32(&acks[0].payload).expect("4 bytes");
        assert!(seq as usize >= 5000 && seq as usize <= sent);
    }

    #[test]
    fn budget_before_publish() {
        let mut sess = Session::new();
        assert_eq!(sess.feed(&vec![0u8; BUDGET_BEFORE_PUBLISH]), Ok(()));
        assert_eq!(sess.feed(&[0]), Err(SessionError::OverBudget));
    }

    #[test]
    fn metadata_forms() {
        let meta = |vals: Vec<Value>| metadata(&vals);
        let obj = |k: &str, v: Value| Value::Object(vec![(k.into(), v)]);
        assert_eq!(
            meta(vec![s("onMetaData"), obj("audiocodecid", s("mp4a"))]),
            Some(Event::Metadata { audio: true })
        );
        assert_eq!(
            meta(vec![
                s("@setDataFrame"),
                s("onMetaData"),
                obj("videocodecid", n(7.0))
            ]),
            Some(Event::Metadata { audio: false })
        );
        assert_eq!(
            meta(vec![s("onMetaData"), obj("audiocodecid", Value::Null)]),
            Some(Event::Metadata { audio: false })
        );
        assert_eq!(meta(vec![s("onTextData"), obj("a", n(1.0))]), None);
        assert_eq!(meta(vec![s("onMetaData"), n(1.0)]), None);
        assert_eq!(meta(vec![s("@setDataFrame")]), None);
        assert_eq!(meta(vec![]), None);
    }
}
