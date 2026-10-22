//! RTMP publishing in pure Rust, sans-IO: the server side of a publish
//! ([`Connection`]) and the publishing client ([`client::Publisher`]).
//!
//! Bytes from the socket go in, events and bytes to send come out; the
//! caller owns the socket, timeouts and policy. Layers:
//!
//! - [`handshake`]: C0/C1/C2 and S0/S1/S2, original and digest forms.
//! - [`chunk`]: the chunk stream, reassembling messages, with bounds.
//! - [`amf0`]: the values commands and metadata are made of.
//! - [`session`]: connect, createStream, publish and the media that follows.
//! - [`client`]: the same exchange from the publishing side.
//!
//! Video and audio arrive as FLV tag bodies with millisecond timestamps;
//! `sapflow-hls` turns them into LL-HLS (`FlvIngest`). Plain RTMP only: bring
//! your own TLS for RTMPS.
//!
//! Every input is hostile until the publish is accepted: nothing here
//! indexes without a bound, does unchecked arithmetic on a length or
//! allocates from a declared length. Fuzzed by `rtmp_session`, `rtmp_client`
//! and `amf0`.

#![warn(missing_docs)]

pub mod amf0;
pub mod chunk;
pub mod client;
pub mod handshake;
pub mod session;

pub use handshake::{Form, HandshakeError};
pub use sapflow_core::TrackSet;
pub use session::{Event, Session, SessionError};

use handshake::{Handshake, Progress};

/// Why a [`Connection`] failed.
#[derive(Debug, Clone, PartialEq)]
pub enum RtmpError {
    /// The handshake failed.
    Handshake(HandshakeError),
    /// The session failed after the handshake.
    Session(SessionError),
}

impl std::fmt::Display for RtmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handshake(e) => write!(f, "handshake: {e}"),
            Self::Session(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RtmpError {}

impl From<HandshakeError> for RtmpError {
    fn from(e: HandshakeError) -> Self {
        Self::Handshake(e)
    }
}

impl From<SessionError> for RtmpError {
    fn from(e: SessionError) -> Self {
        Self::Session(e)
    }
}

/// One RTMP connection from its first byte: the handshake, then the
/// session.
pub struct Connection {
    handshake: Option<Handshake>,
    form: Option<Form>,
    session: Session,
    out: Vec<u8>,
}

impl Connection {
    /// `seed` fills the handshake's random bytes; see [`Handshake::new`].
    pub fn new(seed: u64) -> Self {
        Self {
            handshake: Some(Handshake::new(seed)),
            form: None,
            session: Session::new(),
            out: Vec::new(),
        }
    }

    /// Which handshake the client used, once it is known.
    pub fn handshake_form(&self) -> Option<Form> {
        self.form
            .or_else(|| self.handshake.as_ref().and_then(Handshake::form))
    }

    /// Takes bytes from the socket. Feed a read at a time and poll until
    /// `None` in between.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), RtmpError> {
        let Some(hs) = &mut self.handshake else {
            return Ok(self.session.feed(bytes)?);
        };
        match hs.feed(bytes, &mut self.out)? {
            Progress::Continue => Ok(()),
            Progress::Done { leftover } => {
                self.form = hs.form();
                self.handshake = None;
                // The session's opening messages follow S0 S1 S2.
                self.out.extend(self.session.take_output());
                Ok(self.session.feed(&leftover)?)
            }
        }
    }

    /// The next event, or `None` when more input is needed.
    pub fn poll(&mut self) -> Result<Option<Event>, RtmpError> {
        if self.handshake.is_some() {
            return Ok(None);
        }
        Ok(self.session.poll()?)
    }

    /// Bytes to send to the client, in order.
    pub fn take_output(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.out);
        if self.handshake.is_none() {
            out.extend(self.session.take_output());
        }
        out
    }

    /// Accepts the publish an [`Event::Publish`] announced: the client may then
    /// send media. See [`Session::accept_publish`].
    pub fn accept_publish(&mut self) {
        self.session.accept_publish();
    }

    /// Refuses the publish with an `onStatus` error (`code` such as
    /// `NetStream.Publish.Denied`). The caller should then close the
    /// connection. See [`Session::refuse_publish`].
    pub fn refuse_publish(&mut self, code: &str, description: &str) {
        self.session.refuse_publish(code, description);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client side of a real publish, recorded through a TCP relay:
    /// `ffmpeg -f lavfi -i testsrc=size=160x90:rate=15 -f lavfi -i sine
    /// -t 3 -c:v libx264 -g 15 -b:v 100k -c:a aac -b:a 32k -f flv
    /// rtmp://127.0.0.1:<port>/live/event-x?user=u&pass=p` (ffmpeg 8.0.1).
    pub(crate) const FFMPEG_PUBLISH: &[u8] =
        include_bytes!("../tests/fixtures/ffmpeg-rtmp-publish.bin");

    fn run(slice: usize) -> (Vec<Event>, Option<Form>) {
        let mut conn = Connection::new(1);
        let mut events = Vec::new();
        for part in FFMPEG_PUBLISH.chunks(slice) {
            conn.feed(part).expect("feed");
            while let Some(ev) = conn.poll().expect("valid session") {
                if matches!(ev, Event::Publish { .. }) {
                    conn.accept_publish();
                }
                events.push(ev);
            }
            conn.take_output();
        }
        (events, conn.handshake_form())
    }

    #[test]
    fn recorded_ffmpeg_publish() {
        let (events, form) = run(FFMPEG_PUBLISH.len());
        assert_eq!(form, Some(Form::Digest), "ffmpeg sends a digest C1");
        assert_eq!(
            events.first(),
            Some(&Event::Publish {
                app: "live".into(),
                key: "event-x?user=u&pass=p".into()
            })
        );
        assert_eq!(events.get(1), Some(&Event::Metadata { audio: true }));
        assert_eq!(events.last(), Some(&Event::Finished));
        let video = events
            .iter()
            .filter(|e| matches!(e, Event::Video { .. }))
            .count();
        let audio = events
            .iter()
            .filter(|e| matches!(e, Event::Audio { .. }))
            .count();
        // 3 s at 15 fps, the sequence header and the end-of-sequence tag;
        // AAC at 48 kHz is ~47 frames a second.
        assert_eq!(video, 47);
        let Some(Event::Video { data, .. }) = events
            .iter()
            .rev()
            .find(|e| matches!(e, Event::Video { .. }))
        else {
            panic!("no video");
        };
        assert_eq!(data.get(1), Some(&2), "AVC end of sequence last");
        assert!((140..=150).contains(&audio), "{audio} audio messages");
    }

    #[test]
    fn recorded_publish_is_the_same_however_it_is_sliced() {
        let (whole, _) = run(FFMPEG_PUBLISH.len());
        for slice in [1, 7, 97, 1536, 4096] {
            assert_eq!(run(slice).0, whole, "slice {slice}");
        }
    }

    #[test]
    fn output_starts_with_the_handshake_then_the_session() {
        let mut conn = Connection::new(1);
        conn.feed(&FFMPEG_PUBLISH[..1 + 1536]).expect("feed");
        let out = conn.take_output();
        assert_eq!(out.len(), 1 + 2 * 1536, "S0 S1 S2 only until C2");
        conn.feed(&FFMPEG_PUBLISH[1 + 1536..2 * 1536 + 1])
            .expect("feed");
        let out = conn.take_output();
        // Set Chunk Size on csid 2 comes first.
        assert_eq!(&out[..8], &[0x02, 0, 0, 0, 0, 0, 4, 1]);
    }
}
