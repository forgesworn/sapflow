//! One WHIP session, sans-IO: ICE-lite, then `sapflow-dtls`, then SRTP,
//! then the `sapflow-rtp` receiver.
//!
//! The API mirrors `sapflow-dtls`: [`Session::handle_datagram`] and
//! [`Session::handle_timeout`] take input and the time;
//! [`Session::poll_transmit`], [`Session::poll_event`] and
//! [`Session::poll_timeout`] give output. Every datagram, STUN, DTLS, RTP
//! or RTCP, from UDP or from an RFC 4571 frame, goes through
//! `handle_datagram` with its [`Tuple`].
//!
//! The rules:
//!
//! - **Nothing to a stranger.** A tuple gets bytes only after a STUN check
//!   from it passed MESSAGE-INTEGRITY ([`crate::ice`]), and a repeated
//!   transaction ID from another tuple gets nothing.
//! - **Where the session sends.** MESSAGE-INTEGRITY does not prove the
//!   source address, so a check that arrives before its original, or after
//!   its ID is forgotten, can come from anywhere. DTLS and RTCP go to one
//!   send tuple ([`Session::send_tuple`]):
//!   - a tuple is *proven* once fresh SRTP or SRTCP from it (authenticated
//!     and inside the replay window) arrives;
//!   - the send tuple follows the selected tuple while it is unproven, and
//!     once it is proven moves only to a selected tuple that is proven too;
//!   - to a tuple that is not proven, the session sends at most
//!     [`SessionConfig::amplification_limit`] (4) times the bytes it has
//!     received from it, as RFC 9000 section 8.1 limits a QUIC server
//!     before address validation (3 there, with padded client datagrams).
//!     STUN responses are never larger than their request.
//! - **DTLS from the selected tuple only.** `sapflow-dtls` is fed only
//!   datagrams from the selected tuple. A few that arrive from a validated
//!   tuple before its USE-CANDIDATE check ([`SessionConfig::max_early_dtls`])
//!   are held, and fed if that tuple is then selected; everything else is
//!   dropped and counted. Browsers start DTLS once a check succeeds and
//!   nominate on the next, so this saves a retransmission.
//! - **SRTP and SRTCP from any validated tuple.** RFC 8445 section 12.2
//!   says an agent should be prepared to receive data on any of the
//!   candidates from the most recent exchange. A lite agent learns the
//!   publisher's candidates only from its checks, so the validated tuples
//!   stand in for them; and a browser that nominates a second pair keeps
//!   sending on the first for a moment (Chrome 157 did, on loopback).
//!   Consent (RFC 7675) governs sending only. SRTP authenticates every
//!   packet, so one from a validated tuple is taken; one from any other
//!   tuple is dropped and counted. What the session sends goes to the
//!   send tuple, below.
//! - **SRTP** keys come from the DTLS-SRTP exporter, split with
//!   `split_keying_material` as the DTLS server: the client's write keys
//!   unprotect what we receive, the server's protect our RTCP.
//! - **Media.** Each authenticated RTP packet is matched to a stream: by
//!   the offer's SSRCs (and `ssrc-group:FID`), then by the MID header
//!   extension, then by payload type, which the receiver does. A packet
//!   for a section answered with port 0, or naming an unknown MID, is
//!   dropped and counted. The receiver's events come out as
//!   [`Event::Media`]; its RTCP goes out through SRTCP.
//! - **Keyframes.** A PLI (or FIR) as soon as SRTP is up, then every
//!   [`SessionConfig::keyframe_interval`].
//! - **Ending.** A session ends with an [`EndReason`]: no ICE and DTLS by
//!   [`SessionConfig::handshake_timeout`] after creation, consent expired,
//!   no fresh SRTP or SRTCP for [`SessionConfig::media_timeout`] once
//!   connected, DTLS failed or closed by the peer, or [`Session::close`].
//!   After that it sends nothing and ignores input. The media timeout is
//!   what bounds sending after the publisher has gone: a check whose ID
//!   the agent has forgotten renews consent when replayed, but nobody can
//!   replay SRTP into the replay window.
//!
//! Counters ([`SessionStats`]) hold no addresses, and no `Debug` output
//! shows one.

use crate::entropy::{ALPHANUMERIC, Entropy, EntropyError};
use crate::ice::{Agent, Credentials, IceConfig, IceStats, Kind, Tuple, classify};
use crate::sdp::{MediaKind, Negotiated, Stream};
use sapflow_dtls::{Identity, Server};
use sapflow_rtp::receive::{Codec, Received, RtxConfig};
use sapflow_rtp::srtp::{MasterKey, Profile, Role, SrtpError, split_keying_material};
use sapflow_rtp::{
    Compound, Inbound, Outbound, Receiver, ReceiverConfig, RtpPacket, StreamConfig, StreamId,
    TransportCcConfig,
};
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Most SSRCs the session remembers from MID matching.
const MAX_ROUTES: usize = 16;

/// A session's settings. Every bound has a refusal path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// ICE and DTLS must complete within this of creation. 1 to 120 s;
    /// default 10 s (`docs/whip-ingest.md`, Resources).
    pub handshake_timeout: Duration,
    /// Keyframe request cadence after the first
    /// (`SAPFLOW_WHIP_KEYFRAME_INTERVAL`), or `None`. It replaces
    /// `receiver.keyframe_interval`. At least 100 ms; default 2 s.
    pub keyframe_interval: Option<Duration>,
    /// Largest datagram or RFC 4571 frame taken; longer ones are dropped
    /// unread. 256 to 65535; default 1500.
    pub max_datagram: usize,
    /// DTLS datagrams held from a validated tuple until it is selected. 0
    /// to 16; default 4.
    pub max_early_dtls: usize,
    /// Datagrams queued for [`Session::poll_transmit`]; beyond it output is
    /// dropped and counted. 16 to 1024; default 64.
    pub max_queued: usize,
    /// Once connected, the session ends this long after the last fresh
    /// SRTP or SRTCP packet (or after connecting, if none came). 1 to 300
    /// s; default 30 s, as consent. A publisher always sends media or
    /// sender reports, so a silent one has gone.
    pub media_timeout: Duration,
    /// To a tuple not yet proven by fresh SRTP or SRTCP, the session sends
    /// at most this many times the bytes it received from it, STUN
    /// responses included; the rest is dropped and counted. 1 to 8;
    /// default 4: one 104-byte check and the smallest ClientHello in the
    /// profile (about 120 bytes) must draw the check's 64-byte response
    /// and our 637-byte first flight at once. QUIC's limit is 3 (RFC 9000
    /// section 8.1), but QUIC clients pad their first datagrams to 1200
    /// bytes, and ICE and DTLS clients do not.
    pub amplification_limit: u32,
    /// ICE-lite bounds and consent.
    pub ice: IceConfig,
    /// DTLS bounds and timers.
    pub dtls: sapflow_dtls::Config,
    /// SRTP replay window and stream slots.
    pub srtp: sapflow_rtp::Config,
    /// The receiver: reorder, NACK, timeline, reports, events. Its
    /// `transport_cc` gives the transport-cc settings (the extension ID
    /// comes from the answer); `None` takes the defaults.
    pub receiver: ReceiverConfig,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            keyframe_interval: Some(Duration::from_secs(2)),
            max_datagram: 1500,
            max_early_dtls: 4,
            max_queued: 64,
            media_timeout: Duration::from_secs(30),
            amplification_limit: 4,
            ice: IceConfig::default(),
            dtls: sapflow_dtls::Config::default(),
            srtp: sapflow_rtp::Config::default(),
            receiver: ReceiverConfig::default(),
        }
    }
}

impl SessionConfig {
    /// Checks every field's range.
    pub fn validate(&self) -> Result<(), SessionError> {
        let check = |ok: bool, what| {
            if ok {
                Ok(())
            } else {
                Err(SessionError::Config(what))
            }
        };
        check(
            (Duration::from_secs(1)..=Duration::from_secs(120)).contains(&self.handshake_timeout),
            "handshake_timeout",
        )?;
        check(
            self.keyframe_interval
                .is_none_or(|k| k >= Duration::from_millis(100)),
            "keyframe_interval",
        )?;
        check((256..=65535).contains(&self.max_datagram), "max_datagram")?;
        check(self.max_early_dtls <= 16, "max_early_dtls")?;
        check((16..=1024).contains(&self.max_queued), "max_queued")?;
        check(
            (1..=8).contains(&self.amplification_limit),
            "amplification_limit",
        )?;
        check(
            (Duration::from_secs(1)..=Duration::from_secs(300)).contains(&self.media_timeout),
            "media_timeout",
        )?;
        self.ice.validate().map_err(SessionError::Config)?;
        self.dtls
            .validate()
            .map_err(|e| SessionError::Config(e.0))?;
        // sapflow-rtp checks its config when a context is made.
        let probe = MasterKey::new(Profile::AeadAes128Gcm, &[0; 16], &[0; 12])
            .and_then(|k| Inbound::new(&k, self.srtp));
        check(probe.is_ok(), "srtp")?;
        Ok(())
    }
}

/// Why a session could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// A setting is out of range, named.
    Config(&'static str),
    /// The offer's streams do not fit the receiver.
    Streams(sapflow_rtp::receive::ConfigError),
    /// The system random source failed.
    Entropy,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WHIP session: {self:?}")
    }
}

impl std::error::Error for SessionError {}

impl From<EntropyError> for SessionError {
    fn from(_: EntropyError) -> Self {
        Self::Entropy
    }
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// ICE and DTLS did not complete within
    /// [`SessionConfig::handshake_timeout`].
    HandshakeTimeout,
    /// No new successful check from the send tuple for
    /// [`IceConfig::consent_timeout`] (RFC 7675).
    ConsentExpired,
    /// The DTLS handshake or association failed.
    Dtls(sapflow_dtls::Error),
    /// The publisher sent `close_notify`.
    PeerClosed,
    /// No fresh SRTP or SRTCP for [`SessionConfig::media_timeout`] once
    /// connected.
    MediaTimeout,
    /// [`Session::close`].
    Closed,
    /// The SRTCP index for our key is spent (2^31 packets).
    KeysExhausted,
}

/// Where a session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for a check with USE-CANDIDATE.
    Checking,
    /// A tuple is selected; the DTLS handshake is under way.
    Handshaking,
    /// SRTP is up and media flows.
    Connected,
    /// Ended; nothing more is sent.
    Ended(EndReason),
}

/// Something to act on.
#[derive(Debug)]
pub enum Event {
    /// The selected tuple was set or moved ([`Session::selected`]). Moves
    /// not yet polled are coalesced into one.
    Nominated,
    /// DTLS completed and SRTP is up, with this profile.
    Connected(sapflow_dtls::SrtpProfile),
    /// From the receiver: access units, audio, alignment, loss.
    Media(sapflow_rtp::Event),
    /// The session ended.
    Ended(EndReason),
}

/// A datagram to send. `Debug` leaves the address out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmit {
    /// Where.
    pub tuple: Tuple,
    /// The datagram, or the frame's payload on TCP.
    pub bytes: Vec<u8>,
}

/// Counters. None holds an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionStats {
    /// ICE counters.
    pub ice: IceStats,
    /// DTLS counters.
    pub dtls: sapflow_dtls::Stats,
    /// Datagrams taken.
    pub datagrams: u64,
    /// Datagrams over [`SessionConfig::max_datagram`].
    pub oversized: u64,
    /// Datagrams whose first byte is not STUN, DTLS, RTP or RTCP.
    pub other: u64,
    /// DTLS from a tuple that is not selected, not held.
    pub dtls_dropped: u64,
    /// DTLS held before nomination.
    pub dtls_held: u64,
    /// RTP or RTCP from a tuple that is not validated.
    pub media_unvalidated: u64,
    /// RTP or RTCP before SRTP keys.
    pub media_before_keys: u64,
    /// SRTP packets authenticated.
    pub srtp_packets: u64,
    /// SRTP packets refused (authentication, replay, too many streams).
    pub srtp_refused: u64,
    /// SRTCP packets authenticated.
    pub srtcp_packets: u64,
    /// SRTCP packets refused.
    pub srtcp_refused: u64,
    /// Authenticated packets that do not parse as RTP or RTCP.
    pub malformed: u64,
    /// RTP for a rejected section or an unknown MID.
    pub unmatched: u64,
    /// RTP the receiver refused (no stream, wrong payload type).
    pub refused: u64,
    /// SRTCP datagrams sent.
    pub feedback_sent: u64,
    /// Output dropped because the queue was full.
    pub transmit_dropped: u64,
    /// Output to an unproven tuple dropped by the amplification limit.
    pub budget_dropped: u64,
    /// Input after the session ended.
    pub after_end: u64,
}

/// SSRC to stream, from the offer and from MID matching.
#[derive(Debug)]
struct Routes {
    mid_extension: Option<u8>,
    /// Accepted sections: MID, stream, media and RTX payload types.
    accepted: Vec<(String, StreamId, u8, Option<u8>)>,
    rejected_mids: Vec<String>,
    /// SSRC and its stream, or `None` for a rejected section.
    known: Vec<(u32, Option<StreamId>)>,
}

impl Routes {
    /// Whether a packet may go to the receiver.
    fn allow(&mut self, p: &RtpPacket<'_>) -> bool {
        let ssrc = p.ssrc();
        if let Some((_, route)) = self.known.iter().find(|(s, _)| *s == ssrc) {
            return route.is_some();
        }
        let Some(mid) = self.mid_extension.and_then(|id| p.extension_element(id)) else {
            // No MID: the receiver binds by payload type.
            return true;
        };
        let pt = p.payload_type();
        if let Some(&(_, stream, media, rtx)) =
            self.accepted.iter().find(|(m, ..)| m.as_bytes() == mid)
        {
            let ok = pt == media || rtx == Some(pt);
            if ok {
                self.remember(ssrc, Some(stream));
            }
            return ok;
        }
        if self.rejected_mids.iter().any(|m| m.as_bytes() == mid) {
            self.remember(ssrc, None);
        }
        false
    }

    fn remember(&mut self, ssrc: u32, route: Option<StreamId>) {
        if self.known.len() < MAX_ROUTES {
            self.known.push((ssrc, route));
        }
    }
}

/// What has passed between the session and one validated tuple.
#[derive(Debug, Clone, Copy)]
struct Path {
    tuple: Tuple,
    received: u64,
    sent: u64,
    /// Fresh authenticated SRTP or SRTCP has arrived from it.
    proven: bool,
}

/// SRTP and the receiver, from the moment DTLS completes.
struct Media {
    profile: sapflow_dtls::SrtpProfile,
    inbound: Inbound,
    outbound: Outbound,
    rx: Receiver,
    routes: Routes,
    overhead: usize,
}

/// One WHIP session.
pub struct Session {
    config: SessionConfig,
    created: Instant,
    state: State,
    ufrag: String,
    ice: Agent,
    dtls: Server,
    negotiated: Negotiated,
    media: Option<Box<Media>>,
    /// Where DTLS and RTCP go.
    send_to: Option<Tuple>,
    /// When SRTP came up, then when fresh SRTP or SRTCP last arrived.
    last_srtp: Option<Instant>,
    paths: Vec<Path>,
    early: Vec<(Tuple, Vec<u8>)>,
    out: VecDeque<Transmit>,
    events: VecDeque<Event>,
    last_media: Option<Instant>,
    our_ssrc: u32,
    cname: Vec<u8>,
    stats: SessionStats,
    scratch: Vec<u8>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("selected", &self.ice.selected().is_some())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// A session for an admitted offer: our `credentials` (the ones in the
    /// answer), the process `identity`, and what [`crate::sdp::answer`]
    /// accepted. `now` starts the handshake deadline.
    pub fn new(
        config: SessionConfig,
        identity: Arc<Identity>,
        credentials: Credentials,
        negotiated: Negotiated,
        now: Instant,
    ) -> Result<Self, SessionError> {
        config.validate()?;
        let dtls = Server::new(config.dtls.clone(), identity, &negotiated.fingerprints)
            .map_err(|e| SessionError::Config(e.0))?;
        Self::build(
            config,
            dtls,
            credentials,
            negotiated,
            now,
            &mut Entropy::System,
        )
    }

    /// As [`Session::new`], with randomness from a seed (ours and the DTLS
    /// server's), so that a run replays byte for byte. Not for production
    /// use.
    #[cfg(any(test, feature = "testing"))]
    pub fn new_seeded(
        config: SessionConfig,
        identity: Arc<Identity>,
        credentials: Credentials,
        negotiated: Negotiated,
        now: Instant,
        seed: [u8; 32],
    ) -> Result<Self, SessionError> {
        config.validate()?;
        let mut entropy = Entropy::seeded(seed);
        let dtls_seed = *entropy.array::<32>()?;
        let dtls = Server::new_seeded(
            config.dtls.clone(),
            identity,
            &negotiated.fingerprints,
            dtls_seed,
        )
        .map_err(|e| SessionError::Config(e.0))?;
        Self::build(config, dtls, credentials, negotiated, now, &mut entropy)
    }

    fn build(
        config: SessionConfig,
        dtls: Server,
        credentials: Credentials,
        negotiated: Negotiated,
        now: Instant,
        entropy: &mut Entropy,
    ) -> Result<Self, SessionError> {
        let theirs: Vec<u32> = negotiated
            .streams()
            .flat_map(|s| [s.ssrc, s.rtx_ssrc])
            .flatten()
            .chain(negotiated.rejected_ssrcs.iter().copied())
            .collect();
        let mut our_ssrc = entropy.u32()?;
        while our_ssrc == 0 || theirs.contains(&our_ssrc) {
            our_ssrc = entropy.u32()?;
        }
        let cname = entropy.string(ALPHANUMERIC, 16)?.into_bytes();
        // The receiver is made when SRTP comes up; check now that the
        // streams fit it, so a bad answer fails here and not later.
        probe_streams(&config, &negotiated, now)?;
        Ok(Self {
            ice: Agent::new(config.ice, &credentials, &negotiated.remote_ufrag),
            ufrag: credentials.ufrag().to_owned(),
            config,
            created: now,
            state: State::Checking,
            dtls,
            negotiated,
            media: None,
            send_to: None,
            last_srtp: None,
            paths: Vec::new(),
            early: Vec::new(),
            out: VecDeque::new(),
            events: VecDeque::new(),
            last_media: None,
            our_ssrc,
            cname,
            stats: SessionStats::default(),
            scratch: Vec::with_capacity(1500),
        })
    }

    /// Where the session stands.
    pub fn state(&self) -> State {
        self.state
    }

    /// Our ufrag: the first half of every check's USERNAME, by which the
    /// UDP owner finds the session for a check from a new address.
    pub fn local_ufrag(&self) -> &str {
        &self.ufrag
    }

    /// The selected tuple.
    pub fn selected(&self) -> Option<Tuple> {
        self.ice.selected()
    }

    /// Where DTLS and RTCP go: the selected tuple, unless the selection has
    /// moved from a proven tuple to one that is not proven yet.
    pub fn send_tuple(&self) -> Option<Tuple> {
        self.send_to
    }

    /// Whether a check from this tuple succeeded: the UDP owner may then
    /// route its datagrams here.
    pub fn accepts(&self, tuple: &Tuple) -> bool {
        self.ice.is_validated(tuple)
    }

    /// When the last authenticated RTP packet that the receiver took as
    /// new media arrived (not a duplicate, padding or probe), for
    /// superseding a stalled session (`SAPFLOW_WHIP_STALE`).
    pub fn last_media(&self) -> Option<Instant> {
        self.last_media
    }

    /// The SRTP profile, once connected.
    pub fn profile(&self) -> Option<sapflow_dtls::SrtpProfile> {
        self.media.as_ref().map(|m| m.profile)
    }

    /// What the answer accepted.
    pub fn negotiated(&self) -> &Negotiated {
        &self.negotiated
    }

    /// The receiver's ID for the accepted audio or video stream, once
    /// connected.
    pub fn stream(&self, kind: MediaKind) -> Option<StreamId> {
        let mid = match kind {
            MediaKind::Video => &self.negotiated.video.as_ref()?.mid,
            MediaKind::Audio => &self.negotiated.audio.as_ref()?.mid,
            _ => return None,
        };
        let m = self.media.as_ref()?;
        m.routes
            .accepted
            .iter()
            .find(|(x, ..)| x == mid)
            .map(|&(_, id, ..)| id)
    }

    /// The receiver, once connected, for its per-stream counters.
    pub fn receiver(&self) -> Option<&Receiver> {
        self.media.as_ref().map(|m| &m.rx)
    }

    /// Counters.
    pub fn stats(&self) -> SessionStats {
        let mut s = self.stats;
        s.ice = self.ice.stats();
        s.dtls = self.dtls.stats();
        s
    }

    /// The next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.out.pop_front()
    }

    /// The next event.
    pub fn poll_event(&mut self) -> Option<Event> {
        if let Some(e) = self.events.pop_front() {
            return Some(e);
        }
        self.media
            .as_mut()
            .and_then(|m| m.rx.poll_event())
            .map(Event::Media)
    }

    /// When [`Session::handle_timeout`] should next be called.
    pub fn poll_timeout(&self) -> Option<Instant> {
        if matches!(self.state, State::Ended(_)) {
            return None;
        }
        let deadline = (self.state != State::Connected)
            .then(|| self.created.checked_add(self.config.handshake_timeout))
            .flatten();
        [
            deadline,
            self.consent_expires(),
            self.media_expires(),
            self.dtls.poll_timeout(),
            self.media.as_ref().and_then(|m| m.rx.poll_timeout()),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Runs whatever is due at `now`: the handshake deadline, consent,
    /// DTLS retransmission and the receiver's timers.
    pub fn handle_timeout(&mut self, now: Instant) {
        if self.expire(now) {
            return;
        }
        if self.dtls.poll_timeout().is_some_and(|t| t <= now) {
            let r = self.dtls.handle_timeout(now);
            self.after_dtls(now, r);
        }
        if let Some(m) = self.media.as_mut() {
            m.rx.handle_timeout(now);
        }
        self.flush();
    }

    /// Ends the session: `close_notify` to the selected tuple if DTLS is
    /// up. Idempotent.
    pub fn close(&mut self) {
        if matches!(self.state, State::Ended(_)) {
            return;
        }
        self.dtls.close();
        self.drain_dtls();
        self.end(EndReason::Closed);
    }

    /// One datagram (or RFC 4571 frame) from `tuple`.
    pub fn handle_datagram(&mut self, now: Instant, tuple: Tuple, bytes: &[u8]) {
        if matches!(self.state, State::Ended(_)) {
            self.stats.after_end += 1;
            return;
        }
        if self.expire(now) {
            self.stats.after_end += 1;
            return;
        }
        if bytes.len() > self.config.max_datagram {
            self.stats.oversized += 1;
            return;
        }
        self.stats.datagrams += 1;
        // Bytes from a validated tuple raise what the session may send it;
        // a check that validates its tuple counts once it has.
        let counted = self.received(tuple, bytes.len());
        match classify(bytes) {
            Kind::Stun => self.stun(now, tuple, bytes, counted),
            Kind::Dtls => self.dtls_in(now, tuple, bytes),
            Kind::Rtp | Kind::Rtcp if !self.ice.is_validated(&tuple) => {
                self.stats.media_unvalidated += 1;
            }
            Kind::Rtp => self.rtp(now, tuple, bytes),
            Kind::Rtcp => self.rtcp(now, tuple, bytes),
            Kind::Other => self.stats.other += 1,
        }
        self.flush();
    }

    /// Counts bytes from a validated tuple. Returns whether it did.
    fn received(&mut self, tuple: Tuple, len: usize) -> bool {
        if !self.ice.is_validated(&tuple) {
            return false;
        }
        let len = len as u64;
        if let Some(p) = self.paths.iter_mut().find(|p| p.tuple == tuple) {
            p.received = p.received.saturating_add(len);
            return true;
        }
        // One path per validated tuple: forget those the agent forgot.
        let ice = &self.ice;
        self.paths.retain(|p| ice.is_validated(&p.tuple));
        self.paths.push(Path {
            tuple,
            received: len,
            sent: 0,
            proven: false,
        });
        true
    }

    fn proven(&self, tuple: Tuple) -> bool {
        self.paths.iter().any(|p| p.tuple == tuple && p.proven)
    }

    /// Fresh authenticated SRTP or SRTCP arrived from `tuple`.
    fn prove(&mut self, now: Instant, tuple: Tuple) {
        self.last_srtp = Some(now);
        if let Some(p) = self.paths.iter_mut().find(|p| p.tuple == tuple) {
            p.proven = true;
        }
        self.retarget();
    }

    /// Points the send tuple at the selected tuple, unless that would move
    /// it from a proven tuple to one that is not proven.
    fn retarget(&mut self) {
        let Some(selected) = self.ice.selected() else {
            return;
        };
        if self.send_to == Some(selected) {
            return;
        }
        let keep = self.send_to.is_some_and(|t| self.proven(t)) && !self.proven(selected);
        if !keep {
            self.send_to = Some(selected);
            self.ice.protect(self.send_to);
        }
    }

    /// When consent for the send tuple runs out (RFC 7675: consent is for
    /// sending).
    fn consent_expires(&self) -> Option<Instant> {
        self.send_to.and_then(|t| self.ice.consent_expires(&t))
    }

    /// When the media timeout runs out, once connected.
    fn media_expires(&self) -> Option<Instant> {
        self.last_srtp
            .and_then(|t| t.checked_add(self.config.media_timeout))
    }

    /// Ends the session if a deadline has passed. Returns whether it has
    /// ended.
    fn expire(&mut self, now: Instant) -> bool {
        if matches!(self.state, State::Ended(_)) {
            return true;
        }
        let handshake = self.created.checked_add(self.config.handshake_timeout);
        if self.state != State::Connected && handshake.is_some_and(|d| now >= d) {
            self.end(EndReason::HandshakeTimeout);
            return true;
        }
        if self.consent_expires().is_some_and(|d| now >= d) {
            self.end(EndReason::ConsentExpired);
            return true;
        }
        if self.media_expires().is_some_and(|d| now >= d) {
            self.end(EndReason::MediaTimeout);
            return true;
        }
        false
    }

    fn end(&mut self, reason: EndReason) {
        if matches!(self.state, State::Ended(_)) {
            return;
        }
        // Media already released keeps its order before the end.
        if let Some(mut m) = self.media.take() {
            while let Some(e) = m.rx.poll_event() {
                self.events.push_back(Event::Media(e));
            }
        }
        self.early.clear();
        self.state = State::Ended(reason);
        self.events.push_back(Event::Ended(reason));
    }

    /// Queues a session event. None is dropped: `Connected` and `Ended`
    /// come once each, and a `Nominated` already waiting stands for any
    /// later move, so the queue stays bounded (media events wait in the
    /// receiver, within its own bound).
    fn push_event(&mut self, e: Event) {
        let waiting = |q: &VecDeque<Event>| q.iter().any(|x| matches!(x, Event::Nominated));
        if matches!(e, Event::Nominated) && waiting(&self.events) {
            return;
        }
        self.events.push_back(e);
    }

    /// Queues a datagram for `tuple`. A STUN `response` is never larger
    /// than its request and always goes; anything else to a tuple not
    /// proven goes only within the amplification limit. Returns whether it
    /// was queued.
    fn send(&mut self, tuple: Tuple, bytes: Vec<u8>, response: bool) -> bool {
        if self.out.len() >= self.config.max_queued {
            self.stats.transmit_dropped += 1;
            return false;
        }
        let len = bytes.len() as u64;
        let limit = u64::from(self.config.amplification_limit);
        match self.paths.iter_mut().find(|p| p.tuple == tuple) {
            Some(p)
                if p.proven
                    || response
                    || p.sent.saturating_add(len) <= p.received.saturating_mul(limit) =>
            {
                p.sent = p.sent.saturating_add(len);
            }
            None if response => {}
            _ => {
                self.stats.budget_dropped += 1;
                return false;
            }
        }
        self.out.push_back(Transmit { tuple, bytes });
        true
    }

    fn stun(&mut self, now: Instant, tuple: Tuple, bytes: &[u8], counted: bool) {
        let Some(answer) = self.ice.handle(now, tuple, bytes) else {
            return;
        };
        if !counted && answer.success {
            self.received(tuple, bytes.len());
        }
        self.send(tuple, answer.response, true);
        if !answer.nominated {
            return;
        }
        self.retarget();
        self.push_event(Event::Nominated);
        if self.state == State::Checking {
            self.state = State::Handshaking;
        }
        // DTLS that arrived from this tuple before it was nominated.
        let early = std::mem::take(&mut self.early);
        for (t, d) in early {
            if t == tuple && !matches!(self.state, State::Ended(_)) {
                let r = self.dtls.handle_datagram(now, &d);
                self.after_dtls(now, r);
            }
        }
    }

    fn dtls_in(&mut self, now: Instant, tuple: Tuple, bytes: &[u8]) {
        if self.ice.selected() == Some(tuple) {
            let r = self.dtls.handle_datagram(now, bytes);
            self.after_dtls(now, r);
        } else if self.state == State::Checking
            && self.ice.is_validated(&tuple)
            && self.early.len() < self.config.max_early_dtls
        {
            self.early.push((tuple, bytes.to_vec()));
            self.stats.dtls_held += 1;
        } else {
            self.stats.dtls_dropped += 1;
        }
    }

    fn drain_dtls(&mut self) {
        let Some(to) = self.send_to else {
            // Nothing is sent before a tuple is selected; DTLS has nothing
            // to send then either.
            while self.dtls.poll_transmit().is_some() {}
            return;
        };
        while let Some(d) = self.dtls.poll_transmit() {
            self.send(to, d, false);
        }
    }

    fn after_dtls(&mut self, now: Instant, result: Result<(), sapflow_dtls::Error>) {
        self.drain_dtls();
        if let Err(e) = result {
            self.end(EndReason::Dtls(e));
            return;
        }
        while let Some(e) = self.dtls.poll_event() {
            match e {
                sapflow_dtls::Event::Connected(c) => self.connect(now, c),
                sapflow_dtls::Event::Closed => {
                    self.drain_dtls();
                    self.end(EndReason::PeerClosed);
                }
            }
        }
    }

    fn connect(&mut self, now: Instant, c: sapflow_dtls::Connected) {
        if self.media.is_some() || matches!(self.state, State::Ended(_)) {
            return;
        }
        let made = Profile::from_dtls_id(c.profile.id())
            .ok_or(SrtpError::Config)
            .and_then(|p| split_keying_material(p, Role::Server, c.keying_material.as_bytes()))
            .and_then(|keys| {
                Ok((
                    Inbound::new(&keys.remote, self.config.srtp)?,
                    Outbound::new(&keys.local)?,
                ))
            });
        let Ok((inbound, outbound)) = made else {
            // The profile and lengths come from sapflow-dtls, so this does
            // not happen; refuse rather than run without keys.
            self.end(EndReason::Dtls(sapflow_dtls::Error::Internal));
            return;
        };
        let Ok((rx, routes)) = receiver(&self.config, &self.negotiated, now) else {
            self.end(EndReason::Dtls(sapflow_dtls::Error::Internal));
            return;
        };
        let overhead = outbound.profile().rtcp_overhead();
        let mut media = Box::new(Media {
            profile: c.profile,
            inbound,
            outbound,
            rx,
            routes,
            overhead,
        });
        // The first keyframe at once: a PLI as soon as SRTP is up. Only a
        // video stream takes it.
        for &(_, id, ..) in &media.routes.accepted {
            media.rx.request_keyframe(id);
        }
        media.rx.handle_timeout(now);
        self.media = Some(media);
        self.last_srtp = Some(now);
        self.state = State::Connected;
        self.push_event(Event::Connected(c.profile));
    }

    fn rtp(&mut self, now: Instant, tuple: Tuple, bytes: &[u8]) {
        let Some(m) = self.media.as_mut() else {
            self.stats.media_before_keys += 1;
            return;
        };
        self.scratch.clear();
        self.scratch.extend_from_slice(bytes);
        let Ok(len) = m.inbound.unprotect_rtp(&mut self.scratch) else {
            self.stats.srtp_refused += 1;
            return;
        };
        self.stats.srtp_packets += 1;
        // Authenticated and inside the replay window: this tuple is the
        // publisher's.
        self.prove(now, tuple);
        let Some(m) = self.media.as_mut() else {
            return;
        };
        let Some(Ok(packet)) = self.scratch.get(..len).map(RtpPacket::parse) else {
            self.stats.malformed += 1;
            return;
        };
        if !m.routes.allow(&packet) {
            self.stats.unmatched += 1;
            return;
        }
        match m.rx.handle_rtp(now, &packet) {
            Ok((_, Received::New | Received::Recovered | Received::Restart)) => {
                self.last_media = Some(now);
            }
            Ok(_) => {}
            Err(_) => self.stats.refused += 1,
        }
    }

    fn rtcp(&mut self, now: Instant, tuple: Tuple, bytes: &[u8]) {
        let Some(m) = self.media.as_mut() else {
            self.stats.media_before_keys += 1;
            return;
        };
        self.scratch.clear();
        self.scratch.extend_from_slice(bytes);
        let Ok(len) = m.inbound.unprotect_rtcp(&mut self.scratch) else {
            self.stats.srtcp_refused += 1;
            return;
        };
        self.stats.srtcp_packets += 1;
        self.prove(now, tuple);
        let Some(m) = self.media.as_mut() else {
            return;
        };
        match self.scratch.get(..len).map(Compound::parse) {
            Some(Ok(c)) => {
                m.rx.handle_rtcp(now, &c);
            }
            _ => self.stats.malformed += 1,
        }
    }

    /// Sends the receiver's RTCP through SRTCP to the send tuple.
    fn flush(&mut self) {
        let Some(to) = self.send_to else {
            return;
        };
        let mut protected = Vec::new();
        let mut exhausted = false;
        if let Some(m) = self.media.as_mut() {
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if !m.rx.poll_feedback(self.our_ssrc, &self.cname, &mut buf) {
                    break;
                }
                let len = buf.len();
                buf.resize(len + m.overhead, 0);
                match m.outbound.protect_rtcp(&mut buf, len) {
                    Ok(n) => {
                        buf.truncate(n);
                        protected.push(buf.clone());
                    }
                    Err(_) => {
                        exhausted = true;
                        break;
                    }
                }
            }
        }
        for d in protected {
            if self.send(to, d, false) {
                self.stats.feedback_sent += 1;
            }
        }
        if exhausted {
            self.end(EndReason::KeysExhausted);
        }
    }
}

fn stream_config(s: &Stream) -> StreamConfig {
    let codec = match s.kind {
        MediaKind::Video => Codec::H264,
        _ => Codec::Opus,
    };
    let mut c = StreamConfig::new(codec, s.payload_type);
    c.ssrc = s.ssrc;
    c.rtx = s.rtx_payload_type.map(|payload_type| RtxConfig {
        payload_type,
        ssrc: s.rtx_ssrc,
    });
    c.nack = s.nack;
    c.pli = s.pli;
    c.fir = s.fir;
    c
}

/// The receiver and routes for a session's streams.
fn receiver(
    config: &SessionConfig,
    n: &Negotiated,
    now: Instant,
) -> Result<(Receiver, Routes), SessionError> {
    let mut rc = config.receiver;
    rc.keyframe_interval = config.keyframe_interval;
    rc.reports.reduced_size = false;
    rc.transport_cc = n
        .transport_cc_extension
        .map(|id| match config.receiver.transport_cc {
            Some(t) => TransportCcConfig {
                extension_id: id,
                ..t
            },
            None => TransportCcConfig::new(id),
        });
    let mut rx = Receiver::new(rc, now);
    let mut routes = Routes {
        mid_extension: n.mid_extension,
        accepted: Vec::new(),
        rejected_mids: n.rejected_mids.clone(),
        known: n
            .rejected_ssrcs
            .iter()
            .map(|s| (*s, None))
            .take(MAX_ROUTES)
            .collect(),
    };
    for s in n.streams() {
        let id = rx
            .add_stream(stream_config(s))
            .map_err(SessionError::Streams)?;
        routes
            .accepted
            .push((s.mid.clone(), id, s.payload_type, s.rtx_payload_type));
        // The offer's SSRCs come before the MID: matched by SSRC, a packet
        // goes to the receiver whatever MID it carries.
        for ssrc in [s.ssrc, s.rtx_ssrc].into_iter().flatten() {
            routes.known.insert(0, (ssrc, Some(id)));
        }
    }
    routes.known.truncate(MAX_ROUTES);
    Ok((rx, routes))
}

fn probe_streams(config: &SessionConfig, n: &Negotiated, now: Instant) -> Result<(), SessionError> {
    receiver(config, n, now).map(|_| ())
}
