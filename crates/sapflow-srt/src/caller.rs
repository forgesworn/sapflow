//! Live caller, written from draft-sharabayko-srt-01 sections
//! 3.2.1 and 4.3.1 and our own packet/listener modules.
//!
//! The adapter owns its UDP socket and supplies monotonic time. Drain
//! `poll_transmit(now)` immediately to that socket, then wait for input or
//! `poll_timeout()`. A returned datagram counts as sent: the adapter must
//! stop the session if it cannot submit it. Do not buffer paced datagrams
//! in another unbounded queue. Qualification is still pending for
//! seamless reconnection, late-drop negotiation or adaptive bitrate.

use std::collections::{VecDeque, hash_map::RandomState};
use std::hash::BuildHasher;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::crypto::{CryptoConfig, Encryptor};
use crate::listener::SRT_VERSION;
use crate::packet::{
    Control, ControlPacket, Extension, Handshake, HandshakeKind, MAX_STREAM_ID, Packet, SRT_MAGIC,
    SrtHandshake, ext_flags, srt_flags,
};
use crate::sender::{Failure, SendConfig, SendError, SendStats, Sender};
use crate::seq::Seq;

/// CRYPT is the specification's mandatory legacy KK-understanding flag,
/// NOT a claim of encryption. Configured sessions require verified KM.
/// We are send-only: do not advertise receiving or periodic NAK capability.
const FLAGS: u32 = srt_flags::TSBPDSND | srt_flags::CRYPT | srt_flags::REXMITFLG;

/// Settings for a [`Caller`]; [`Caller::new`] checks them and fails with a
/// [`ConfigError`] if any is invalid.
#[derive(Debug, Clone)]
pub struct CallerConfig {
    /// UTF-8, at most 512 bytes, no NUL. Sent in clear in this lab milestone.
    pub stream_id: String,
    /// Receiver latency (TSBPD delay) we ask the listener to use, rounded up
    /// to whole milliseconds; it may use more. 1 ms to 65535 ms; default 120
    /// ms.
    pub latency: Duration,
    /// How long to wait for the handshake to complete, counted from creation;
    /// at least `handshake_retry`, at most 60 s; default 5 s.
    pub connect_timeout: Duration,
    /// Interval between handshake packets while connecting; 10 ms to 5 s;
    /// default 250 ms.
    pub handshake_retry: Duration,
    /// Most handshake packets sent before giving up with
    /// [`CallerCloseReason::HandshakeLimit`]; 1 to 1024; default 32.
    pub max_handshake_packets: u32,
    /// A connected session ends after this long without a valid packet from
    /// the listener; 10 ms to 60 s; default 5 s.
    pub peer_idle: Duration,
    /// Limits and pacing for the sending window.
    pub send: SendConfig,
    /// None explicitly selects plaintext; Some requires encryption.
    pub crypto: Option<CryptoConfig>,
}

impl Default for CallerConfig {
    fn default() -> Self {
        Self {
            stream_id: String::new(),
            latency: Duration::from_millis(120),
            connect_timeout: Duration::from_secs(5),
            handshake_retry: Duration::from_millis(250),
            max_handshake_packets: 32,
            peer_idle: Duration::from_secs(5),
            send: SendConfig::default(),
            crypto: None,
        }
    }
}

/// Why a [`Caller`] could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// The stream ID is longer than 512 bytes or contains a NUL.
    StreamId,
    /// A time, packet, payload, bandwidth or byte limit is out of range.
    Limits,
    /// The caller socket ID is zero.
    SocketId,
    /// The peer address has port 0, or is unspecified or multicast.
    Peer,
    /// The crypto configuration is invalid, or secure randomness was
    /// unavailable.
    Crypto,
}
impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::StreamId => "stream ID must contain at most 512 UTF-8 bytes and no NUL",
            Self::Limits => "invalid caller time, packet, payload, bandwidth or byte limit",
            Self::SocketId => "caller socket ID must be nonzero",
            Self::Crypto => "invalid crypto configuration or unavailable secure randomness",
            Self::Peer => "peer must have a nonzero port and a specified unicast address",
        })
    }
}
impl std::error::Error for ConfigError {}

/// Why a [`Caller`] session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerCloseReason {
    /// [`Caller::stop`] was called.
    Cancelled,
    /// The handshake did not complete within `connect_timeout`.
    ConnectTimeout,
    /// `max_handshake_packets` were sent without completing the handshake.
    HandshakeLimit,
    /// No valid packet from the listener for `peer_idle`.
    PeerIdle,
    /// The listener refused the handshake; the code is its rejection reason
    /// (see [`RejectCode`](crate::listener::RejectCode)).
    Rejected(u32),
    /// The listener's handshake is not one this live caller can use: wrong
    /// version, missing or unsupported extensions, or incompatible flags or
    /// sizes.
    UnsupportedPeer,
    /// Encryption failed: the listener's key material response did not match,
    /// or a key could not be used or rotated safely.
    Crypto,
    /// Queued media reached `send.max_age_us` before it was acknowledged.
    MediaExpired,
    /// A packet was retransmitted `send.max_retransmits` times.
    RetryLimit,
    /// The listener sent SHUTDOWN.
    PeerClosed,
    /// The listener sent a peer error; the code is the one it reported.
    PeerError(u32),
}

/// What happened to a [`Caller`] session; see [`Caller::poll_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerEvent {
    /// Handshake only; does not prove application acceptance or playback.
    Connected {
        /// The socket ID the listener gave the connection.
        peer_socket: u32,
        /// The negotiated TSBPD latency: the larger of ours and the
        /// listener's receiver delay, in whole milliseconds.
        latency: Duration,
        /// Largest message [`Caller::enqueue`] accepts, in bytes
        /// (`send.max_payload`).
        max_payload: usize,
    },
    /// The session ended.
    Closed {
        /// Why it ended.
        reason: CallerCloseReason,
    },
}

/// Counters for a [`Caller`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallerStats {
    /// Handshake packets sent, repeats included.
    pub handshake_packets: u64,
    /// Datagrams dropped for being over 1500 bytes or not a well-formed
    /// control packet.
    pub malformed: u64,
    /// Datagrams ignored as not meant for this session: wrong source address,
    /// wrong destination socket, or unexpected in the current state.
    pub stray: u64,
    /// The sender's counters; all zero until connected, and kept after
    /// closing.
    pub send: SendStats,
}

enum State {
    Induction,
    Conclusion { cookie: u32, socket_id: u32 },
    Open(Box<Sender>),
    Closed,
}

/// A live SRT caller: connects to a listener, then sends queued messages
/// paced and bounded. Sans-IO; see the module docs.
pub struct Caller {
    config: CallerConfig,
    peer: SocketAddr,
    socket_id: u32,
    initial: Seq,
    epoch: Instant,
    latest: i64,
    state: State,
    next_handshake: i64,
    events: VecDeque<CallerEvent>,
    stats: CallerStats,
    shutdown: Option<u32>,
    crypto: Option<Encryptor>,
}

impl Caller {
    /// A caller for `peer` with a random socket ID and initial sequence
    /// number; `now` starts its clock. Fails if `config` or `peer` is not
    /// valid.
    pub fn new(config: CallerConfig, peer: SocketAddr, now: Instant) -> Result<Self, ConfigError> {
        let random = RandomState::new();
        let socket_id = (random.hash_one(("sapflow caller", peer)) as u32 & 0x3fff_ffff).max(1);
        let initial = Seq::new(random.hash_one(("sapflow sequence", peer)) as u32);
        Self::with_ids(config, peer, socket_id, initial, now)
    }

    /// Deterministic construction for tests. Production adapters should use
    /// `new`, or provide fresh independently random socket/sequence IDs.
    pub fn with_ids(
        config: CallerConfig,
        peer: SocketAddr,
        socket_id: u32,
        initial: Seq,
        now: Instant,
    ) -> Result<Self, ConfigError> {
        if config.stream_id.len() > MAX_STREAM_ID || config.stream_id.contains('\0') {
            return Err(ConfigError::StreamId);
        }
        if socket_id == 0 {
            return Err(ConfigError::SocketId);
        }
        if peer.port() == 0 || peer.ip().is_unspecified() || peer.ip().is_multicast() {
            return Err(ConfigError::Peer);
        }
        if !config.send.valid()
            || config.latency > Duration::from_millis(u64::from(u16::MAX))
            || config.latency < Duration::from_millis(1)
            || !(Duration::from_millis(10)..=Duration::from_secs(5))
                .contains(&config.handshake_retry)
            || config.connect_timeout < config.handshake_retry
            || config.connect_timeout > Duration::from_secs(60)
            || !(1..=1024).contains(&config.max_handshake_packets)
            || !(Duration::from_millis(10)..=Duration::from_secs(60)).contains(&config.peer_idle)
        {
            return Err(ConfigError::Limits);
        }
        let crypto = config
            .crypto
            .as_ref()
            .map(Encryptor::new)
            .transpose()
            .map_err(|_| ConfigError::Crypto)?;
        Ok(Self {
            crypto,
            config,
            peer,
            socket_id,
            initial,
            epoch: now,
            latest: 0,
            state: State::Induction,
            next_handshake: 0,
            events: VecDeque::new(),
            stats: CallerStats::default(),
            shutdown: None,
        })
    }

    fn micros(&mut self, now: Instant) -> i64 {
        self.latest = self.latest.max(
            i64::try_from(now.saturating_duration_since(self.epoch).as_micros())
                .unwrap_or(i64::MAX),
        );
        self.latest
    }

    /// Whether the handshake has completed and the session is still open.
    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Open(_))
    }
    /// Whether the session has ended, for any reason.
    pub fn is_closed(&self) -> bool {
        matches!(self.state, State::Closed)
    }
    /// The counters so far, with the sender's once connected.
    pub fn stats(&self) -> CallerStats {
        let mut stats = self.stats;
        if let State::Open(sender) = &self.state {
            stats.send = sender.stats();
        }
        stats
    }
    /// The next [`CallerEvent`], if any.
    pub fn poll_event(&mut self) -> Option<CallerEvent> {
        self.events.pop_front()
    }

    /// Transfers the payload's allocation into the bounded send window.
    /// QueueFull is backpressure; the rejected payload is discarded.
    pub fn enqueue(&mut self, now: Instant, payload: Vec<u8>) -> Result<(), SendError> {
        self.try_enqueue(now, payload).map_err(|(error, _)| error)
    }

    /// Transfers ownership on success; returns the identical Vec allocation
    /// on every rejection, including QueueFull. No payload clone or copy.
    pub fn try_enqueue(
        &mut self,
        now: Instant,
        payload: Vec<u8>,
    ) -> Result<(), (SendError, Vec<u8>)> {
        let t = self.micros(now);
        self.tick(t);
        if let State::Open(sender) = &mut self.state {
            sender.try_enqueue(t, payload)
        } else {
            Err((SendError::NotConnected, payload))
        }
    }

    /// Takes a datagram that arrived from `peer` at `now`. One that is not a
    /// well-formed control packet for this session from the listener is
    /// counted in [`CallerStats`] and ignored.
    pub fn handle_datagram(&mut self, now: Instant, peer: SocketAddr, bytes: &[u8]) {
        let t = self.micros(now);
        self.tick(t);
        if self.is_closed() {
            return;
        }
        if peer != self.peer {
            self.stats.stray += 1;
            return;
        }
        if bytes.len() > 1500 || !control_shape(bytes) {
            self.stats.malformed += 1;
            return;
        }
        let Ok(Packet::Control(packet)) = Packet::parse(bytes) else {
            self.stats.malformed += 1;
            return;
        };
        if packet.dest != self.socket_id {
            self.stats.stray += 1;
            return;
        }
        match packet.body {
            Control::Handshake(h) => self.handshake(t, h),
            control => {
                if let State::Open(sender) = &mut self.state {
                    sender.on_control(t, &control);
                } else {
                    self.stats.stray += 1;
                }
                self.tick(t);
            }
        }
    }

    /// Does what is due at `now`: connect and idle timeouts, the media age
    /// limit and retransmission timers.
    pub fn handle_timeout(&mut self, now: Instant) {
        let t = self.micros(now);
        self.tick(t);
    }

    /// One datagram at most. Repeated calls at the same time cannot burst
    /// data; control responses are coalesced to one ACKACK slot.
    pub fn poll_transmit(&mut self, now: Instant) -> Option<(SocketAddr, Vec<u8>)> {
        let t = self.micros(now);
        self.tick(t);
        if let Some(dest) = self.shutdown.take() {
            let mut bytes = Vec::with_capacity(16);
            ControlPacket::new(t as u32, dest, Control::Shutdown).encode(&mut bytes);
            return Some((self.peer, bytes));
        }
        let bytes = match &mut self.state {
            State::Open(sender) => sender.poll_transmit(t),
            State::Induction | State::Conclusion { .. } if t >= self.next_handshake => {
                if self.stats.handshake_packets >= u64::from(self.config.max_handshake_packets) {
                    self.close(CallerCloseReason::HandshakeLimit, false);
                    return None;
                }
                self.stats.handshake_packets += 1;
                self.next_handshake = t.saturating_add(duration_us(self.config.handshake_retry));
                Some(self.handshake_packet(t))
            }
            _ => None,
        };
        self.tick(t);
        bytes.map(|bytes| (self.peer, bytes))
    }

    /// When to call [`handle_timeout`](Self::handle_timeout) next; `None`
    /// once the session has closed.
    pub fn poll_timeout(&self) -> Option<Instant> {
        let t = match &self.state {
            State::Induction | State::Conclusion { .. } => self
                .next_handshake
                .min(duration_us(self.config.connect_timeout)),
            State::Open(sender) => sender.next_timer().min(
                sender
                    .last_receive()
                    .saturating_add(duration_us(self.config.peer_idle)),
            ),
            State::Closed => return None,
        };
        self.epoch
            .checked_add(Duration::from_micros(t.max(0) as u64))
    }

    /// Idempotent cancellation releases payloads/timers immediately. Poll
    /// once afterwards to send the optional best-effort shutdown datagram.
    pub fn stop(&mut self, now: Instant) {
        self.micros(now);
        self.close(CallerCloseReason::Cancelled, true);
    }

    fn tick(&mut self, t: i64) {
        let reason = match &mut self.state {
            State::Induction | State::Conclusion { .. }
                if t >= duration_us(self.config.connect_timeout) =>
            {
                Some(CallerCloseReason::ConnectTimeout)
            }
            State::Open(sender) => {
                sender.on_timer(t);
                sender
                    .failure()
                    .map(|failure| match failure {
                        Failure::Crypto => CallerCloseReason::Crypto,
                        Failure::MediaExpired => CallerCloseReason::MediaExpired,
                        Failure::RetryLimit => CallerCloseReason::RetryLimit,
                        Failure::PeerClosed => CallerCloseReason::PeerClosed,
                        Failure::PeerError(code) => CallerCloseReason::PeerError(code),
                    })
                    .or_else(|| {
                        (t.saturating_sub(sender.last_receive())
                            >= duration_us(self.config.peer_idle))
                        .then_some(CallerCloseReason::PeerIdle)
                    })
            }
            _ => None,
        };
        if let Some(reason) = reason {
            self.close(
                reason,
                !matches!(
                    reason,
                    CallerCloseReason::PeerClosed | CallerCloseReason::PeerError(_)
                ),
            );
        }
    }

    fn close(&mut self, reason: CallerCloseReason, notify: bool) {
        if self.is_closed() {
            return;
        }
        if let State::Open(sender) = &mut self.state {
            if notify {
                self.shutdown = Some(sender.peer_socket());
            }
            sender.clear();
            self.stats.send = sender.stats();
        }
        self.state = State::Closed;
        self.crypto = None;
        self.config.crypto = None;
        self.events.push_back(CallerEvent::Closed { reason });
    }

    fn handshake(&mut self, t: i64, h: Handshake) {
        if matches!(self.state, State::Open(_)) {
            self.stats.stray += 1;
            return;
        }
        if h.initial_seq != self.initial.get() {
            self.stats.stray += 1;
            return;
        }
        if let HandshakeKind::Rejection(code) = h.kind {
            self.close(CallerCloseReason::Rejected(code), false);
            return;
        }
        if h.version != 5 {
            self.close(CallerCloseReason::UnsupportedPeer, false);
            return;
        }
        match self.state {
            State::Induction if h.kind == HandshakeKind::Induction => {
                if h.extension != SRT_MAGIC
                    || (h.encryption != 0
                        && self
                            .config
                            .crypto
                            .as_ref()
                            .is_none_or(|c| c.key_size.field() != h.encryption))
                    || !h.extensions.is_empty()
                {
                    self.close(CallerCloseReason::UnsupportedPeer, false);
                    return;
                }
                self.state = State::Conclusion {
                    cookie: h.cookie,
                    // libsrt 1.5.4 echoes the caller ID in induction (observed
                    // on the wire), not an addressable listener connection.
                    // Its demultiplexer accepts the conclusion at socket 0.
                    socket_id: if h.socket_id == self.socket_id {
                        0
                    } else {
                        h.socket_id
                    },
                };
                self.next_handshake = t;
            }
            State::Conclusion { .. } if h.kind == HandshakeKind::Conclusion => {
                let responses: Vec<_> = h
                    .extensions
                    .iter()
                    .filter_map(|e| match e {
                        Extension::HsRsp(s) => Some(*s),
                        _ => None,
                    })
                    .collect();
                let Some(response) = responses.first().copied() else {
                    self.close(CallerCloseReason::UnsupportedPeer, false);
                    return;
                };
                let unsupported = h.extensions.iter().any(|e| match e {
                    Extension::KmReq(_)
                    | Extension::Filter(_)
                    | Extension::Group(_)
                    | Extension::HsReq(_) => true,
                    Extension::Congestion(c) => c.as_slice() != b"live",
                    _ => false,
                });
                if responses.len() != 1
                    || unsupported
                    || h.extension & ext_flags::HSREQ == 0
                    || h.socket_id == 0
                    || h.mtu < 44 + self.config.send.max_payload as u32
                    || h.mtu > 1500
                    || h.flow_window == 0
                    || h.flow_window >= 0x4000_0000
                    || response.version < 0x01_03_00
                    || response.flags & srt_flags::STREAM != 0
                    || response.flags & srt_flags::TSBPDRCV == 0
                    || response.flags & srt_flags::REXMITFLG == 0
                {
                    self.close(CallerCloseReason::UnsupportedPeer, false);
                    return;
                }
                let kms: Vec<_> = h
                    .extensions
                    .iter()
                    .filter_map(|e| match e {
                        Extension::KmRsp(b) => Some(b.as_slice()),
                        _ => None,
                    })
                    .collect();
                let crypto_ok = match &self.crypto {
                    Some(crypto) => {
                        kms.len() == 1
                            && h.extension & ext_flags::KMREQ != 0
                            && self.config.crypto.as_ref().is_some_and(|c| {
                                h.encryption == 0 || c.key_size.field() == h.encryption
                            })
                            && crypto.confirm_initial(kms.first().copied().unwrap_or_default())
                    }
                    None => {
                        kms.is_empty() && h.encryption == 0 && h.extension & ext_flags::KMREQ == 0
                    }
                };
                if !crypto_ok {
                    self.close(
                        if self.crypto.is_some() {
                            CallerCloseReason::Crypto
                        } else {
                            CallerCloseReason::UnsupportedPeer
                        },
                        false,
                    );
                    return;
                }
                let requested_ms = self.config.latency.as_micros().div_ceil(1000) as u64;
                let latency =
                    Duration::from_millis(requested_ms.max(u64::from(response.receiver_delay)));
                let mut sender = Sender::new(
                    self.config.send.clone(),
                    t,
                    h.socket_id,
                    self.initial,
                    (h.flow_window as usize).min(self.config.send.max_packets),
                );
                sender.set_crypto(self.crypto.take());
                let max_payload = sender.max_payload();
                self.state = State::Open(Box::new(sender));
                self.events.push_back(CallerEvent::Connected {
                    peer_socket: h.socket_id,
                    latency,
                    max_payload,
                });
            }
            _ => self.stats.stray += 1,
        }
    }

    fn handshake_packet(&self, t: i64) -> Vec<u8> {
        let (cookie, dest, conclusion) = match self.state {
            State::Conclusion { cookie, socket_id } => (cookie, socket_id, true),
            _ => (0, 0, false),
        };
        let mut extensions = Vec::new();
        if conclusion {
            extensions.push(Extension::HsReq(SrtHandshake {
                version: SRT_VERSION,
                flags: FLAGS,
                receiver_delay: 0,
                sender_delay: self.config.latency.as_micros().div_ceil(1000) as u16,
            }));
            if let Some(crypto) = &self.crypto
                && let Ok(km) = crypto.initial()
            {
                extensions.push(Extension::KmReq(km));
            }
            if !self.config.stream_id.is_empty() {
                extensions.push(Extension::StreamId(
                    self.config.stream_id.as_bytes().to_vec(),
                ));
            }
        }
        let h = Handshake {
            version: if conclusion { 5 } else { 4 },
            encryption: if conclusion {
                self.config
                    .crypto
                    .as_ref()
                    .map_or(0, |c| c.key_size.field())
            } else {
                0
            },
            extension: if conclusion {
                ext_flags::HSREQ
                    | if self.crypto.is_some() {
                        ext_flags::KMREQ
                    } else {
                        0
                    }
                    | if self.config.stream_id.is_empty() {
                        0
                    } else {
                        ext_flags::CONFIG
                    }
            } else {
                2
            },
            initial_seq: self.initial.get(),
            mtu: 1500,
            flow_window: self.config.send.max_packets as u32,
            kind: if conclusion {
                HandshakeKind::Conclusion
            } else {
                HandshakeKind::Induction
            },
            socket_id: self.socket_id,
            cookie,
            peer_ip: encode_ip(self.peer),
            extensions,
        };
        let mut bytes = Vec::with_capacity(84 + self.config.stream_id.len());
        ControlPacket::new(t as u32, dest, Control::Handshake(h)).encode(&mut bytes);
        bytes
    }
}

fn duration_us(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

fn encode_ip(peer: SocketAddr) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    match peer.ip() {
        std::net::IpAddr::V4(ip) => {
            for (dst, src) in bytes.iter_mut().zip(ip.octets().iter().rev()) {
                *dst = *src;
            }
        }
        std::net::IpAddr::V6(ip) => {
            for (dst, src) in bytes.chunks_mut(4).zip(ip.octets().chunks(4)) {
                for (d, s) in dst.iter_mut().zip(src.iter().rev()) {
                    *d = *s;
                }
            }
        }
    }
    bytes
}

/// Reject truncated control bodies before the shared permissive packet
/// parser. Unknown control types cannot allocate beyond the datagram cap.
fn control_shape(bytes: &[u8]) -> bool {
    let Some(header) = bytes.get(..16) else {
        return false;
    };
    let mut words = header.chunks_exact(4);
    let Some(first) = words.next() else {
        return false;
    };
    let Ok(first) = <[u8; 4]>::try_from(first) else {
        return false;
    };
    let first = u32::from_be_bytes(first);
    if first & 0x8000_0000 == 0 {
        return false;
    }
    let kind = (first >> 16) & 0x7fff;
    if kind <= 8 && first & 0xffff != 0 {
        return false;
    }
    let size = bytes.len() - 16;
    match (first >> 16) & 0x7fff {
        0 => size >= 48 && size.is_multiple_of(4),
        2 => matches!(size, 4 | 16 | 24 | 28) && bytes.get(16).is_some_and(|b| b & 0x80 == 0),
        3 => {
            size > 0
                && size.is_multiple_of(4)
                && valid_loss_words(bytes.get(16..).unwrap_or_default())
        }
        1 | 4 | 5 | 6 | 8 => matches!(size, 0 | 4),
        7 => size == 8,
        _ => true,
    }
}

// A range endpoint is an ordinary 31-bit sequence, never another range
// marker. Check before Packet::parse masks the sequence-number flag bit.
fn valid_loss_words(body: &[u8]) -> bool {
    let mut endpoint = false;
    for word in body.chunks_exact(4) {
        let Some(first) = word.first() else {
            return false;
        };
        let marker = first & 0x80 != 0;
        if endpoint && marker {
            return false;
        }
        endpoint = marker && !endpoint;
    }
    !endpoint
}
