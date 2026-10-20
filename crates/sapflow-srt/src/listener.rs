//! The listener: every datagram to the SRT port comes here. It answers
//! handshakes (section 4.3.1), asks the application about each stream id,
//! and passes later packets to their connection's [`Receiver`].
//!
//! Sans-IO: give it datagrams with their source address and the time, and
//! collect the datagrams to send, the events, and when to call
//! [`Listener::handle_timeout`] next. With no connection there is no timer.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, RandomState};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::packet::{
    Control, ControlPacket, Extension, Handshake, HandshakeKind, Packet, SRT_MAGIC, SrtHandshake,
    ext_flags, srt_flags,
};
use crate::receiver::{CloseReason, Receiver, Settings, Stats};
use crate::seq::Seq;

/// The version this implementation reports in its handshake response:
/// the protocol level of draft-sharabayko-srt-01 that libsrt calls 1.5.0.
pub const SRT_VERSION: u32 = 0x01_05_00;

/// What we tell a caller we do: TSBPD, too-late packet drop, periodic NAK
/// reports, and the two legacy flags every peer must set. Not the stream
/// (file) mode and not packet filters.
pub const OUR_FLAGS: u32 = srt_flags::TSBPDSND
    | srt_flags::TSBPDRCV
    | srt_flags::CRYPT
    | srt_flags::TLPKTDROP
    | srt_flags::PERIODICNAK
    | srt_flags::REXMITFLG;

/// A SYN cookie is valid for the minute it was made in and the next.
const COOKIE_PERIOD: i64 = 60_000_000;
/// The largest packet we take.
const MTU_MAX: u32 = 1500;
/// IP and UDP headers, and the SRT header.
const OVERHEAD: u32 = 28 + 16;

#[derive(Debug, Clone)]
pub struct Config {
    /// Our receive latency; a caller asking for more gets more.
    pub latency: Duration,
    /// Connections, open or waiting for the application, at most.
    pub max_connections: usize,
    /// Handshake packets answered per second, at most (bursts of twice).
    pub handshakes_per_second: u32,
    /// Receive buffer per connection, in packets.
    pub buffer_packets: usize,
    /// A connection that sends nothing for this long is closed.
    pub peer_idle: Duration,
    /// The application has this long to accept or reject a caller.
    pub accept_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            latency: Duration::from_millis(120),
            max_connections: 64,
            handshakes_per_second: 100,
            buffer_packets: 8192,
            peer_idle: Duration::from_secs(5),
            accept_timeout: Duration::from_secs(5),
        }
    }
}

/// A connection, named by the socket id we gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u32);

/// Why a handshake is refused: the code in the Handshake Type field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectCode(pub u32);

impl RejectCode {
    pub const UNKNOWN: Self = Self(1000);
    pub const SYSTEM: Self = Self(1001);
    pub const PEER: Self = Self(1002);
    pub const RESOURCE: Self = Self(1003);
    pub const ROGUE: Self = Self(1004);
    pub const BACKLOG: Self = Self(1005);
    pub const CLOSE: Self = Self(1007);
    pub const VERSION: Self = Self(1008);
    pub const UNSECURE: Self = Self(1011);
    pub const MESSAGE_API: Self = Self(1012);
    pub const CONGESTION: Self = Self(1013);
    pub const FILTER: Self = Self(1014);
    pub const GROUP: Self = Self(1015);

    /// An application refusal carrying an HTTP status (2000 + status),
    /// which libsrt reports as `PREDEFINED:<status>`.
    pub const fn http(status: u16) -> Self {
        Self(2000 + status as u32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A caller completed the handshake. Answer with
    /// [`Listener::accept`] or [`Listener::reject`].
    Connect {
        id: ConnId,
        peer: SocketAddr,
        stream_id: Vec<u8>,
    },
    /// The next payload, in order, at its delivery time.
    Data { id: ConnId, payload: Vec<u8> },
    /// The connection ended from the other side, or a caller was not
    /// answered in time (`reason` is `None` then).
    Closed {
        id: ConnId,
        reason: Option<CloseReason>,
        stats: Stats,
    },
}

/// Datagrams the listener threw away, by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListenerStats {
    pub malformed: u64,
    /// For no connection, or from the wrong address.
    pub stray: u64,
    pub bad_cookie: u64,
    pub rate_limited: u64,
    pub rejected: u64,
}

struct Pending {
    hs: Handshake,
    hs_timestamp: u32,
    latency: i64,
    since: i64,
}

enum State {
    Pending(Box<Pending>),
    Open {
        rx: Box<Receiver>,
        /// Our conclusion response, sent again if the caller repeats its
        /// conclusion.
        response: Vec<u8>,
    },
}

struct Conn {
    peer: SocketAddr,
    caller_socket: u32,
    state: State,
    next_timer: i64,
}

pub struct Listener<S = RandomState> {
    config: Config,
    epoch: Instant,
    /// Keyed per process: the SYN cookies and our socket ids come from it.
    key: S,
    ids_made: u64,
    conns: HashMap<u32, Conn>,
    by_caller: HashMap<(SocketAddr, u32), u32>,
    transmits: VecDeque<(SocketAddr, Vec<u8>)>,
    events: VecDeque<Event>,
    /// Handshake allowance, in millionths of a handshake.
    tokens: u64,
    tokens_at: i64,
    stats: ListenerStats,
}

impl Listener<RandomState> {
    /// A listener whose cookies and socket ids are keyed by a random key
    /// made for this process.
    pub fn new(config: Config, now: Instant) -> Self {
        Self::with_key(config, now, RandomState::new())
    }
}

impl<S: BuildHasher> Listener<S> {
    /// A listener keyed by `key`: give it a random one (tests and fuzzing
    /// use a fixed one, for repeatable runs).
    pub fn with_key(config: Config, now: Instant, key: S) -> Self {
        let burst = u64::from(config.handshakes_per_second) * 2 * 1_000_000;
        Self {
            config,
            epoch: now,
            key,
            ids_made: 0,
            conns: HashMap::new(),
            by_caller: HashMap::new(),
            transmits: VecDeque::new(),
            events: VecDeque::new(),
            tokens: burst,
            tokens_at: 0,
            stats: ListenerStats::default(),
        }
    }

    fn micros(&self, now: Instant) -> i64 {
        i64::try_from(now.saturating_duration_since(self.epoch).as_micros()).unwrap_or(i64::MAX)
    }

    pub fn stats(&self) -> ListenerStats {
        self.stats
    }

    /// Connections open or waiting for an answer.
    pub fn connections(&self) -> usize {
        self.conns.len()
    }

    pub fn connection_stats(&self, id: ConnId) -> Option<Stats> {
        match &self.conns.get(&id.0)?.state {
            State::Open { rx, .. } => Some(rx.stats()),
            State::Pending(_) => None,
        }
    }

    /// The negotiated latency of an open connection.
    pub fn latency(&self, id: ConnId) -> Option<Duration> {
        match &self.conns.get(&id.0)?.state {
            State::Open { rx, .. } => Some(Duration::from_micros(rx.latency().max(0) as u64)),
            State::Pending(_) => None,
        }
    }

    pub fn poll_transmit(&mut self) -> Option<(SocketAddr, Vec<u8>)> {
        self.transmits.pop_front()
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// When to call [`handle_timeout`](Self::handle_timeout); `None` when
    /// there are no connections.
    pub fn poll_timeout(&self) -> Option<Instant> {
        let t = self.conns.values().map(|c| c.next_timer).min()?;
        Some(self.epoch + Duration::from_micros(t.max(0) as u64))
    }

    pub fn handle_datagram(&mut self, now: Instant, peer: SocketAddr, datagram: &[u8]) {
        let t = self.micros(now);
        let packet = match Packet::parse(datagram) {
            Ok(p) => p,
            Err(_) => {
                self.stats.malformed += 1;
                return;
            }
        };
        match packet {
            Packet::Data(d) => {
                let Some(c) = self.conns.get_mut(&d.dest).filter(|c| c.peer == peer) else {
                    self.stats.stray += 1;
                    return;
                };
                if let State::Open { rx, .. } = &mut c.state {
                    rx.on_data(t, &d);
                    let id = d.dest;
                    self.collect(id);
                } else {
                    self.stats.stray += 1;
                }
            }
            Packet::Control(ControlPacket {
                timestamp,
                body: Control::Handshake(h),
                ..
            }) => self.handshake(t, peer, timestamp, h),
            Packet::Control(c) => {
                let Some(conn) = self.conns.get_mut(&c.dest).filter(|x| x.peer == peer) else {
                    self.stats.stray += 1;
                    return;
                };
                if let State::Open { rx, .. } = &mut conn.state {
                    rx.on_control(t, &c.body);
                    self.collect(c.dest);
                }
            }
        }
    }

    pub fn handle_timeout(&mut self, now: Instant) {
        let t = self.micros(now);
        let due: Vec<u32> = self
            .conns
            .iter()
            .filter(|(_, c)| c.next_timer <= t)
            .map(|(id, _)| *id)
            .collect();
        let accept_timeout = duration_micros(self.config.accept_timeout);
        for id in due {
            let Some(c) = self.conns.get_mut(&id) else {
                continue;
            };
            match &mut c.state {
                State::Pending(p) => {
                    if t - p.since >= accept_timeout {
                        self.remove(id);
                        self.events.push_back(Event::Closed {
                            id: ConnId(id),
                            reason: None,
                            stats: Stats::default(),
                        });
                    }
                }
                State::Open { rx, .. } => {
                    rx.on_timer(t);
                    self.collect(id);
                }
            }
        }
    }

    /// Accepts a caller announced by [`Event::Connect`]: the handshake
    /// response goes out and data is delivered from now on.
    pub fn accept(&mut self, id: ConnId, now: Instant) -> bool {
        let t = self.micros(now);
        let max_payload = MTU_MAX - OVERHEAD;
        let buffer = self.config.buffer_packets.max(32);
        let peer_idle = duration_micros(self.config.peer_idle);
        let Some(c) = self.conns.get_mut(&id.0) else {
            return false;
        };
        let State::Pending(p) = &c.state else {
            return false;
        };
        let p = &**p;
        let req = p.hs.srt_request().unwrap_or(SrtHandshake {
            version: 0,
            flags: 0,
            receiver_delay: 0,
            sender_delay: 0,
        });
        let ms = u16::try_from(p.latency / 1000).unwrap_or(u16::MAX);
        let response = Handshake {
            version: 5,
            encryption: 0,
            extension: ext_flags::HSREQ,
            initial_seq: p.hs.initial_seq,
            mtu: p.hs.mtu.min(MTU_MAX),
            flow_window: p
                .hs
                .flow_window
                .min(u32::try_from(buffer).unwrap_or(u32::MAX)),
            kind: HandshakeKind::Conclusion,
            socket_id: id.0,
            cookie: p.hs.cookie,
            peer_ip: encode_ip(c.peer),
            extensions: vec![Extension::HsRsp(SrtHandshake {
                version: SRT_VERSION,
                flags: OUR_FLAGS,
                receiver_delay: ms,
                sender_delay: ms.max(req.receiver_delay),
            })],
        };
        let mut bytes = Vec::with_capacity(80);
        ControlPacket::new(0, c.caller_socket, Control::Handshake(response)).encode(&mut bytes);
        let rx = Receiver::new(
            Settings {
                latency: p.latency,
                buffer,
                peer_idle,
                max_payload: max_payload as usize,
            },
            t,
            c.caller_socket,
            Seq::new(p.hs.initial_seq),
            p.hs_timestamp,
        );
        self.transmits.push_back((c.peer, bytes.clone()));
        c.state = State::Open {
            rx: Box::new(rx),
            response: bytes,
        };
        self.collect(id.0);
        true
    }

    /// Refuses a caller announced by [`Event::Connect`].
    pub fn reject(&mut self, id: ConnId, code: RejectCode) -> bool {
        let Some(c) = self.conns.get(&id.0) else {
            return false;
        };
        let State::Pending(p) = &c.state else {
            return false;
        };
        let (peer, hs) = (c.peer, p.hs.clone());
        self.send_rejection(peer, &hs, code);
        self.remove(id.0);
        true
    }

    /// Ends an open connection from our side (a SHUTDOWN goes to the
    /// caller), or forgets one waiting for an answer.
    pub fn close(&mut self, id: ConnId, now: Instant) {
        let t = self.micros(now);
        if let Some(c) = self.conns.get_mut(&id.0)
            && let State::Open { rx, .. } = &mut c.state
        {
            rx.shutdown(t);
            while let Some(b) = rx.take_send() {
                self.transmits.push_back((c.peer, b));
            }
        }
        self.remove(id.0);
    }

    fn remove(&mut self, id: u32) {
        if let Some(c) = self.conns.remove(&id) {
            self.by_caller.remove(&(c.peer, c.caller_socket));
        }
    }

    /// Moves a connection's output into the queues and reschedules it.
    fn collect(&mut self, id: u32) {
        let Some(c) = self.conns.get_mut(&id) else {
            return;
        };
        let State::Open { rx, .. } = &mut c.state else {
            return;
        };
        while let Some(b) = rx.take_send() {
            self.transmits.push_back((c.peer, b));
        }
        while let Some(payload) = rx.take_delivery() {
            self.events.push_back(Event::Data {
                id: ConnId(id),
                payload,
            });
        }
        if let Some(reason) = rx.closed() {
            let stats = rx.stats();
            self.remove(id);
            self.events.push_back(Event::Closed {
                id: ConnId(id),
                reason: Some(reason),
                stats,
            });
            return;
        }
        c.next_timer = rx.next_timer();
    }

    fn take_token(&mut self, t: i64) -> bool {
        let rate = u64::from(self.config.handshakes_per_second);
        let burst = rate * 2 * 1_000_000;
        let elapsed = (t - self.tokens_at).max(0) as u64;
        self.tokens = self
            .tokens
            .saturating_add(elapsed.saturating_mul(rate))
            .min(burst);
        self.tokens_at = t;
        if self.tokens >= 1_000_000 {
            self.tokens -= 1_000_000;
            true
        } else {
            false
        }
    }

    fn cookie(&self, peer: SocketAddr, period: i64) -> u32 {
        let h = self.key.hash_one((b"srt cookie", peer, period));
        (h ^ (h >> 32)) as u32
    }

    fn new_socket_id(&mut self) -> u32 {
        loop {
            self.ids_made += 1;
            let h = self.key.hash_one((b"srt socket", self.ids_made));
            // 30 bits, never 0 (0 addresses the listener).
            let id = ((h ^ (h >> 32)) as u32) & 0x3FFF_FFFF;
            if id != 0 && !self.conns.contains_key(&id) {
                return id;
            }
        }
    }

    fn handshake(&mut self, t: i64, peer: SocketAddr, timestamp: u32, h: Handshake) {
        match h.kind {
            HandshakeKind::Induction => {
                if !self.take_token(t) {
                    self.stats.rate_limited += 1;
                    return;
                }
                self.induction(t, peer, &h);
            }
            HandshakeKind::Conclusion => {
                // A repeat (our response was lost): the same answer again.
                if let Some(id) = self.by_caller.get(&(peer, h.socket_id)) {
                    if let Some(Conn {
                        state: State::Open { response, .. },
                        ..
                    }) = self.conns.get(id)
                    {
                        self.transmits.push_back((peer, response.clone()));
                    }
                    return;
                }
                if !self.take_token(t) {
                    self.stats.rate_limited += 1;
                    return;
                }
                let period = t / COOKIE_PERIOD;
                if h.cookie != self.cookie(peer, period)
                    && h.cookie != self.cookie(peer, period - 1)
                {
                    // Not from the address it claims, or stale: no answer.
                    self.stats.bad_cookie += 1;
                    return;
                }
                self.conclusion(t, peer, timestamp, h);
            }
            _ => self.stats.stray += 1,
        }
    }

    fn induction(&mut self, t: i64, peer: SocketAddr, h: &Handshake) {
        // Observed from libsrt's listener: version 5, the magic number, the
        // caller's own socket id, sequence number, MTU and window echoed.
        let reply = Handshake {
            version: 5,
            encryption: 0,
            extension: SRT_MAGIC,
            initial_seq: h.initial_seq,
            mtu: h.mtu,
            flow_window: h.flow_window,
            kind: HandshakeKind::Induction,
            socket_id: h.socket_id,
            cookie: self.cookie(peer, t / COOKIE_PERIOD),
            peer_ip: encode_ip(peer),
            extensions: Vec::new(),
        };
        let mut out = Vec::with_capacity(64);
        ControlPacket::new(t as u32, h.socket_id, Control::Handshake(reply)).encode(&mut out);
        self.transmits.push_back((peer, out));
    }

    fn conclusion(&mut self, t: i64, peer: SocketAddr, timestamp: u32, h: Handshake) {
        if let Err(code) = self.check(&h) {
            self.send_rejection(peer, &h, code);
            return;
        }
        if self.conns.len() >= self.config.max_connections {
            self.send_rejection(peer, &h, RejectCode::BACKLOG);
            return;
        }
        let ours = duration_micros(self.config.latency);
        let theirs = h
            .srt_request()
            .map_or(0, |r| i64::from(r.sender_delay) * 1000);
        let id = self.new_socket_id();
        let stream_id = h.stream_id().map(<[u8]>::to_vec).unwrap_or_default();
        self.by_caller.insert((peer, h.socket_id), id);
        self.conns.insert(
            id,
            Conn {
                peer,
                caller_socket: h.socket_id,
                state: State::Pending(Box::new(Pending {
                    hs: h,
                    hs_timestamp: timestamp,
                    latency: ours.max(theirs),
                    since: t,
                })),
                next_timer: t + duration_micros(self.config.accept_timeout),
            },
        );
        self.events.push_back(Event::Connect {
            id: ConnId(id),
            peer,
            stream_id,
        });
    }

    /// What this receiver cannot do, refused before the application sees
    /// the caller.
    fn check(&self, h: &Handshake) -> Result<(), RejectCode> {
        if h.version != 5 {
            return Err(RejectCode::VERSION);
        }
        let km = h
            .extensions
            .iter()
            .any(|e| matches!(e, Extension::KmReq(_)));
        if h.encryption != 0 || km || h.extension & ext_flags::KMREQ != 0 {
            // Encryption is not implemented yet (M6b).
            return Err(RejectCode::UNSECURE);
        }
        let req = h.srt_request().ok_or(RejectCode::ROGUE)?;
        if req.flags & srt_flags::STREAM != 0 {
            return Err(RejectCode::MESSAGE_API);
        }
        if req.flags & srt_flags::TSBPDSND == 0 {
            return Err(RejectCode::ROGUE);
        }
        for e in &h.extensions {
            match e {
                Extension::Group(_) => return Err(RejectCode::GROUP),
                Extension::Filter(_) => return Err(RejectCode::FILTER),
                Extension::Congestion(c) if c.as_slice() != b"live" => {
                    return Err(RejectCode::CONGESTION);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn send_rejection(&mut self, peer: SocketAddr, h: &Handshake, code: RejectCode) {
        self.stats.rejected += 1;
        let reply = Handshake {
            version: 5,
            encryption: 0,
            extension: 0,
            initial_seq: h.initial_seq,
            mtu: h.mtu,
            flow_window: h.flow_window,
            kind: HandshakeKind::Rejection(code.0),
            socket_id: 0,
            cookie: h.cookie,
            peer_ip: encode_ip(peer),
            extensions: Vec::new(),
        };
        let mut out = Vec::with_capacity(64);
        ControlPacket::new(0, h.socket_id, Control::Handshake(reply)).encode(&mut out);
        self.transmits.push_back((peer, out));
    }
}

fn duration_micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

/// The Peer IP Address field: four 32-bit little-endian words (observed:
/// libsrt writes 127.0.0.1 as `01 00 00 7f`, then zeros).
fn encode_ip(peer: SocketAddr) -> [u8; 16] {
    let mut out = [0u8; 16];
    let octets: Vec<u8> = match peer {
        SocketAddr::V4(a) => a.ip().octets().to_vec(),
        SocketAddr::V6(a) => a.ip().octets().to_vec(),
    };
    for (dst, src) in out.chunks_mut(4).zip(octets.chunks(4)) {
        for (d, s) in dst.iter_mut().zip(src.iter().rev()) {
            *d = *s;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{DataPacket, Position};
    use std::hash::BuildHasherDefault;
    use std::hash::DefaultHasher;

    type Fixed = BuildHasherDefault<DefaultHasher>;

    fn listener(t0: Instant) -> Listener<Fixed> {
        Listener::with_key(Config::default(), t0, Fixed::default())
    }

    fn peer() -> SocketAddr {
        "192.0.2.1:5000".parse().expect("addr")
    }

    fn induction(socket: u32) -> Vec<u8> {
        let h = Handshake {
            version: 4,
            encryption: 0,
            extension: 2,
            initial_seq: 1000,
            mtu: 1500,
            flow_window: 8192,
            kind: HandshakeKind::Induction,
            socket_id: socket,
            cookie: 0,
            peer_ip: [0; 16],
            extensions: vec![],
        };
        let mut out = Vec::new();
        ControlPacket::new(10, 0, Control::Handshake(h)).encode(&mut out);
        out
    }

    fn conclusion(socket: u32, cookie: u32, extra: Vec<Extension>) -> Vec<u8> {
        let mut extensions = vec![
            Extension::HsReq(SrtHandshake {
                version: 0x01_05_04,
                flags: 0xBF,
                receiver_delay: 2000,
                sender_delay: 2000,
            }),
            Extension::StreamId(b"publish:live/event-x:u:p".to_vec()),
        ];
        extensions.extend(extra);
        let h = Handshake {
            version: 5,
            encryption: 0,
            extension: ext_flags::HSREQ | ext_flags::CONFIG,
            initial_seq: 1000,
            mtu: 1500,
            flow_window: 8192,
            kind: HandshakeKind::Conclusion,
            socket_id: socket,
            cookie,
            peer_ip: [0; 16],
            extensions,
        };
        let mut out = Vec::new();
        ControlPacket::new(500, 0, Control::Handshake(h)).encode(&mut out);
        out
    }

    fn sent_handshakes<S: BuildHasher>(l: &mut Listener<S>) -> Vec<(u32, Handshake)> {
        std::iter::from_fn(|| l.poll_transmit())
            .filter_map(|(_, b)| match Packet::parse(&b) {
                Ok(Packet::Control(ControlPacket {
                    dest,
                    body: Control::Handshake(h),
                    ..
                })) => Some((dest, h)),
                _ => None,
            })
            .collect()
    }

    /// Induction then conclusion; returns the connection.
    fn connect(l: &mut Listener<Fixed>, t: Instant, socket: u32) -> ConnId {
        l.handle_datagram(t, peer(), &induction(socket));
        let hs = sent_handshakes(l);
        let [(dest, reply)] = hs.as_slice() else {
            panic!("{hs:?}");
        };
        assert_eq!(*dest, socket);
        assert_eq!(reply.version, 5);
        assert_eq!(reply.extension, SRT_MAGIC);
        assert_eq!(reply.kind, HandshakeKind::Induction);
        assert_eq!(reply.peer_ip[..4], [1, 2, 0, 192]);
        l.handle_datagram(t, peer(), &conclusion(socket, reply.cookie, vec![]));
        match l.poll_event() {
            Some(Event::Connect {
                id,
                peer: p,
                stream_id,
            }) => {
                assert_eq!(p, peer());
                assert_eq!(stream_id, b"publish:live/event-x:u:p");
                id
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn handshake_accept_and_data() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        assert_eq!(l.poll_timeout(), None, "no connections, no timer");
        let id = connect(&mut l, t0, 0x1234);
        assert!(sent_handshakes(&mut l).is_empty(), "nothing until accepted");
        assert!(l.accept(id, t0));
        let hs = sent_handshakes(&mut l);
        let [(dest, r)] = hs.as_slice() else {
            panic!("{hs:?}");
        };
        assert_eq!(*dest, 0x1234);
        assert_eq!(r.kind, HandshakeKind::Conclusion);
        assert_eq!(r.socket_id, id.0);
        assert_eq!(r.extension, ext_flags::HSREQ);
        let Some(Extension::HsRsp(rsp)) = r.extensions.first() else {
            panic!("{r:?}");
        };
        assert_eq!(rsp.flags, OUR_FLAGS);
        assert_eq!(rsp.receiver_delay, 2000, "the larger latency wins");
        assert_eq!(l.latency(id), Some(Duration::from_secs(2)));
        assert!(l.poll_timeout().is_some());

        // Data for the connection, then delivery after the latency.
        let mut b = Vec::new();
        DataPacket {
            seq: Seq::new(1000),
            position: Position::Only,
            in_order: false,
            key: 0,
            retransmitted: false,
            message: 1,
            timestamp: 600,
            dest: id.0,
            payload: b"ts",
        }
        .encode(&mut b);
        l.handle_datagram(t0, peer(), &b);
        // From another address it is ignored.
        l.handle_datagram(t0, "192.0.2.9:5000".parse().expect("addr"), &b);
        assert_eq!(l.stats().stray, 1);
        let mut t = t0;
        let mut got = Vec::new();
        while let Some(next) = l.poll_timeout() {
            if next > t0 + Duration::from_millis(2200) {
                break;
            }
            l.handle_timeout(next);
            while let Some(ev) = l.poll_event() {
                got.push(ev);
                t = next;
            }
        }
        assert_eq!(
            got,
            vec![Event::Data {
                id,
                payload: b"ts".to_vec()
            }]
        );
        // Due 2000 ms after its timestamp, which is 100 µs after the
        // conclusion's.
        assert_eq!(t - t0, Duration::from_micros(2_000_100));
    }

    #[test]
    fn a_repeated_conclusion_gets_the_same_response_not_a_second_connection() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        let id = connect(&mut l, t0, 7);
        // Repeated while the application decides: ignored.
        let cookie = l.cookie(peer(), 0);
        l.handle_datagram(t0, peer(), &conclusion(7, cookie, vec![]));
        assert_eq!(l.poll_event(), None);
        l.accept(id, t0);
        let first = sent_handshakes(&mut l);
        l.handle_datagram(t0, peer(), &conclusion(7, cookie, vec![]));
        assert_eq!(sent_handshakes(&mut l), first);
        assert_eq!(l.connections(), 1);
        assert_eq!(l.poll_event(), None);
    }

    #[test]
    fn a_wrong_cookie_gets_no_answer() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        l.handle_datagram(t0, peer(), &conclusion(7, 12345, vec![]));
        assert!(sent_handshakes(&mut l).is_empty());
        assert_eq!(l.poll_event(), None);
        assert_eq!(l.stats().bad_cookie, 1);
        // A cookie made for another address does not work either.
        let other: SocketAddr = "192.0.2.2:5000".parse().expect("addr");
        let c = l.cookie(other, 0);
        l.handle_datagram(t0, peer(), &conclusion(7, c, vec![]));
        assert_eq!(l.stats().bad_cookie, 2);
    }

    #[test]
    fn cookies_last_one_to_two_minutes_and_depend_on_the_key() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        let c = l.cookie(peer(), 0);
        let t = t0 + Duration::from_secs(90);
        l.handle_datagram(t, peer(), &conclusion(7, c, vec![]));
        assert!(matches!(l.poll_event(), Some(Event::Connect { .. })));
        let t = t0 + Duration::from_secs(121);
        l.handle_datagram(t, peer(), &conclusion(8, c, vec![]));
        assert_eq!(l.poll_event(), None);
        let other = Listener::new(Config::default(), t0);
        assert_ne!(other.cookie(peer(), 0), c, "a random key per process");
    }

    #[test]
    fn rejection_carries_the_code() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        let id = connect(&mut l, t0, 9);
        assert!(l.reject(id, RejectCode::http(401)));
        let hs = sent_handshakes(&mut l);
        assert!(
            matches!(hs.as_slice(), [(9, h)] if h.kind == HandshakeKind::Rejection(2401)),
            "{hs:?}"
        );
        assert_eq!(l.connections(), 0);
        assert_eq!(l.poll_timeout(), None);
    }

    #[test]
    fn what_we_cannot_do_is_refused_in_the_handshake() {
        let t0 = Instant::now();
        let cases = [
            (Extension::KmReq(vec![0; 16]), RejectCode::UNSECURE),
            (Extension::Group(vec![0; 8]), RejectCode::GROUP),
            (Extension::Filter(b"fec".to_vec()), RejectCode::FILTER),
            (
                Extension::Congestion(b"file".to_vec()),
                RejectCode::CONGESTION,
            ),
        ];
        for (i, (ext, code)) in cases.into_iter().enumerate() {
            let mut l = listener(t0);
            let c = l.cookie(peer(), 0);
            l.handle_datagram(t0, peer(), &conclusion(i as u32 + 1, c, vec![ext]));
            assert_eq!(l.poll_event(), None);
            let hs = sent_handshakes(&mut l);
            assert!(
                matches!(hs.as_slice(), [(_, h)] if h.kind == HandshakeKind::Rejection(code.0)),
                "{code:?}: {hs:?}"
            );
        }
        // Live congestion control is what we do.
        let mut l = listener(t0);
        let c = l.cookie(peer(), 0);
        l.handle_datagram(
            t0,
            peer(),
            &conclusion(1, c, vec![Extension::Congestion(b"live".to_vec())]),
        );
        assert!(matches!(l.poll_event(), Some(Event::Connect { .. })));
    }

    #[test]
    fn connection_limit_and_handshake_rate() {
        let t0 = Instant::now();
        let mut l = Listener::with_key(
            Config {
                max_connections: 2,
                handshakes_per_second: 5,
                ..Config::default()
            },
            t0,
            Fixed::default(),
        );
        let c = l.cookie(peer(), 0);
        for s in 1..=3 {
            l.handle_datagram(t0, peer(), &conclusion(s, c, vec![]));
        }
        let hs = sent_handshakes(&mut l);
        assert!(
            matches!(hs.as_slice(), [(3, h)] if h.kind == HandshakeKind::Rejection(1005)),
            "{hs:?}"
        );
        // A burst of 10 allowed, then nothing until the allowance refills.
        for s in 0..20 {
            l.handle_datagram(t0, peer(), &induction(100 + s));
        }
        assert_eq!(sent_handshakes(&mut l).len(), 7);
        assert_eq!(l.stats().rate_limited, 13);
        l.handle_datagram(t0 + Duration::from_secs(1), peer(), &induction(200));
        assert_eq!(sent_handshakes(&mut l).len(), 1);
    }

    #[test]
    fn an_unanswered_caller_is_forgotten() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        let id = connect(&mut l, t0, 1);
        let t = l.poll_timeout().expect("pending timer");
        assert_eq!(t - t0, Duration::from_secs(5));
        l.handle_timeout(t);
        assert!(matches!(
            l.poll_event(),
            Some(Event::Closed { id: i, reason: None, .. }) if i == id
        ));
        assert_eq!(l.connections(), 0);
    }

    #[test]
    fn shutdown_and_idle_timeout_close() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        let id = connect(&mut l, t0, 1);
        l.accept(id, t0);
        let mut b = Vec::new();
        ControlPacket::new(0, id.0, Control::Shutdown).encode(&mut b);
        l.handle_datagram(t0, peer(), &b);
        assert!(matches!(
            l.poll_event(),
            Some(Event::Closed {
                reason: Some(CloseReason::Shutdown),
                ..
            })
        ));
        assert_eq!(l.poll_timeout(), None);

        while l.poll_transmit().is_some() {}
        let id = connect(&mut l, t0, 2);
        l.accept(id, t0);
        let mut t = t0;
        while l.connections() > 0 {
            t = l.poll_timeout().expect("timer while connected");
            l.handle_timeout(t);
        }
        assert_eq!(t - t0, Duration::from_secs(5));
        let events: Vec<_> = std::iter::from_fn(|| l.poll_event()).collect();
        assert!(matches!(
            events.last(),
            Some(Event::Closed {
                reason: Some(CloseReason::Timeout),
                ..
            })
        ));
        // Keepalives went out meanwhile.
        let keepalives = std::iter::from_fn(|| l.poll_transmit())
            .filter(|(_, b)| b.get(..4) == Some(&[0x80, 0x01, 0, 0]))
            .count();
        assert_eq!(keepalives, 4);
    }

    #[test]
    fn close_sends_shutdown() {
        let t0 = Instant::now();
        let mut l = listener(t0);
        let id = connect(&mut l, t0, 1);
        l.accept(id, t0);
        sent_handshakes(&mut l);
        l.close(id, t0);
        let out: Vec<_> = std::iter::from_fn(|| l.poll_transmit()).collect();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out.first().and_then(|(_, b)| b.get(..4)),
            Some(&[0x80, 0x05, 0, 0][..])
        );
        assert_eq!(l.connections(), 0);
    }

    #[test]
    fn ipv6_peer_address_field() {
        let a: SocketAddr = "[2001:db8::1]:5000".parse().expect("addr");
        let f = encode_ip(a);
        assert_eq!(f[..4], [0xB8, 0x0D, 0x01, 0x20]);
        assert_eq!(f[12..], [1, 0, 0, 0]);
    }
}
