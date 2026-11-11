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
//!   XOR-MAPPED-ADDRESS, and its tuple becomes validated: one the session
//!   may send to.
//! - A response is never larger than the request that caused it; one that
//!   would be is not sent.
//! - The selected tuple is set and moved only by a successful request with
//!   USE-CANDIDATE (RFC 8445 section 8.2: a lite agent sends media only on
//!   a nominated pair). Nothing else moves it.
//! - Consent: each successful check on the selected tuple renews it, and
//!   it expires [`IceConfig::consent_timeout`] (30 s) after the last.

use crate::entropy::{ALPHANUMERIC, Entropy, EntropyError};
use crate::stun::{self, Builder, Message};
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

/// The publisher's end of a path: the transport and its address. For TCP
/// the address names the connection (behind a front proxy, the address
/// PROXY protocol gave, which W3d adds). `Debug` leaves the address out.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tuple {
    /// UDP or TCP.
    pub transport: Transport,
    /// The publisher's address and port.
    pub remote: SocketAddr,
}

impl Tuple {
    /// A UDP tuple.
    pub const fn udp(remote: SocketAddr) -> Self {
        Self {
            transport: Transport::Udp,
            remote,
        }
    }

    /// A TCP tuple.
    pub const fn tcp(remote: SocketAddr) -> Self {
        Self {
            transport: Transport::Tcp,
            remote,
        }
    }
}

impl fmt::Debug for Tuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tuple({:?}, <address>)", self.transport)
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
    /// Most validated tuples remembered. A new one beyond it evicts the
    /// oldest that is not selected. 1 to 64; default 8.
    pub max_tuples: usize,
    /// Consent lasts this long after the last successful check on the
    /// selected tuple (RFC 7675 section 5.1). 1 to 300 s; default 30 s.
    pub consent_timeout: Duration,
}

impl Default for IceConfig {
    fn default() -> Self {
        Self {
            max_tuples: 8,
            consent_timeout: Duration::from_secs(30),
        }
    }
}

impl IceConfig {
    /// Checks every field's range.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=64).contains(&self.max_tuples) {
            return Err("max_tuples");
        }
        if !(Duration::from_secs(1)..=Duration::from_secs(300)).contains(&self.consent_timeout) {
            return Err("consent_timeout");
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

/// The ICE-lite agent of one session.
pub struct Agent {
    config: IceConfig,
    /// `<our ufrag>:<their ufrag>`.
    username: Vec<u8>,
    key: Zeroizing<Vec<u8>>,
    validated: Vec<Tuple>,
    selected: Option<Tuple>,
    last_consent: Option<Instant>,
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
            last_consent: None,
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
        self.validated.contains(tuple)
    }

    /// When consent for the selected tuple runs out.
    pub fn consent_expires(&self) -> Option<Instant> {
        self.last_consent
            .and_then(|t| t.checked_add(self.config.consent_timeout))
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
        // Authenticated from here.
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
        let mut nominated = false;
        if success {
            self.stats.successes += 1;
            self.validate(tuple);
            if m.use_candidate && self.selected != Some(tuple) {
                self.selected = Some(tuple);
                self.stats.nominations += 1;
                nominated = true;
            }
            if self.selected == Some(tuple) {
                self.last_consent = Some(now);
            }
        }
        Some(Answer {
            response,
            success,
            nominated,
        })
    }

    fn validate(&mut self, tuple: Tuple) {
        if self.validated.contains(&tuple) {
            return;
        }
        if self.validated.len() >= self.config.max_tuples.max(1) {
            let selected = self.selected;
            if let Some(i) = self.validated.iter().position(|t| Some(*t) != selected) {
                self.validated.remove(i);
                self.stats.evicted += 1;
            } else {
                return;
            }
        }
        self.validated.push(tuple);
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
        assert_eq!(a.consent_expires(), None);
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
        assert_eq!(a.consent_expires(), Some(t0 + Duration::from_secs(30)));
        // A plain check on another tuple validates it, moves nothing.
        let t1 = t0 + Duration::from_secs(5);
        let plain = Req::honest(&c);
        assert!(
            !a.handle(t1, tuple(2), &plain.build(6))
                .expect("answered")
                .nominated
        );
        assert_eq!(a.selected(), Some(tuple(1)));
        assert_eq!(a.consent_expires(), Some(t0 + Duration::from_secs(30)));
        // A plain check on the selected tuple renews consent.
        a.handle(t1, tuple(1), &plain.build(7)).expect("answered");
        assert_eq!(a.consent_expires(), Some(t1 + Duration::from_secs(30)));
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
    fn response_never_larger_than_request() {
        // A request with only USERNAME, MESSAGE-INTEGRITY and FINGERPRINT,
        // from an IPv6 address: the success response would be larger, so
        // nothing is sent.
        let c = creds();
        let mut a = Agent::new(IceConfig::default(), &c, "peer");
        let user = format!("{}:peer", c.ufrag());
        let req = Builder::new(stun::BINDING_REQUEST, &[9; 12])
            .attribute(stun::attr::USERNAME, user.as_bytes())
            .integrity(c.pwd().as_bytes())
            .fingerprint();
        let v6 = Tuple::udp("[2001:db8::1]:5000".parse().expect("answered"));
        assert_eq!(req.len(), 72);
        assert!(a.handle(Instant::now(), v6, &req).is_none());
        assert_eq!(a.stats().oversized, 1);
        // IPv4: the response (64 bytes) fits.
        let ans = a.handle(Instant::now(), tuple(9), &req).expect("answered");
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
