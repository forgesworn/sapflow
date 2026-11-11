//! The DTLS 1.2 server state machine, sans-IO.
//!
//! Flights (RFC 6347 section 4.2.4), with an optional cookie exchange:
//!
//! ```text
//! ClientHello                 -->
//!                             <--  HelloVerifyRequest      (if enabled)
//! ClientHello (cookie)        -->
//!                             <--  ServerHello, Certificate,
//!                                  ServerKeyExchange,
//!                                  CertificateRequest, ServerHelloDone
//! Certificate,
//! ClientKeyExchange,
//! CertificateVerify,
//! [ChangeCipherSpec], Finished -->
//!                             <--  [ChangeCipherSpec], Finished
//! ```
//!
//! The server owns no socket or clock: [`Server::handle_datagram`] and
//! [`Server::handle_timeout`] take input and the time, and
//! [`Server::poll_transmit`], [`Server::poll_event`] and
//! [`Server::poll_timeout`] give output.

use crate::crypto::{self, Entropy, KeyBlock, MasterSecret};
use crate::error::alert;
use crate::flight::{self, Flight, OutMessage, Sequences};
use crate::identity::{self, Fingerprint, Identity};
use crate::message::{self, ClientHello, Negotiated, SrtpProfile};
use crate::reassembly::{Limits, Message, Push, Reassembler};
use crate::record::{self, DTLS_1_2, Protection, Record, ReplayWindow};
use crate::{ConfigError, Error, Limit};
use p256::elliptic_curve::sec1::ToSec1Point;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Most certificates read from the peer's Certificate message; only the
/// leaf is used.
pub const MAX_CHAIN: usize = 4;
/// Most datagrams queued for [`Server::poll_transmit`]. One input never
/// produces more than a flight and an alert; if the caller stops polling,
/// further output is dropped and counted in [`Stats::output_dropped`].
pub const MAX_QUEUED: usize = 16;
/// Messages past the next expected one that reassembly buffers. The
/// client's longest flight is four handshake messages.
const AHEAD: u16 = 4;
/// The TLS 1.3 downgrade sentinels (RFC 8446 section 4.1.3), which a DTLS
/// 1.2 server must not send by chance either.
const DOWNGRADE: [&[u8; 8]; 2] = [b"DOWNGRD\x01", b"DOWNGRD\x00"];

/// Limits and timers. Every field is a bound with a refusal path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Largest datagram we send, in bytes. Our messages are fragmented to
    /// fit. 256 to 16384; default 1200.
    pub mtu: usize,
    /// Largest datagram accepted; longer ones are dropped unread. 256 to
    /// 65535; default 1500.
    pub max_datagram: usize,
    /// First retransmission timeout (RFC 6347 section 4.2.4.1 suggests
    /// 1 s). Default 1 s.
    pub initial_retransmit: Duration,
    /// Longest retransmission timeout; the timer doubles up to it. Default
    /// 60 s, the RFC's floor for the cap.
    pub max_retransmit: Duration,
    /// Most sends of one flight, the first included, whatever triggered
    /// them. After that many sends of flight 4 with no reply the handshake
    /// fails with [`Error::Timeout`]. 1 to 32; default 6.
    pub max_transmissions: u32,
    /// Whether to send a HelloVerifyRequest cookie first. Default off: the
    /// server is used only behind ICE, which proves return routability for
    /// the address before any DTLS datagram is accepted, and that is what
    /// the cookie is for (RFC 6347 section 4.2.1 allows turning it off
    /// where amplification is not a problem). Off saves a round trip.
    pub hello_verify: bool,
    /// Largest handshake message accepted, by declared length. 512 to
    /// 65536; default 8192 (Chrome's ClientHello is 1413 bytes).
    pub max_message_len: usize,
    /// Most fragments accepted for one message, duplicates included. 1 to
    /// 1024; default 32.
    pub max_fragments: usize,
    /// Most bytes held by messages being reassembled. At least
    /// `max_message_len`, at most 1 MiB; default 16384.
    pub max_buffered: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mtu: 1200,
            max_datagram: 1500,
            initial_retransmit: Duration::from_secs(1),
            max_retransmit: Duration::from_secs(60),
            max_transmissions: 6,
            hello_verify: false,
            max_message_len: 8192,
            max_fragments: 32,
            max_buffered: 16384,
        }
    }
}

impl Config {
    /// Checks every field's range.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let check = |ok: bool, what| if ok { Ok(()) } else { Err(ConfigError(what)) };
        check((256..=16384).contains(&self.mtu), "mtu")?;
        check((256..=65535).contains(&self.max_datagram), "max_datagram")?;
        check(
            self.initial_retransmit >= Duration::from_millis(10)
                && self.initial_retransmit <= self.max_retransmit
                && self.max_retransmit <= Duration::from_secs(600),
            "retransmit timers",
        )?;
        check(
            (1..=32).contains(&self.max_transmissions),
            "max_transmissions",
        )?;
        check(
            (512..=65536).contains(&self.max_message_len),
            "max_message_len",
        )?;
        check((1..=1024).contains(&self.max_fragments), "max_fragments")?;
        check(
            self.max_buffered >= self.max_message_len && self.max_buffered <= 1 << 20,
            "max_buffered",
        )?;
        Ok(())
    }
}

/// The exporter output for DTLS-SRTP (RFC 5764 section 4.2), the input to
/// `sapflow_rtp::srtp::split_keying_material`. Zeroed on drop; `Debug`
/// shows only its length.
pub struct KeyingMaterial(Zeroizing<Vec<u8>>);

impl KeyingMaterial {
    /// The bytes: `client_key || server_key || client_salt || server_salt`.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for KeyingMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyingMaterial({} bytes)", self.0.len())
    }
}

/// The handshake completed.
#[derive(Debug)]
pub struct Connected {
    /// The SRTP profile negotiated.
    pub profile: SrtpProfile,
    /// The peer certificate's fingerprint, which matched the expected one.
    pub peer_fingerprint: Fingerprint,
    /// DTLS-SRTP keying material, `profile.keying_material_len()` bytes.
    /// This is the only copy; the server keeps none.
    pub keying_material: KeyingMaterial,
}

/// Something the caller should act on.
#[derive(Debug)]
pub enum Event {
    /// The handshake completed; SRTP can start.
    Connected(Connected),
    /// The peer sent `close_notify` after the handshake, and the server
    /// answered it.
    Closed,
}

/// Where the association stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No ClientHello accepted yet.
    AwaitingHello,
    /// Flight 4 sent; waiting for the client's flight.
    Handshaking,
    /// The handshake completed.
    Connected,
    /// Closed by either side with `close_notify`, or by [`Server::close`].
    Closed,
    /// Failed; one fatal alert was sent if [`Error::alert`] names one.
    Failed(Error),
}

/// Counters. None of them counts anything secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Datagrams above [`Config::max_datagram`], or that arrived after the
    /// association ended.
    pub datagrams_dropped: u64,
    /// Records dropped: bad version, unknown epoch, replayed, failed
    /// authentication, or not acceptable in this state.
    pub records_dropped: u64,
    /// Application data records, which sapflow does not use.
    pub application_data_dropped: u64,
    /// Flight sends after the first.
    pub retransmissions: u64,
    /// Output dropped because [`MAX_QUEUED`] datagrams were waiting.
    pub output_dropped: u64,
    /// ECDSA signatures made (one per handshake).
    pub signatures: u64,
    /// ECDH agreements made (at most one per handshake).
    pub key_agreements: u64,
    /// Signature verifications made (at most one per handshake).
    pub verifications: u64,
}

enum Stage {
    Certificate,
    KeyExchange,
    Verify { hash: [u8; 32] },
    Finished { expected: [u8; 12] },
}

enum Phase {
    Hello {
        hello_verifies: u32,
    },
    Flight5 {
        stage: Stage,
    },
    Connected {
        finished_seq: u16,
        refused_renegotiation: bool,
    },
    Closed,
    Failed(Error),
}

/// Per-handshake secrets and state, dropped when the handshake ends.
struct Handshake {
    client_random: [u8; 32],
    server_random: [u8; 32],
    negotiated: Negotiated,
    ecdh: Option<p256::SecretKey>,
    transcript: Sha256,
    master: Option<MasterSecret>,
    peer: Option<p256::ecdsa::VerifyingKey>,
    pending_read: Option<Protection>,
    pending_write: Option<Protection>,
    next_server_seq: u16,
}

/// A DTLS 1.2 server for one peer (one ICE-validated address pair).
pub struct Server {
    config: Config,
    identity: Arc<Identity>,
    expected: Fingerprint,
    entropy: Entropy,
    cookie_secret: Option<Zeroizing<[u8; 32]>>,
    phase: Phase,
    reassembler: Reassembler,
    hs: Option<Box<Handshake>>,
    read: Option<Protection>,
    replay: ReplayWindow,
    write: Option<Protection>,
    seqs: Sequences,
    sent_any: bool,
    /// The ClientHello our last flight answered: its seq and body. Only a
    /// repeat of exactly this (from offset 0) triggers a resend.
    answered: Option<(u16, Vec<u8>)>,
    flight: Option<Flight>,
    transmissions: u32,
    timer: Option<Instant>,
    interval: Duration,
    last_sent: Option<Instant>,
    sent_this_input: bool,
    out: VecDeque<Vec<u8>>,
    events: VecDeque<Event>,
    profile: Option<SrtpProfile>,
    peer_fingerprint: Option<Fingerprint>,
    stats: Stats,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("state", &self.state())
            .field("profile", &self.profile)
            .field("peer_fingerprint", &self.peer_fingerprint)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// Whether a random ends in a TLS 1.3 downgrade sentinel, which a DTLS 1.2
/// server's random must never do (RFC 8446 section 4.1.3).
pub(crate) fn downgrade_sentinel(random: &[u8; 32]) -> bool {
    random
        .get(24..)
        .is_some_and(|tail| DOWNGRADE.iter().any(|s| s.as_slice() == tail))
}

fn whole(m: &Message) -> Result<Vec<u8>, Error> {
    OutMessage {
        msg_type: m.msg_type,
        seq: m.seq,
        body: m.body.clone(),
    }
    .whole()
}

impl Server {
    /// A server that will accept only a peer whose certificate's SHA-256 is
    /// `expected_peer`, the `a=fingerprint` of the offer that came with the
    /// admitted request. There is no way to skip this check.
    pub fn new(
        config: Config,
        identity: Arc<Identity>,
        expected_peer: Fingerprint,
    ) -> Result<Self, ConfigError> {
        Self::with_entropy(config, identity, expected_peer, Entropy::System)
    }

    /// As [`Server::new`], with randomness from a seed so that a run
    /// replays byte for byte. Not for production use.
    #[cfg(any(test, feature = "testing"))]
    pub fn new_seeded(
        config: Config,
        identity: Arc<Identity>,
        expected_peer: Fingerprint,
        seed: [u8; 32],
    ) -> Result<Self, ConfigError> {
        let entropy = Entropy::Seeded {
            seed: Zeroizing::new(seed),
            counter: 0,
        };
        Self::with_entropy(config, identity, expected_peer, entropy)
    }

    fn with_entropy(
        config: Config,
        identity: Arc<Identity>,
        expected: Fingerprint,
        entropy: Entropy,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        let limits = Limits {
            max_message: config.max_message_len,
            max_fragments: config.max_fragments,
            max_buffered: config.max_buffered,
            ahead: AHEAD,
        };
        Ok(Self {
            interval: config.initial_retransmit,
            config,
            identity,
            expected,
            entropy,
            cookie_secret: None,
            phase: Phase::Hello { hello_verifies: 0 },
            reassembler: Reassembler::new(limits),
            hs: None,
            read: None,
            replay: ReplayWindow::default(),
            write: None,
            seqs: Sequences::default(),
            sent_any: false,
            answered: None,
            flight: None,
            transmissions: 0,
            timer: None,
            last_sent: None,
            sent_this_input: false,
            out: VecDeque::new(),
            events: VecDeque::new(),
            profile: None,
            peer_fingerprint: None,
            stats: Stats::default(),
        })
    }

    /// Where the association stands.
    pub fn state(&self) -> State {
        match &self.phase {
            Phase::Hello { .. } => State::AwaitingHello,
            Phase::Flight5 { .. } => State::Handshaking,
            Phase::Connected { .. } => State::Connected,
            Phase::Closed => State::Closed,
            Phase::Failed(e) => State::Failed(*e),
        }
    }

    /// The SRTP profile, once connected.
    pub fn profile(&self) -> Option<SrtpProfile> {
        self.profile
    }

    /// The peer's certificate fingerprint, once it has matched.
    pub fn peer_fingerprint(&self) -> Option<Fingerprint> {
        self.peer_fingerprint
    }

    /// Counters.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Bytes held by handshake messages being reassembled.
    pub fn buffered(&self) -> usize {
        self.reassembler.buffered()
    }

    /// The next datagram to send to the peer.
    pub fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    /// The next event.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// When [`Server::handle_timeout`] should next be called, if at all.
    pub fn poll_timeout(&self) -> Option<Instant> {
        self.timer
    }

    fn terminal(&self) -> bool {
        matches!(self.phase, Phase::Closed | Phase::Failed(_))
    }

    fn handshaking(&self) -> bool {
        matches!(self.phase, Phase::Hello { .. } | Phase::Flight5 { .. })
    }

    /// One datagram from the peer. Invalid records are dropped silently
    /// (RFC 6347 section 4.1.2.7). An `Err` means the association failed:
    /// one fatal alert, if any, is queued, and later input is ignored.
    pub fn handle_datagram(&mut self, now: Instant, datagram: &[u8]) -> Result<(), Error> {
        if self.terminal() || datagram.len() > self.config.max_datagram {
            self.stats.datagrams_dropped += 1;
            return Ok(());
        }
        self.sent_this_input = false;
        let result = self.datagram(now, datagram);
        self.settle(result)
    }

    /// The timer from [`Server::poll_timeout`] fired: resend the flight,
    /// or fail with [`Error::Timeout`] once it has been sent
    /// [`Config::max_transmissions`] times.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
        let Some(at) = self.timer else {
            return Ok(());
        };
        if now < at || self.terminal() {
            return Ok(());
        }
        if self.transmissions >= self.config.max_transmissions {
            return self.settle(Err(Error::Timeout));
        }
        self.interval = self
            .interval
            .saturating_mul(2)
            .min(self.config.max_retransmit);
        self.sent_this_input = false;
        let result = self.send_flight(now);
        self.timer = Some(now + self.interval);
        self.settle(result)
    }

    /// Closes the association: `close_notify` if connected, nothing during
    /// the handshake. Keys are dropped.
    pub fn close(&mut self) {
        if matches!(self.phase, Phase::Connected { .. }) {
            self.queue_alert(alert::WARNING, alert::CLOSE_NOTIFY);
        }
        if !self.terminal() {
            self.end(Phase::Closed);
        }
    }

    fn settle(&mut self, result: Result<(), Error>) -> Result<(), Error> {
        if let Err(e) = result {
            if !self.terminal() {
                if let Some(desc) = e.alert() {
                    self.queue_alert(alert::FATAL, desc);
                }
                self.end(Phase::Failed(e));
            }
            return Err(e);
        }
        Ok(())
    }

    /// Ends the association: secrets, flight and timer go.
    fn end(&mut self, phase: Phase) {
        self.phase = phase;
        self.hs = None;
        self.answered = None;
        self.read = None;
        self.write = None;
        self.flight = None;
        self.timer = None;
        self.reassembler.restart_at(self.reassembler.next_seq());
    }

    fn queue(&mut self, datagram: Vec<u8>) {
        if self.out.len() >= MAX_QUEUED {
            self.stats.output_dropped += 1;
        } else {
            self.out.push_back(datagram);
            self.sent_any = true;
        }
    }

    /// One alert at the epoch the peer can read: epoch 1 only once our
    /// Finished has been sent.
    fn queue_alert(&mut self, level: u8, description: u8) {
        let write = match self.phase {
            Phase::Connected { .. } => self.write.as_ref(),
            _ => None,
        };
        if let Ok(a) = flight::alert(level, description, &mut self.seqs, write) {
            self.queue(a);
        }
    }

    fn send_flight(&mut self, now: Instant) -> Result<(), Error> {
        let Some(f) = &self.flight else {
            return Ok(());
        };
        let write = if f.finished.is_some() {
            self.write.as_ref()
        } else {
            None
        };
        let grams = flight::render(f, self.config.mtu, &mut self.seqs, write)?;
        if self.transmissions > 0 {
            self.stats.retransmissions += 1;
        }
        self.transmissions += 1;
        self.last_sent = Some(now);
        self.sent_this_input = true;
        for g in grams {
            self.queue(g);
        }
        Ok(())
    }

    /// A retransmission from the peer suggests our flight was lost (RFC
    /// 6347 section 4.2.4). At most one resend per input datagram, none
    /// beyond [`Config::max_transmissions`], and none sooner than half the
    /// initial timeout after the last send.
    fn resend(&mut self, now: Instant) -> Result<(), Error> {
        if self.sent_this_input
            || self.flight.is_none()
            || self.transmissions >= self.config.max_transmissions
        {
            return Ok(());
        }
        if self
            .last_sent
            .is_some_and(|last| now < last + self.config.initial_retransmit / 2)
        {
            return Ok(());
        }
        self.send_flight(now)?;
        if self.timer.is_some() {
            self.timer = Some(now + self.interval);
        }
        Ok(())
    }

    fn datagram(&mut self, now: Instant, datagram: &[u8]) -> Result<(), Error> {
        for rec in record::records(datagram) {
            self.record(now, &rec)?;
            if self.terminal() {
                break;
            }
        }
        Ok(())
    }

    fn drop_record(&mut self) -> Result<(), Error> {
        self.stats.records_dropped += 1;
        Ok(())
    }

    fn record(&mut self, now: Instant, rec: &Record<'_>) -> Result<(), Error> {
        match rec.epoch {
            0 => {
                if !message::first_flight_version(rec.version) {
                    return self.drop_record();
                }
                match rec.content_type {
                    record::HANDSHAKE if self.handshaking() => {
                        for f in message::fragments(rec.body) {
                            self.fragment(now, 0, rec.seq, &f)?;
                            if !self.handshaking() {
                                break;
                            }
                        }
                        Ok(())
                    }
                    record::CHANGE_CIPHER_SPEC => self.change_cipher_spec(rec.body),
                    record::ALERT if self.handshaking() => self.peer_alert(rec.body),
                    // Unauthenticated after the handshake, or not ours.
                    _ => self.drop_record(),
                }
            }
            1 => {
                if rec.version != DTLS_1_2 || !self.replay.check(rec.seq) {
                    return self.drop_record();
                }
                let Some(plain) = self.read.as_ref().and_then(|r| r.open(rec)) else {
                    return self.drop_record();
                };
                self.replay.accept(rec.seq);
                match rec.content_type {
                    record::HANDSHAKE => {
                        for f in message::fragments(&plain) {
                            self.fragment(now, 1, rec.seq, &f)?;
                            if self.terminal() {
                                break;
                            }
                        }
                        Ok(())
                    }
                    record::ALERT => self.peer_alert(&plain),
                    record::APPLICATION_DATA => {
                        self.stats.application_data_dropped += 1;
                        Ok(())
                    }
                    _ => self.drop_record(),
                }
            }
            _ => self.drop_record(),
        }
    }

    fn change_cipher_spec(&mut self, body: &[u8]) -> Result<(), Error> {
        // Accepted only once CertificateVerify is in, so the read keys can
        // never switch before the client has proved its key.
        let ready = matches!(
            self.phase,
            Phase::Flight5 {
                stage: Stage::Finished { .. },
                ..
            }
        );
        if body != [1] || !ready || self.read.is_some() {
            return self.drop_record();
        }
        self.read = self.hs.as_mut().and_then(|h| h.pending_read.take());
        Ok(())
    }

    fn peer_alert(&mut self, body: &[u8]) -> Result<(), Error> {
        let [level, description] = <[u8; 2]>::try_from(body).unwrap_or([0, 255]);
        if level != alert::WARNING && level != alert::FATAL {
            return self.drop_record();
        }
        if description == alert::CLOSE_NOTIFY {
            if matches!(self.phase, Phase::Connected { .. }) {
                self.queue_alert(alert::WARNING, alert::CLOSE_NOTIFY);
                self.end(Phase::Closed);
                self.events.push_back(Event::Closed);
                return Ok(());
            }
            return Err(Error::PeerAlert(description));
        }
        if level == alert::FATAL {
            return Err(Error::PeerAlert(description));
        }
        Ok(())
    }

    fn fragment(
        &mut self,
        now: Instant,
        epoch: u16,
        record_seq: u64,
        f: &message::Fragment<'_>,
    ) -> Result<(), Error> {
        if let Phase::Connected {
            finished_seq,
            refused_renegotiation,
        } = self.phase
        {
            // Only authenticated (epoch 1) handshake data reaches here.
            if f.msg_type == message::FINISHED && f.seq == finished_seq && f.offset == 0 {
                return self.resend(now);
            }
            if f.msg_type == message::CLIENT_HELLO && f.seq > finished_seq && !refused_renegotiation
            {
                self.phase = Phase::Connected {
                    finished_seq,
                    refused_renegotiation: true,
                };
                self.queue_alert(alert::WARNING, alert::NO_RENEGOTIATION);
            }
            return Ok(());
        }
        match self.reassembler.push(epoch, f)? {
            Push::Old { .. } => {
                if self.repeats_answered_hello(f) {
                    self.resend(now)?;
                }
                Ok(())
            }
            Push::Dropped => self.drop_record(),
            Push::Stored => {
                while let Some(m) = self.reassembler.pop() {
                    self.message(now, record_seq, m)?;
                    if !self.handshaking() {
                        break;
                    }
                }
                Ok(())
            }
        }
    }

    /// Whether `f` is the start of a retransmission of the ClientHello our
    /// last flight answered. A different hello under the same seq (another
    /// random, say) is ignored rather than answered with our flight.
    fn repeats_answered_hello(&self, f: &message::Fragment<'_>) -> bool {
        let Some((seq, body)) = &self.answered else {
            return false;
        };
        f.msg_type == message::CLIENT_HELLO
            && f.seq == *seq
            && f.offset == 0
            && !f.data.is_empty()
            && usize::try_from(f.length).is_ok_and(|n| n == body.len())
            && body.get(..f.data.len()) == Some(f.data)
    }

    fn message(&mut self, now: Instant, record_seq: u64, m: Message) -> Result<(), Error> {
        match self.phase {
            Phase::Hello { .. } => self.client_hello(now, record_seq, m),
            Phase::Flight5 { .. } => self.client_flight(now, m),
            _ => Ok(()),
        }
    }

    fn cookie(&mut self, hello: &ClientHello<'_>) -> Result<[u8; 32], Error> {
        let secret = match &self.cookie_secret {
            Some(s) => s.clone(),
            None => {
                let s = self.entropy.array::<32>()?;
                self.cookie_secret = Some(s.clone());
                s
            }
        };
        // The parameters a client must repeat (RFC 6347 section 4.2.1).
        crypto::hmac_sha256(
            secret.as_slice(),
            &[
                &hello.version.to_be_bytes(),
                &hello.random,
                &[u8::try_from(hello.session_id.len()).map_err(|_| Error::Internal)?],
                hello.session_id,
                hello.cipher_suites,
                &[0xff],
                hello.compression_methods,
            ],
        )
    }

    fn client_hello(&mut self, now: Instant, record_seq: u64, m: Message) -> Result<(), Error> {
        if m.msg_type != message::CLIENT_HELLO || m.epoch != 0 {
            return Err(Error::UnexpectedMessage);
        }
        let hello = ClientHello::parse(&m.body)?;
        let negotiated = hello.negotiate()?;
        if !self.sent_any {
            // Our first record reuses the ClientHello's record sequence
            // number (RFC 6347 section 4.2.1); later ones count up.
            self.seqs.epoch0 = record_seq;
        }
        if self.config.hello_verify {
            let cookie = self.cookie(&hello)?;
            if !bool::from(hello.cookie.ct_eq(&cookie)) {
                let Phase::Hello { hello_verifies, .. } = self.phase else {
                    return Err(Error::Internal);
                };
                if hello_verifies >= self.config.max_transmissions {
                    return Err(Error::Limit(Limit::HelloVerify));
                }
                self.phase = Phase::Hello {
                    hello_verifies: hello_verifies + 1,
                };
                self.answered = Some((m.seq, m.body.clone()));
                self.flight = Some(Flight {
                    messages: vec![OutMessage {
                        msg_type: message::HELLO_VERIFY_REQUEST,
                        seq: m.seq,
                        body: message::hello_verify_request(&cookie)?,
                    }],
                    finished: None,
                });
                self.transmissions = 0;
                return self.send_flight(now);
            }
        }
        self.server_flight(now, &m, &hello, negotiated)
    }

    fn server_random(&mut self) -> Result<[u8; 32], Error> {
        for _ in 0..4 {
            let r = *self.entropy.array::<32>()?;
            if !downgrade_sentinel(&r) {
                return Ok(r);
            }
        }
        Err(Error::Entropy)
    }

    fn ecdh_key(&mut self) -> Result<p256::SecretKey, Error> {
        for _ in 0..4 {
            let bytes = self.entropy.array::<32>()?;
            if let Ok(k) = p256::SecretKey::from_slice(bytes.as_slice()) {
                return Ok(k);
            }
        }
        Err(Error::Entropy)
    }

    fn server_flight(
        &mut self,
        now: Instant,
        m: &Message,
        hello: &ClientHello<'_>,
        negotiated: Negotiated,
    ) -> Result<(), Error> {
        let server_random = self.server_random()?;
        let ecdh = self.ecdh_key()?;
        let point = ecdh.public_key().to_sec1_point(false);
        let params = message::ecdh_params(point.as_bytes())?;
        let digest = crypto::sha256(&[&hello.random, &server_random, &params]);
        let signature = identity::sign_der(self.identity.key(), &digest)?;
        self.stats.signatures += 1;

        let seq = |k: u16| m.seq.checked_add(k).ok_or(Error::Limit(Limit::Sequence));
        let messages = vec![
            OutMessage {
                msg_type: message::SERVER_HELLO,
                seq: seq(0)?,
                body: message::server_hello(&server_random, &negotiated)?,
            },
            OutMessage {
                msg_type: message::CERTIFICATE,
                seq: seq(1)?,
                body: message::certificate(self.identity.certificate_der())?,
            },
            OutMessage {
                msg_type: message::SERVER_KEY_EXCHANGE,
                seq: seq(2)?,
                body: message::server_key_exchange(&params, &signature)?,
            },
            OutMessage {
                msg_type: message::CERTIFICATE_REQUEST,
                seq: seq(3)?,
                body: message::certificate_request()?,
            },
            OutMessage {
                msg_type: message::SERVER_HELLO_DONE,
                seq: seq(4)?,
                body: Vec::new(),
            },
        ];
        let mut transcript = Sha256::new();
        transcript.update(whole(m)?);
        for out in &messages {
            transcript.update(out.whole()?);
        }
        self.hs = Some(Box::new(Handshake {
            client_random: hello.random,
            server_random,
            negotiated,
            ecdh: Some(ecdh),
            transcript,
            master: None,
            peer: None,
            pending_read: None,
            pending_write: None,
            next_server_seq: seq(5)?,
        }));
        self.flight = Some(Flight {
            messages,
            finished: None,
        });
        self.phase = Phase::Flight5 {
            stage: Stage::Certificate,
        };
        self.answered = Some((m.seq, m.body.clone()));
        self.transmissions = 0;
        self.interval = self.config.initial_retransmit;
        self.send_flight(now)?;
        self.timer = Some(now + self.interval);
        Ok(())
    }

    fn client_flight(&mut self, now: Instant, m: Message) -> Result<(), Error> {
        let Phase::Flight5 { stage } = &mut self.phase else {
            return Err(Error::Internal);
        };
        let hs = self.hs.as_mut().ok_or(Error::Internal)?;
        let next = match (&*stage, m.msg_type, m.epoch) {
            (Stage::Certificate, message::CERTIFICATE, 0) => {
                let leaf = message::certificate_leaf(&m.body, MAX_CHAIN)?;
                // Compared before the DER is parsed at all.
                let fingerprint = Fingerprint::of_certificate(leaf);
                if !fingerprint.ct_eq(&self.expected) {
                    return Err(Error::FingerprintMismatch);
                }
                hs.peer = Some(identity::peer_key(leaf)?);
                self.peer_fingerprint = Some(fingerprint);
                hs.transcript.update(whole(&m)?);
                Stage::KeyExchange
            }
            (Stage::KeyExchange, message::CLIENT_KEY_EXCHANGE, 0) => {
                let point = message::client_key_exchange(&m.body)?;
                let public =
                    p256::PublicKey::from_sec1_bytes(point).map_err(|_| Error::BadKeyShare)?;
                let secret = hs.ecdh.take().ok_or(Error::Internal)?;
                let shared =
                    p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
                drop(secret);
                self.stats.key_agreements += 1;
                hs.transcript.update(whole(&m)?);
                // RFC 7627 section 3: ClientHello up to and including
                // ClientKeyExchange. It is also what CertificateVerify
                // signs (everything before it).
                let session_hash: [u8; 32] = hs.transcript.clone().finalize().into();
                let master =
                    crypto::extended_master_secret(shared.raw_secret_bytes(), &session_hash)?;
                drop(shared);
                let keys = KeyBlock::derive(&master, &hs.client_random, &hs.server_random)?;
                hs.pending_read = Some(Protection::new(keys.client_key()?, keys.client_iv()?)?);
                hs.pending_write = Some(Protection::new(keys.server_key()?, keys.server_iv()?)?);
                hs.master = Some(master);
                Stage::Verify { hash: session_hash }
            }
            (Stage::Verify { hash }, message::CERTIFICATE_VERIFY, 0) => {
                let signature = message::certificate_verify(&m.body)?;
                let key = hs.peer.as_ref().ok_or(Error::Internal)?;
                self.stats.verifications += 1;
                identity::verify_der(key, hash, signature)?;
                hs.transcript.update(whole(&m)?);
                let hash: [u8; 32] = hs.transcript.clone().finalize().into();
                let master = hs.master.as_ref().ok_or(Error::Internal)?;
                Stage::Finished {
                    expected: crypto::finished(master, b"client finished", &hash)?,
                }
            }
            (Stage::Finished { expected }, message::FINISHED, 1) => {
                let got = message::finished(&m.body)?;
                if !bool::from(got.ct_eq(expected)) {
                    return Err(Error::BadFinished);
                }
                hs.transcript.update(whole(&m)?);
                let hash: [u8; 32] = hs.transcript.clone().finalize().into();
                let master = hs.master.as_ref().ok_or(Error::Internal)?;
                let server_finished = crypto::finished(master, b"server finished", &hash)?;
                let profile = hs.negotiated.profile;
                let mut material = Zeroizing::new(vec![0u8; profile.keying_material_len()]);
                crypto::export(
                    master,
                    b"EXTRACTOR-dtls_srtp",
                    &hs.client_random,
                    &hs.server_random,
                    &mut material,
                )?;
                let fin = OutMessage {
                    msg_type: message::FINISHED,
                    seq: hs.next_server_seq,
                    body: server_finished.to_vec(),
                };
                self.write = hs.pending_write.take();
                let peer_fingerprint = self.peer_fingerprint.ok_or(Error::Internal)?;
                self.hs = None;
                self.answered = None;
                self.flight = Some(Flight {
                    messages: Vec::new(),
                    finished: Some(fin),
                });
                self.phase = Phase::Connected {
                    finished_seq: m.seq,
                    refused_renegotiation: false,
                };
                self.timer = None;
                self.transmissions = 0;
                self.profile = Some(profile);
                self.send_flight(now)?;
                self.events.push_back(Event::Connected(Connected {
                    profile,
                    peer_fingerprint,
                    keying_material: KeyingMaterial(material),
                }));
                return Ok(());
            }
            _ => return Err(Error::UnexpectedMessage),
        };
        self.phase = Phase::Flight5 { stage: next };
        Ok(())
    }
}
