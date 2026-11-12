//! An ICE-lite agent in the controlled role (RFC 8445 sections 2.5, 7.3
//! and 8.2) with consent freshness (RFC 7675), and the first-byte
//! demultiplexer (RFC 7983 as updated by RFC 9443).
//!
//! The agent sends no checks of its own. It answers Binding requests and
//! learns the publisher's addresses from them, so it never needs the
//! publisher's candidates. The rules:
//!
//! - A request is authenticated when its USERNAME is exactly
//!   `<our ufrag>:<their ufrag>`, its FINGERPRINT checks and its
//!   MESSAGE-INTEGRITY verifies under our password. Anything else gets no
//!   answer at all: no 400 or 401, unlike RFC 8489 section 9.1.3, so the
//!   port never answers a stranger.
//! - An authenticated request with an unknown comprehension-required
//!   attribute gets 420 with UNKNOWN-ATTRIBUTES; one carrying
//!   ICE-CONTROLLED gets 487 Role Conflict, because a lite agent is always
//!   controlled and cannot switch (RFC 8445 section 7.3.1.1).
//! - Every other authenticated request gets a success response with
//!   XOR-MAPPED-ADDRESS, and its tuple becomes validated.
//! - MESSAGE-INTEGRITY does not cover the source address (RFC 8445
//!   section 19.2), so a captured check verifies from anywhere. The agent
//!   remembers the transaction IDs of the last
//!   [`IceConfig::max_transactions`] authenticated requests (the ID is
//!   covered by MESSAGE-INTEGRITY; a hash of the datagram would not do,
//!   since bytes after it are not). A repeated ID is answered only from
//!   the tuple that first sent it, as a retransmission, and never
//!   validates, nominates or renews consent. From any other tuple it gets
//!   nothing. A copy that arrives before its original, or after the ID is
//!   forgotten, is the session's to contain: see `crate::session`.
//! - A response is never larger than the request that caused it; one that
//!   would be is not sent.
//! - The selected tuple is set and moved only by a successful request with
//!   USE-CANDIDATE (RFC 8445 section 8.2: a lite agent sends media only on
//!   a nominated pair), and only to a validated tuple. Nothing else moves
//!   it. Where the session actually sends is the session's decision.
//! - Consent (RFC 7675) is kept per validated tuple: each new successful
//!   check from a tuple renews it, and it expires
//!   [`IceConfig::consent_timeout`] (30 s) after the last. The session
//!   reads it for the tuple it sends to.

use crate::entropy::{ALPHANUMERIC, Entropy, EntropyError};
use crate::stun::{self, Builder, Message};
use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// How a datagram on the media port is told apart (RFC 7983 section 7, as
/// updated by RFC 9443 section 4), by its first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 0 to 3.
    Stun,
    /// 20 to 63.
    Dtls,
    /// 128 to 191, and not RTCP.
    Rtp,
    /// 128 to 191 with an RTCP packet type in the second byte (RFC 5761
    /// section 4: the low seven bits 64 to 95).
    Rtcp,
    /// Anything else: ZRTP, TURN channels, QUIC, empty. Dropped.
    Other,
}

/// Classifies a datagram by its first byte.
pub fn classify(b: &[u8]) -> Kind {
    match b.first() {
        Some(0..=3) => Kind::Stun,
        Some(20..=63) => Kind::Dtls,
        Some(128..=191) => match b.get(1) {
            Some(pt) if (64..=95).contains(&(pt & 0x7f)) => Kind::Rtcp,
            _ => Kind::Rtp,
        },
        _ => Kind::Other,
    }
}

/// How a tuple carries packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Transport {
    /// One datagram per packet.
    Udp,
    /// RFC 4571 frames on one TCP connection (a passive ICE-TCP candidate,
    /// RFC 6544).
    Tcp,
}

/// The publisher's end of a path: the transport, its address and, for TCP,
/// the connection.
///
/// A UDP tuple is the publisher's address and port. A TCP tuple is one
/// connection to the ICE-TCP listener (RFC 6544: each connection is its own
/// candidate pair): `connection` is the listener's number for it, never
/// reused while the process runs, and `remote` is the publisher's address as
/// the connection gives it (behind a front proxy, the source in its PROXY
/// protocol v2 header), which a check's XOR-MAPPED-ADDRESS reports. The
/// address alone would not name a connection: a header can repeat one, and
/// a port is reused after a close. Tuples compare all three fields, so a
/// TCP tuple never equals a UDP one, nor one of another connection. Build
/// them with [`Tuple::udp`] and [`Tuple::tcp`]. `Debug` leaves the address
/// out.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tuple {
    /// UDP or TCP.
    pub transport: Transport,
    /// The publisher's address and port.
    pub remote: SocketAddr,
    /// The TCP connection's number; 0 for UDP.
    pub connection: u64,
}

impl Tuple {
    /// A UDP tuple.
    pub const fn udp(remote: SocketAddr) -> Self {
        Self {
            transport: Transport::Udp,
            remote,
            connection: 0,
        }
    }

    /// A TCP tuple: connection number `connection` to the listener, from
    /// `remote`.
    pub const fn tcp(remote: SocketAddr, connection: u64) -> Self {
        Self {
            transport: Transport::Tcp,
            remote,
            connection,
        }
    }
}

impl fmt::Debug for Tuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.transport {
            Transport::Udp => f.write_str("Tuple(Udp, <address>)"),
            Transport::Tcp => write!(f, "Tuple(Tcp #{}, <address>)", self.connection),
        }
    }
}

/// Our ICE username fragment and password, made per session. The password
/// is zeroed on drop and kept out of `Debug`.
pub struct Credentials {
    ufrag: String,
    pwd: Zeroizing<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("ufrag", &self.ufrag)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    /// Length of our ufrag: 8 letters or digits, about 47 bits. It names
    /// the session to the UDP owner and is not a secret.
    pub const UFRAG_LEN: usize = 8;
    /// Length of our password: 24 letters or digits, about 142 bits (RFC
    /// 8445 section 5.3 asks for at least 128).
    pub const PWD_LEN: usize = 24;

    /// New random credentials.
    pub fn generate() -> Result<Self, EntropyError> {
        Self::generate_with(&mut Entropy::System)
    }

    /// As [`Credentials::generate`], from a seed. Not for production use.
    #[cfg(any(test, feature = "testing"))]
    pub fn generate_seeded(seed: [u8; 32]) -> Result<Self, EntropyError> {
        Self::generate_with(&mut Entropy::seeded(seed))
    }

    pub(crate) fn generate_with(e: &mut Entropy) -> Result<Self, EntropyError> {
        Ok(Self {
            ufrag: e.string(ALPHANUMERIC, Self::UFRAG_LEN)?,
            pwd: Zeroizing::new(e.string(ALPHANUMERIC, Self::PWD_LEN)?),
        })
    }

    /// The ufrag, for `a=ice-ufrag`.
    pub fn ufrag(&self) -> &str {
        &self.ufrag
    }

    /// The password, for `a=ice-pwd` only.
    pub fn pwd(&self) -> &str {
        &self.pwd
    }
}

/// The agent's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IceConfig {
    /// Most validated tuples remembered. A new one beyond it evicts one that
    /// is neither selected nor the session's send tuple: first one that has
    /// never delivered fresh media, then the one whose last fresh media is
    /// oldest, the earliest validated breaking ties. 2 to 64; default 8.
    pub max_tuples: usize,
    /// Consent for a tuple lasts this long after the last successful check
    /// from it (RFC 7675 section 5.1). 1 to 300 s; default 30 s.
    pub consent_timeout: Duration,
    /// Transaction IDs of authenticated requests remembered, to tell a
    /// replay from a new check. 16 to 4096; default 512, more than the
    /// checks Chrome 157 sent in the first 10 s (about 130).
    pub max_transactions: usize,
}

impl Default for IceConfig {
    fn default() -> Self {
        Self {
            max_tuples: 8,
            consent_timeout: Duration::from_secs(30),
            max_transactions: 512,
        }
    }
}

impl IceConfig {
    /// Checks every field's range.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(2..=64).contains(&self.max_tuples) {
            return Err("max_tuples");
        }
        if !(Duration::from_secs(1)..=Duration::from_secs(300)).contains(&self.consent_timeout) {
            return Err("consent_timeout");
        }
        if !(16..=4096).contains(&self.max_transactions) {
            return Err("max_transactions");
        }
        Ok(())
    }
}

/// Counters. None holds an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IceStats {
    /// STUN datagrams that do not parse.
    pub malformed: u64,
    /// Parsed messages that are not Binding requests (indications,
    /// responses, other methods).
    pub not_requests: u64,
    /// Binding requests without a valid FINGERPRINT.
    pub bad_fingerprint: u64,
    /// Binding requests whose USERNAME is missing or not ours.
    pub wrong_username: u64,
    /// Binding requests whose MESSAGE-INTEGRITY is missing or wrong.
    pub bad_integrity: u64,
    /// Authenticated requests answered 487.
    pub role_conflicts: u64,
    /// Authenticated requests answered 420.
    pub unknown_attributes: u64,
    /// Answers not sent because they would be larger than the request.
    pub oversized: u64,
    /// Successful checks.
    pub successes: u64,
    /// Times the selected tuple was set or moved.
    pub nominations: u64,
    /// Validated tuples forgotten to make room.
    pub evicted: u64,
    /// Repeated transaction IDs from the tuple that first sent them,
    /// answered without changing anything.
    pub repeats: u64,
    /// Repeated transaction IDs from another tuple: replays, dropped.
    pub replays: u64,
    /// New checks not answered because every validated tuple was selected
    /// or protected.
    pub full: u64,
}

/// What a request did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The response, for the tuple the request came from.
    pub response: Vec<u8>,
    /// The check succeeded and its tuple is validated.
    pub success: bool,
    /// The selected tuple was set or moved to this one.
    pub nominated: bool,
}

/// A validated tuple.
#[derive(Debug, Clone, Copy)]
struct Validated {
    tuple: Tuple,
    /// When it last passed a new check.
    checked: Instant,
    /// When fresh SRTP or SRTCP last arrived from it, as the session says.
    media: Option<Instant>,
}

/// The ICE-lite agent of one session.
pub struct Agent {
    config: IceConfig,
    /// `<our ufrag>:<their ufrag>`.
    username: Vec<u8>,
    key: Zeroizing<Vec<u8>>,
    /// Validated tuples, in the order they were validated.
    validated: Vec<Validated>,
    selected: Option<Tuple>,
    /// A tuple eviction must keep besides the selected one: where the
    /// session sends.
    protected: Option<Tuple>,
    /// Transaction IDs seen, oldest first, with the tuple that sent each.
    seen: VecDeque<([u8; 12], Tuple)>,
    stats: IceStats,
}

impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("validated", &self.validated.len())
            .field("selected", &self.selected.is_some())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// An agent answering checks for `local` from a peer whose offer gave
    /// `remote_ufrag`.
    pub fn new(config: IceConfig, local: &Credentials, remote_ufrag: &str) -> Self {
        let mut username = local.ufrag().as_bytes().to_vec();
        username.push(b':');
        username.extend_from_slice(remote_ufrag.as_bytes());
        Self {
            config,
            username,
            key: Zeroizing::new(local.pwd().as_bytes().to_vec()),
            validated: Vec::new(),
            selected: None,
            protected: None,
            seen: VecDeque::new(),
            stats: IceStats::default(),
        }
    }

    /// Counters.
    pub fn stats(&self) -> IceStats {
        self.stats
    }

    /// The selected tuple, once a check with USE-CANDIDATE has succeeded.
    pub fn selected(&self) -> Option<Tuple> {
        self.selected
    }

    /// Whether a check from this tuple has succeeded (and it has not been
    /// evicted since).
    pub fn is_validated(&self, tuple: &Tuple) -> bool {
        self.validated.iter().any(|v| v.tuple == *tuple)
    }

    /// When fresh SRTP or SRTCP last arrived from a validated tuple, as the
    /// session reported it; `None` for one that has delivered none since it
    /// was validated, or is not validated.
    pub fn media_from(&self, tuple: &Tuple) -> Option<Instant> {
        self.validated
            .iter()
            .find(|v| v.tuple == *tuple)
            .and_then(|v| v.media)
    }

    /// Fresh SRTP or SRTCP arrived from a validated tuple: it now outranks,
    /// for eviction, every tuple whose media is older or that has none.
    pub fn note_media(&mut self, tuple: &Tuple, now: Instant) {
        if let Some(v) = self.validated.iter_mut().find(|v| v.tuple == *tuple) {
            v.media = Some(now);
        }
    }

    /// Keeps `tuple` validated through eviction, as well as the selected
    /// one. The session passes the tuple it sends to.
    pub fn protect(&mut self, tuple: Option<Tuple>) {
        self.protected = tuple;
    }

    /// When consent for a validated tuple runs out.
    pub fn consent_expires(&self, tuple: &Tuple) -> Option<Instant> {
        self.validated
            .iter()
            .find(|v| v.tuple == *tuple)
            .and_then(|v| v.checked.checked_add(self.config.consent_timeout))
    }

    /// One STUN datagram from `tuple`. Returns the answer to send back to
    /// that tuple, if any; unauthenticated input never gets one.
    pub fn handle(&mut self, now: Instant, tuple: Tuple, b: &[u8]) -> Option<Answer> {
        let Ok(m) = Message::parse(b) else {
            self.stats.malformed += 1;
            return None;
        };
        if m.msg_type != stun::BINDING_REQUEST {
            self.stats.not_requests += 1;
            return None;
        }
        if !m.check_fingerprint(b) {
            self.stats.bad_fingerprint += 1;
            return None;
        }
        if m.username != Some(self.username.as_slice()) {
            self.stats.wrong_username += 1;
            return None;
        }
        if !m.check_integrity(b, &self.key) {
            self.stats.bad_integrity += 1;
            return None;
        }
        // Authenticated from here. A repeated transaction ID is a
        // retransmission only from the tuple that first sent it.
        // A new ID is recorded only once it is answered: a request refused
        // here is not taken, so its retransmission is new too.
        let repeat = match self.seen.iter().find(|(id, _)| *id == m.transaction) {
            Some((_, first)) if *first != tuple => {
                self.stats.replays += 1;
                return None;
            }
            Some(_) => true,
            None => false,
        };
        let (response, success) = if !m.unknown_required.is_empty() {
            self.stats.unknown_attributes += 1;
            let r = Builder::new(stun::BINDING_ERROR, &m.transaction)
                .error_code(420, "Unknown Attribute")
                .unknown_attributes(&m.unknown_required)
                .integrity(&self.key)
                .fingerprint();
            (r, false)
        } else if m.ice_controlled.is_some() {
            self.stats.role_conflicts += 1;
            let r = Builder::new(stun::BINDING_ERROR, &m.transaction)
                .error_code(487, "Role Conflict")
                .integrity(&self.key)
                .fingerprint();
            (r, false)
        } else {
            let r = Builder::new(stun::BINDING_SUCCESS, &m.transaction)
                .xor_mapped_address(tuple.remote)
                .integrity(&self.key)
                .fingerprint();
            (r, true)
        };
        if response.len() > b.len() {
            self.stats.oversized += 1;
            return None;
        }
        if repeat {
            self.stats.repeats += 1;
            return Some(Answer {
                response,
                success: false,
                nominated: false,
            });
        }
        let mut nominated = false;
        // Only a validated tuple can be selected; with no room to remember
        // this one, the check is not answered (and its ID not recorded).
        if success && !self.validate(tuple, now) {
            self.stats.full += 1;
            return None;
        }
        if self.seen.len() >= self.config.max_transactions.max(1) {
            self.seen.pop_front();
        }
        self.seen.push_back((m.transaction, tuple));
        if success {
            self.stats.successes += 1;
            if m.use_candidate && self.selected != Some(tuple) {
                self.selected = Some(tuple);
                self.stats.nominations += 1;
                nominated = true;
            }
        }
        Some(Answer {
            response,
            success,
            nominated,
        })
    }

    /// Remembers a tuple as validated at `now`, evicting one if full.
    /// Returns whether it is validated.
    ///
    /// The victim is never the selected tuple or the protected one (the
    /// session's send tuple). Of the rest, it is one that has never
    /// delivered fresh SRTP or SRTCP if there is one, else the one whose
    /// last fresh media is oldest; the earliest validated breaks ties. So
    /// a publisher's tuple that keeps delivering media outranks every
    /// tuple that has delivered none, and every one whose last fresh media
    /// is older: checks alone, replayed or not, cannot push it out.
    fn validate(&mut self, tuple: Tuple, now: Instant) -> bool {
        if let Some(v) = self.validated.iter_mut().find(|v| v.tuple == tuple) {
            v.checked = now;
            return true;
        }
        if self.validated.len() >= self.config.max_tuples.max(2) {
            let keep = [self.selected, self.protected];
            let Some(i) = self
                .validated
                .iter()
                .enumerate()
                .filter(|(_, v)| !keep.contains(&Some(v.tuple)))
                .min_by_key(|&(i, v)| (v.media, i))
                .map(|(i, _)| i)
            else {
                return false;
            };
            self.validated.remove(i);
            self.stats.evicted += 1;
        }
        self.validated.push(Validated {
            tuple,
            checked: now,
            media: None,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Credentials {
        Credentials::generate_seeded([3; 32]).expect("answered")
    }

    fn tuple(port: u16) -> Tuple {
        Tuple::udp(SocketAddr::from(([192, 0, 2, 1], port)))
    }

    struct Req {
        username: Vec<u8>,
        key: Vec<u8>,
        controlled: bool,
        use_candidate: bool,
        extra: Option<u16>,
        fingerprint: bool,
    }

    impl Req {
        fn honest(c: &Credentials) -> Self {
            Self {
                username: format!("{}:peer", c.ufrag()).into_bytes(),
                key: c.pwd().as_bytes().to_vec(),
                controlled: false,
                use_candidate: false,
                extra: None,
                fingerprint: true,
            }
        }
        fn build(&self, tid: u8) -> Vec<u8> {
            let mut b = Builder::new(stun::BINDING_REQUEST, &[tid; 12])
                .attribute(stun::attr::USERNAME, &self.username)
                .attribute(0xc057, &[0, 1, 0, 0])
                .attribute(stun::attr::PRIORITY, &0x6e00_01ffu32.to_be_bytes());
            b = if self.controlled {
                b.attribute(stun::attr::ICE_CONTROLLED, &[1; 8])
            } else {
                b.attribute(stun::attr::ICE_CONTROLLING, &[2; 8])
            };
            if self.use_candidate {
                b = b.attribute(stun::attr::USE_CANDIDATE, &[]);
            }
            if let Some(t) = self.extra {
                b = b.attribute(t, b"x");
            }
            let b = b.integrity(&self.key);
            if self.fingerprint {
                b.fingerprint()
            } else {
                b.finish()
            }
        }
    }

    #[test]
    fn classify_by_first_byte() {
        assert_eq!(classify(&[0, 1]), Kind::Stun);
        assert_eq!(classify(&[3]), Kind::Stun);
        assert_eq!(classify(&[22, 0xfe]), Kind::Dtls);
        assert_eq!(classify(&[63]), Kind::Dtls);
        assert_eq!(classify(&[0x80, 96]), Kind::Rtp);
        assert_eq!(classify(&[0x80, 0x80 | 111]), Kind::Rtp);
        assert_eq!(classify(&[0x80, 200]), Kind::Rtcp);
        assert_eq!(classify(&[0x81, 205]), Kind::Rtcp);
        for other in [4u8, 16, 19, 64, 79, 100, 127, 192, 255] {
            assert_eq!(classify(&[other, 0]), Kind::Other, "{other}");
        }
        assert_eq!(classify(&[]), Kind::Other);
    }

    #[test]
    fn honest_check_is_answered_and_validates() {
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let t0 = Instant::now();
        let req = Req::honest(&c).build(1);
        let ans = a.handle(t0, tuple(5000), &req).expect("answered");
        assert!(ans.success && !ans.nominated);
        assert!(ans.response.len() <= req.len());
        let m = Message::parse(&ans.response).expect("answered");
        assert_eq!(m.msg_type, stun::BINDING_SUCCESS);
        assert_eq!(m.transaction, [1; 12]);
        assert_eq!(m.xor_mapped, Some(tuple(5000).remote));
        assert!(m.check_integrity(&ans.response, c.pwd().as_bytes()));
        assert!(m.check_fingerprint(&ans.response));
        assert!(a.is_validated(&tuple(5000)));
        assert_eq!(a.selected(), None, "no USE-CANDIDATE yet");
        assert_eq!(
            a.consent_expires(&tuple(5000)),
            Some(t0 + Duration::from_secs(30))
        );
        assert_eq!(a.consent_expires(&tuple(5001)), None);
    }

    #[test]
    fn unauthenticated_requests_get_nothing() {
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let now = Instant::now();
        let mut wrong_key = Req::honest(&c);
        wrong_key.key = b"not-the-password-at-all".to_vec();
        let mut wrong_user = Req::honest(&c);
        wrong_user.username = b"someone:else".to_vec();
        let mut reversed = Req::honest(&c);
        reversed.username = format!("peer:{}", c.ufrag()).into_bytes();
        let mut no_fp = Req::honest(&c);
        no_fp.fingerprint = false;
        for r in [wrong_key, wrong_user, reversed, no_fp] {
            let mut r = r;
            r.use_candidate = true;
            assert!(a.handle(now, tuple(1), &r.build(2)).is_none());
        }
        assert!(a.handle(now, tuple(1), b"\x00\x01garbage").is_none());
        let s = a.stats();
        assert_eq!(
            (
                s.bad_integrity,
                s.wrong_username,
                s.bad_fingerprint,
                s.malformed
            ),
            (1, 2, 1, 1)
        );
        assert!(!a.is_validated(&tuple(1)) && a.selected().is_none());
    }

    #[test]
    fn controlled_peer_gets_487_and_is_not_validated() {
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let mut r = Req::honest(&c);
        r.controlled = true;
        r.use_candidate = true;
        let req = r.build(3);
        let ans = a.handle(Instant::now(), tuple(7), &req).expect("487");
        assert!(!ans.success && !ans.nominated);
        let m = Message::parse(&ans.response).expect("answered");
        assert_eq!(m.msg_type, stun::BINDING_ERROR);
        assert_eq!(m.error.map(|e| e.0), Some(487));
        assert!(m.check_integrity(&ans.response, c.pwd().as_bytes()));
        assert!(ans.response.len() <= req.len());
        assert!(!a.is_validated(&tuple(7)) && a.selected().is_none());
        assert_eq!(a.stats().role_conflicts, 1);
    }

    #[test]
    fn unknown_required_attribute_gets_420() {
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let mut r = Req::honest(&c);
        r.extra = Some(0x0033);
        let req = r.build(4);
        let ans = a.handle(Instant::now(), tuple(8), &req).expect("420");
        let m = Message::parse(&ans.response).expect("answered");
        assert_eq!(m.error.map(|e| e.0), Some(420));
        assert_eq!(m.unknown_listed, [0x0033]);
        assert!(!ans.success);
    }

    #[test]
    fn use_candidate_selects_and_moves_and_renews_consent() {
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let t0 = Instant::now();
        let mut r = Req::honest(&c);
        r.use_candidate = true;
        let ans = a.handle(t0, tuple(1), &r.build(5)).expect("answered");
        assert!(ans.nominated);
        assert_eq!(a.selected(), Some(tuple(1)));
        assert_eq!(
            a.consent_expires(&tuple(1)),
            Some(t0 + Duration::from_secs(30))
        );
        // A plain check on another tuple validates it, moves nothing.
        let t1 = t0 + Duration::from_secs(5);
        let plain = Req::honest(&c);
        assert!(
            !a.handle(t1, tuple(2), &plain.build(6))
                .expect("answered")
                .nominated
        );
        assert_eq!(a.selected(), Some(tuple(1)));
        assert_eq!(
            a.consent_expires(&tuple(1)),
            Some(t0 + Duration::from_secs(30))
        );
        assert_eq!(
            a.consent_expires(&tuple(2)),
            Some(t1 + Duration::from_secs(30))
        );
        // A plain check on the selected tuple renews its consent.
        a.handle(t1, tuple(1), &plain.build(7)).expect("answered");
        assert_eq!(
            a.consent_expires(&tuple(1)),
            Some(t1 + Duration::from_secs(30))
        );
        // USE-CANDIDATE from the second tuple moves the selection.
        let t2 = t1 + Duration::from_secs(1);
        assert!(
            a.handle(t2, tuple(2), &r.build(8))
                .expect("answered")
                .nominated
        );
        assert_eq!(a.selected(), Some(tuple(2)));
        assert_eq!(a.stats().nominations, 2);
        // Again from the selected tuple: no new nomination.
        assert!(
            !a.handle(t2, tuple(2), &r.build(9))
                .expect("answered")
                .nominated
        );
    }

    #[test]
    fn tuples_are_bounded_and_the_selected_one_kept() {
        let c = creds();
        let config = IceConfig {
            max_tuples: 2,
            ..IceConfig::default()
        };
        let mut a = Agent::new(config, &c, "peer");
        let now = Instant::now();
        let mut nominate = Req::honest(&c);
        nominate.use_candidate = true;
        a.handle(now, tuple(1), &nominate.build(1))
            .expect("answered");
        let plain = Req::honest(&c);
        for port in 2..6 {
            a.handle(now, tuple(port), &plain.build(port as u8))
                .expect("answered");
        }
        assert!(a.is_validated(&tuple(1)), "selected kept");
        assert!(a.is_validated(&tuple(5)));
        assert!(!a.is_validated(&tuple(2)));
        assert_eq!(a.stats().evicted, 3);
    }

    #[test]
    fn a_repeated_transaction_changes_nothing() {
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let t0 = Instant::now();
        let mut r = Req::honest(&c);
        r.use_candidate = true;
        let check = r.build(40);
        a.handle(t0, tuple(1), &check).expect("answered");
        assert_eq!(
            a.consent_expires(&tuple(1)),
            Some(t0 + Duration::from_secs(30))
        );
        // From another tuple: nothing, and nothing changes.
        let t1 = t0 + Duration::from_secs(5);
        assert!(a.handle(t1, tuple(2), &check).is_none());
        assert!(!a.is_validated(&tuple(2)));
        assert_eq!(a.selected(), Some(tuple(1)));
        // From its own tuple: answered, as a retransmission, but consent
        // is not renewed.
        let again = a.handle(t1, tuple(1), &check).expect("retransmission");
        assert!(!again.success && !again.nominated);
        assert!(again.response.len() <= check.len());
        assert_eq!(
            a.consent_expires(&tuple(1)),
            Some(t0 + Duration::from_secs(30))
        );
        let st = a.stats();
        assert_eq!((st.replays, st.repeats, st.successes), (1, 1, 1));
    }

    #[test]
    fn the_first_copy_wins_and_a_fresh_check_recovers() {
        // A replay that arrives before its original takes the selection;
        // the original is then the repeat. The publisher's next check, a
        // new transaction, takes it back.
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let now = Instant::now();
        let mut r = Req::honest(&c);
        r.use_candidate = true;
        let check = r.build(41);
        assert!(
            a.handle(now, tuple(9), &check)
                .expect("first copy")
                .nominated
        );
        assert!(
            a.handle(now, tuple(1), &check).is_none(),
            "the original, second"
        );
        assert_eq!(a.selected(), Some(tuple(9)));
        assert!(
            a.handle(now, tuple(1), &r.build(42))
                .expect("fresh")
                .nominated
        );
        assert_eq!(a.selected(), Some(tuple(1)));
    }

    #[test]
    fn forgotten_transactions_are_new_again() {
        let c = creds();
        let config = IceConfig {
            max_transactions: 16,
            ..IceConfig::default()
        };
        let mut a = Agent::new(config, &c, "peer");
        let now = Instant::now();
        let r = Req::honest(&c);
        let first = r.build(0);
        a.handle(now, tuple(1), &first).expect("answered");
        for i in 1..=16u8 {
            a.handle(now, tuple(1), &r.build(i)).expect("answered");
        }
        // The first ID has been pushed out: from another tuple it is a new
        // check again. Containing that is the session's job.
        assert!(a.handle(now, tuple(2), &first).expect("forgotten").success);
        assert_eq!(a.stats().replays, 0);
    }

    #[test]
    fn eviction_spares_the_tuple_with_the_freshest_media() {
        let c = creds();
        let config = IceConfig {
            max_tuples: 5,
            ..IceConfig::default()
        };
        let mut a = Agent::new(config, &c, "peer");
        let t0 = Instant::now();
        let secs = Duration::from_secs;
        let plain = Req::honest(&c);
        let mut nominate = Req::honest(&c);
        nominate.use_candidate = true;
        let mut tid = 0u8;
        let mut check = |a: &mut Agent, port: u16, r: &Req| {
            tid += 1;
            a.handle(t0, tuple(port), &r.build(tid)).expect("answered");
        };
        // The publisher's tuple first, with media; one with older media;
        // two with none; and a selected one. The table is full.
        check(&mut a, 1, &plain);
        a.note_media(&tuple(1), t0 + secs(1));
        check(&mut a, 2, &plain);
        a.note_media(&tuple(2), t0);
        check(&mut a, 3, &plain);
        check(&mut a, 8, &plain);
        check(&mut a, 4, &nominate);
        let gone = |a: &Agent, port: u16| !a.is_validated(&tuple(port));
        // Tuples with no media go first, the earliest validated first.
        check(&mut a, 5, &plain);
        assert!(gone(&a, 3));
        check(&mut a, 6, &plain);
        assert!(gone(&a, 8));
        a.note_media(&tuple(5), t0 + secs(2));
        check(&mut a, 7, &plain);
        assert!(gone(&a, 6));
        // Then the oldest media.
        a.note_media(&tuple(7), t0 + secs(3));
        check(&mut a, 9, &plain);
        assert!(gone(&a, 2));
        for kept in [1, 4, 5, 7, 9] {
            assert!(!gone(&a, kept), "{kept}");
        }
        assert_eq!(a.stats().evicted, 4);
    }

    #[test]
    fn eviction_keeps_the_selected_and_protected_tuples() {
        let c = creds();
        let config = IceConfig {
            max_tuples: 2,
            ..IceConfig::default()
        };
        let mut a = Agent::new(config, &c, "peer");
        let now = Instant::now();
        let mut r = Req::honest(&c);
        r.use_candidate = true;
        a.handle(now, tuple(1), &r.build(1)).expect("answered");
        a.protect(Some(tuple(1)));
        a.handle(now, tuple(2), &r.build(2)).expect("answered");
        assert_eq!(a.selected(), Some(tuple(2)));
        // Both slots hold a tuple that must be kept: a third is not
        // answered, and nothing moves.
        assert!(a.handle(now, tuple(3), &r.build(3)).is_none());
        assert_eq!(a.stats().full, 1);
        // Its ID was not recorded: a retransmission is refused the same way,
        // not answered as a repeat.
        assert!(a.handle(now, tuple(3), &r.build(3)).is_none());
        assert_eq!((a.stats().full, a.stats().repeats), (2, 0));
        assert!(a.is_validated(&tuple(1)) && a.is_validated(&tuple(2)));
        assert_eq!(a.selected(), Some(tuple(2)));
    }

    #[test]
    fn the_selected_tuple_is_always_validated() {
        // With room for two, nominations across three tuples never leave
        // the selection on a tuple the agent has forgotten.
        let c = creds();
        let one = IceConfig {
            max_tuples: 1,
            ..IceConfig::default()
        };
        assert_eq!(one.validate(), Err("max_tuples"));
        let config = IceConfig {
            max_tuples: 2,
            ..IceConfig::default()
        };
        assert!(config.validate().is_ok());
        let mut a = Agent::new(config, &c, "peer");
        let now = Instant::now();
        let mut r = Req::honest(&c);
        r.use_candidate = true;
        for (i, port) in [1u16, 2, 3, 1, 3, 2].into_iter().enumerate() {
            a.handle(now, tuple(port), &r.build(i as u8))
                .expect("answered");
            let sel = a.selected().expect("selected");
            assert_eq!(sel, tuple(port));
            assert!(a.is_validated(&sel), "step {i}");
        }
    }

    #[test]
    fn response_never_larger_than_request() {
        // A request with only USERNAME, MESSAGE-INTEGRITY and FINGERPRINT,
        // from an IPv6 address: the success response would be larger, so
        // nothing is sent.
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let user = format!("{}:peer", c.ufrag());
        let bare = |tid: u8| {
            Builder::new(stun::BINDING_REQUEST, &[tid; 12])
                .attribute(stun::attr::USERNAME, user.as_bytes())
                .integrity(c.pwd().as_bytes())
                .fingerprint()
        };
        let req = bare(9);
        let v6 = Tuple::udp("[2001:db8::1]:5000".parse().expect("answered"));
        assert_eq!(req.len(), 72);
        assert!(a.handle(Instant::now(), v6, &req).is_none());
        assert_eq!(a.stats().oversized, 1);
        // IPv4, a new transaction: the response (64 bytes) fits.
        let ans = a
            .handle(Instant::now(), tuple(9), &bare(10))
            .expect("answered");
        assert_eq!(ans.response.len(), 64);
    }

    #[test]
    fn credentials_are_long_enough_and_debug_hides_the_password() {
        let c = Credentials::generate().expect("answered");
        assert_eq!(c.ufrag().len(), Credentials::UFRAG_LEN);
        assert_eq!(c.pwd().len(), Credentials::PWD_LEN);
        assert!(!format!("{c:?}").contains(c.pwd()));
        let t = tuple(1);
        assert!(!format!("{t:?}").contains("192.0.2.1"));
    }
}
