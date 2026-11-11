//! Choosing what to accept from an offer, and the answer that says so.
//!
//! - **Sections.** The first video and the first audio section that send
//!   (`sendonly` or `sendrecv`), are enabled (a port, or `a=bundle-only`)
//!   and are in the BUNDLE group are accepted and answered `recvonly`.
//!   Every other section is answered with port 0 and left out of the group.
//! - **Payload types** 64 to 95 are never chosen: under `rtcp-mux` they
//!   would read as RTCP (RFC 5761 section 4).
//! - **Video.** The first H.264 payload in offer order with
//!   `packetization-mode=1` and a Constrained Baseline, Baseline, Main or
//!   High `profile-level-id` (RFC 6184 section 8.1, Table 5), its `fmtp`
//!   repeated, and the RTX payload whose `apt` names it (RFC 4588). Of the
//!   feedback the offer lists for that payload, `nack`, `nack pli`,
//!   `ccm fir` and `transport-cc` are kept.
//! - **Audio.** `opus/48000/2`, answered `stereo=1`,
//!   `maxaveragebitrate=<configured>` and `usedtx=0` (RFC 7587 section
//!   7), and `transport-cc` if offered for it.
//! - **Header extensions.** Only `sdes:mid` and, where transport-cc is
//!   used, the transport-wide sequence number, with the offer's IDs, which
//!   must agree across the bundle.
//! - **Transport.** `a=ice-lite`, our credentials, the configured
//!   candidates in the answer's tagged section, `a=setup:passive` and our
//!   fingerprint. The offer must give ICE credentials, one to four
//!   `sha-256` fingerprints (all passed to `sapflow-dtls`), `actpass` or
//!   `active`, and `rtcp-mux`, the same for every accepted section.

use super::{Direction, Extmap, MediaKind, Offer, ProfileError, Section, Setup, fmtp_param};
use crate::ice::{Credentials, Transport};
use sapflow_dtls::{Fingerprint, MAX_EXPECTED_FINGERPRINTS};
use std::fmt;
use std::fmt::Write as _;
use std::net::SocketAddr;

/// The MID header extension (RFC 8843 section 15.2).
pub const MID_URI: &str = "urn:ietf:params:rtp-hdrext:sdes:mid";
/// The transport-wide sequence number extension
/// (draft-holmer-rmcat-transport-wide-cc-extensions-01).
pub const TRANSPORT_CC_URI: &str =
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";

/// Lowest and highest Opus `maxaveragebitrate` (RFC 7587 section 6.1).
const OPUS_BITRATE: (u32, u32) = (6000, 510_000);
/// Most candidates put in an answer.
pub const MAX_CANDIDATES: usize = 8;

/// An advertised candidate (`SAPFLOW_WHIP_CANDIDATES`): a host candidate
/// on UDP, or a passive ICE-TCP one (RFC 6544). `Debug` leaves the
/// address out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// UDP, or passive TCP.
    pub transport: Transport,
    /// The address publishers reach.
    pub address: SocketAddr,
}

impl fmt::Debug for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Candidate({:?}, <address>)", self.transport)
    }
}

/// What the answer is built from.
#[derive(Debug, Clone, Copy)]
pub struct AnswerConfig<'a> {
    /// Our ICE credentials for this session.
    pub credentials: &'a Credentials,
    /// Our certificate's fingerprint.
    pub fingerprint: Fingerprint,
    /// Advertised candidates, at most [`MAX_CANDIDATES`] (more are left
    /// out).
    pub candidates: &'a [Candidate],
    /// Opus `maxaveragebitrate` (`SAPFLOW_WHIP_OPUS_MAX_BITRATE`), held to
    /// 6000 to 510000.
    pub opus_max_bitrate: u32,
    /// The session ID for the `o=` line.
    pub origin: u64,
}

/// An H.264 profile the answer accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Profile {
    /// Constrained Baseline.
    ConstrainedBaseline,
    /// Baseline.
    Baseline,
    /// Main.
    Main,
    /// High, including Constrained and Progressive High.
    High,
}

impl H264Profile {
    /// Reads a `profile-level-id` (RFC 6184 section 8.1 and Table 5), or
    /// `None` for a profile outside the four, or one that does not parse.
    pub fn classify(profile_level_id: &str) -> Option<Self> {
        let hex = profile_level_id.as_bytes();
        if hex.len() != 6 {
            return None;
        }
        let byte = |i: usize| {
            let s = std::str::from_utf8(hex.get(i..i + 2)?).ok()?;
            u8::from_str_radix(s, 16).ok()
        };
        let (idc, iop) = (byte(0)?, byte(2)?);
        byte(4)?;
        Some(match idc {
            0x42 if iop & 0x40 != 0 => Self::ConstrainedBaseline,
            0x42 => Self::Baseline,
            0x4d if iop & 0x80 != 0 => Self::ConstrainedBaseline,
            0x4d => Self::Main,
            0x58 if iop & 0xc0 == 0xc0 => Self::ConstrainedBaseline,
            0x58 if iop & 0xc0 == 0x80 => Self::Baseline,
            0x64 => Self::High,
            _ => return None,
        })
    }
}

/// One accepted stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// Audio or video.
    pub kind: MediaKind,
    /// Its section's MID.
    pub mid: String,
    /// The media payload type.
    pub payload_type: u8,
    /// The RTX payload type, if accepted.
    pub rtx_payload_type: Option<u8>,
    /// The media SSRC, if the offer named one (from `a=ssrc-group:FID`, or
    /// a section with a single `a=ssrc`).
    pub ssrc: Option<u32>,
    /// The RTX SSRC from `a=ssrc-group:FID`, if RTX was accepted.
    pub rtx_ssrc: Option<u32>,
    /// `nack` negotiated.
    pub nack: bool,
    /// `nack pli` negotiated.
    pub pli: bool,
    /// `ccm fir` negotiated.
    pub fir: bool,
    /// `transport-cc` negotiated for this section.
    pub transport_cc: bool,
    /// The H.264 profile, for video.
    pub profile: Option<H264Profile>,
}

/// What the answer accepted: everything the session needs, and nothing
/// that identifies the publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    /// The offer's ICE ufrag, the second half of a check's USERNAME.
    pub remote_ufrag: String,
    /// The offer's `sha-256` fingerprints, one to four.
    pub fingerprints: Vec<Fingerprint>,
    /// The offer's DTLS role.
    pub setup: Setup,
    /// The video stream, if accepted.
    pub video: Option<Stream>,
    /// The audio stream, if accepted.
    pub audio: Option<Stream>,
    /// The MID header extension's ID, if negotiated.
    pub mid_extension: Option<u8>,
    /// The transport-wide sequence number extension's ID, if transport-cc
    /// was negotiated for any section.
    pub transport_cc_extension: Option<u8>,
    /// MIDs of the sections answered with port 0.
    pub rejected_mids: Vec<String>,
    /// SSRCs declared in sections answered with port 0.
    pub rejected_ssrcs: Vec<u32>,
}

impl Negotiated {
    /// The accepted streams, video first.
    pub fn streams(&self) -> impl Iterator<Item = &Stream> {
        self.video.iter().chain(self.audio.iter())
    }
}

/// An extension's direction is usable for receiving, and the suffix the
/// answer gives it.
fn receivable(e: &Extmap) -> Option<&'static str> {
    match e.direction {
        None | Some(Direction::SendRecv) => Some(""),
        Some(Direction::SendOnly) => Some("/recvonly"),
        Some(Direction::RecvOnly | Direction::Inactive) => None,
    }
}

/// Whether a payload type can share the port with RTCP. RFC 5761 section
/// 4: under `rtcp-mux`, 64 to 95 would read as RTCP packet types 192 to
/// 223, so the first-byte demultiplexer would never see them as RTP.
fn muxable(pt: u8) -> bool {
    !(64..=95).contains(&pt)
}

fn choose_h264(s: &Section) -> Option<(u8, H264Profile)> {
    s.payload_types().filter(|&pt| muxable(pt)).find_map(|pt| {
        let r = s.rtpmap(pt)?;
        if !r.encoding.eq_ignore_ascii_case("H264") || r.clock_rate != 90_000 {
            return None;
        }
        let params = s.fmtp(pt).unwrap_or_default();
        if fmtp_param(params, "packetization-mode") != Some("1") {
            return None;
        }
        // RFC 6184 section 8.1: absent means 420010, Baseline.
        let profile = match fmtp_param(params, "profile-level-id") {
            None => Some(H264Profile::Baseline),
            Some(p) => H264Profile::classify(p),
        }?;
        Some((pt, profile))
    })
}

fn choose_rtx(s: &Section, apt: u8) -> Option<u8> {
    s.payload_types().filter(|&pt| muxable(pt)).find(|&pt| {
        s.rtpmap(pt)
            .is_some_and(|r| r.encoding.eq_ignore_ascii_case("rtx") && r.clock_rate == 90_000)
            && s.fmtp(pt)
                .and_then(|p| fmtp_param(p, "apt"))
                .and_then(|a| a.parse::<u8>().ok())
                == Some(apt)
    })
}

fn choose_opus(s: &Section) -> Option<u8> {
    s.payload_types().filter(|&pt| muxable(pt)).find(|&pt| {
        s.rtpmap(pt).is_some_and(|r| {
            r.encoding.eq_ignore_ascii_case("opus")
                && r.clock_rate == 48_000
                && r.channels == Some(2)
        })
    })
}

/// The media and RTX SSRCs the offer names for a section.
fn ssrcs(s: &Section, rtx: bool) -> (Option<u32>, Option<u32>) {
    match (s.fid.first(), s.ssrcs.as_slice()) {
        (Some(&(media, r)), _) => (Some(media), rtx.then_some(r)),
        (None, [one]) => (Some(*one), None),
        // Several SSRCs and no FID group: bind by MID or payload type.
        _ => (None, None),
    }
}

struct Chosen<'a> {
    section: &'a Section,
    stream: Stream,
    mid_ext: Option<&'a Extmap>,
    twcc_ext: Option<&'a Extmap>,
}

fn choose<'a>(s: &'a Section) -> Result<Chosen<'a>, ProfileError> {
    if s.simulcast {
        return Err(ProfileError::Simulcast);
    }
    let (pt, rtx, profile) = match s.kind {
        MediaKind::Video => {
            let (pt, profile) = choose_h264(s).ok_or(ProfileError::NoH264)?;
            (pt, choose_rtx(s, pt), Some(profile))
        }
        _ => (choose_opus(s).ok_or(ProfileError::NoOpus)?, None, None),
    };
    let video = s.kind == MediaKind::Video;
    let mid_ext = s.extmap(MID_URI).filter(|e| receivable(e).is_some());
    let twcc_ext = s
        .extmap(TRANSPORT_CC_URI)
        .filter(|e| receivable(e).is_some() && s.has_feedback(pt, "transport-cc"));
    let (ssrc, rtx_ssrc) = ssrcs(s, rtx.is_some());
    let stream = Stream {
        kind: s.kind,
        mid: s.mid.clone().unwrap_or_default(),
        payload_type: pt,
        rtx_payload_type: rtx,
        ssrc,
        rtx_ssrc,
        nack: video && s.has_feedback(pt, "nack"),
        pli: video && s.has_feedback(pt, "nack pli"),
        fir: video && s.has_feedback(pt, "ccm fir"),
        transport_cc: twcc_ext.is_some(),
        profile,
    };
    Ok(Chosen {
        section: s,
        stream,
        mid_ext,
        twcc_ext,
    })
}

/// The ID every accepted section that uses an extension agrees on, after
/// checking no section gives that ID to another extension.
fn agreed_id<'a>(
    chosen: &[Chosen<'a>],
    pick: impl Fn(&Chosen<'a>) -> Option<&'a Extmap>,
) -> Result<Option<u8>, ProfileError> {
    let mut id = None;
    for c in chosen {
        if let Some(e) = pick(c) {
            match id {
                None => id = Some(e.id),
                Some(i) if i != e.id => return Err(ProfileError::InconsistentExtensions),
                Some(_) => {}
            }
        }
    }
    if let Some(i) = id {
        let uri = chosen
            .iter()
            .find_map(|c| pick(c).map(|e| e.uri.as_str()))
            .unwrap_or_default();
        for c in chosen {
            if c.section.extmaps.iter().any(|e| e.id == i && e.uri != uri) {
                return Err(ProfileError::InconsistentExtensions);
            }
        }
    }
    Ok(id)
}

/// Accepts what the profile allows from `offer` and builds the answer, or
/// says why not.
pub fn answer(
    offer: &Offer,
    config: &AnswerConfig<'_>,
) -> Result<(String, Negotiated), ProfileError> {
    if offer.ice_lite {
        return Err(ProfileError::PeerIceLite);
    }
    let bundle = match offer.bundles.as_slice() {
        [] => return Err(ProfileError::NoBundle),
        [one] => one,
        _ => return Err(ProfileError::SeveralBundles),
    };
    let mut chosen: Vec<Chosen<'_>> = Vec::new();
    for s in &offer.sections {
        let eligible = matches!(s.kind, MediaKind::Video | MediaKind::Audio)
            && (s.port != 0 || s.bundle_only)
            && matches!(
                offer.direction(s),
                Direction::SendOnly | Direction::SendRecv
            )
            && s.mid.as_ref().is_some_and(|m| bundle.contains(m))
            && !chosen.iter().any(|c| c.section.kind == s.kind);
        if eligible {
            chosen.push(choose(s)?);
        }
    }
    let first = chosen.first().ok_or(ProfileError::NoMedia)?;

    // The bundled transport, which every accepted section must agree on.
    let transport = |s: &Section| -> Result<(String, Vec<Fingerprint>, Setup), ProfileError> {
        let ufrag = offer.ice_ufrag(s).ok_or(ProfileError::NoIceCredentials)?;
        if !offer.has_ice_pwd(s) {
            return Err(ProfileError::NoIceCredentials);
        }
        let (fps, _) = offer.fingerprints(s);
        if fps.is_empty() {
            return Err(ProfileError::NoFingerprint);
        }
        if fps.len() > MAX_EXPECTED_FINGERPRINTS {
            return Err(ProfileError::TooManyFingerprints);
        }
        let setup = offer.setup(s).ok_or(ProfileError::NoSetup)?;
        if !matches!(setup, Setup::ActPass | Setup::Active) {
            return Err(ProfileError::Setup);
        }
        if !s.rtcp_mux {
            return Err(ProfileError::NoRtcpMux);
        }
        Ok((ufrag.to_owned(), fps.to_vec(), setup))
    };
    let (ufrag, fingerprints, setup) = transport(first.section)?;
    for c in chosen.iter().skip(1) {
        let (u, f, s) = transport(c.section)?;
        let same_fps = f.len() == fingerprints.len() && f.iter().all(|x| fingerprints.contains(x));
        if u != ufrag || !same_fps || s != setup {
            return Err(ProfileError::InconsistentTransport);
        }
    }

    let mid_extension = agreed_id(&chosen, |c| c.mid_ext)?;
    let transport_cc_extension = agreed_id(&chosen, |c| c.twcc_ext)?;
    if mid_extension.is_some() && mid_extension == transport_cc_extension {
        return Err(ProfileError::InconsistentExtensions);
    }

    let mut pts: Vec<u8> = Vec::new();
    let mut accepted_ssrcs: Vec<u32> = Vec::new();
    for c in &chosen {
        for pt in [Some(c.stream.payload_type), c.stream.rtx_payload_type]
            .into_iter()
            .flatten()
        {
            if pts.contains(&pt) {
                return Err(ProfileError::PayloadTypeClash);
            }
            pts.push(pt);
        }
        for ssrc in [c.stream.ssrc, c.stream.rtx_ssrc].into_iter().flatten() {
            if accepted_ssrcs.contains(&ssrc) {
                return Err(ProfileError::SsrcClash);
            }
            accepted_ssrcs.push(ssrc);
        }
    }
    let is_chosen = |s: &Section| chosen.iter().any(|c| std::ptr::eq(c.section, s));
    let mut rejected_mids = Vec::new();
    let mut rejected_ssrcs = Vec::new();
    for s in offer.sections.iter().filter(|s| !is_chosen(s)) {
        rejected_mids.extend(s.mid.clone());
        for ssrc in s.ssrcs.iter().chain(s.fid.iter().flat_map(|(a, b)| [a, b])) {
            if accepted_ssrcs.contains(ssrc) {
                return Err(ProfileError::SsrcClash);
            }
            if !rejected_ssrcs.contains(ssrc) {
                rejected_ssrcs.push(*ssrc);
            }
        }
    }

    // The answer's group, in the offer's group order: its first MID, the
    // tagged section, carries the candidates.
    let group: Vec<&str> = bundle
        .iter()
        .map(String::as_str)
        .filter(|m| chosen.iter().any(|c| c.stream.mid == *m))
        .collect();
    let tagged = group.first().copied().unwrap_or_default();

    let mut out = String::with_capacity(2048);
    let mut line = |s: &str| {
        out.push_str(s);
        out.push_str("\r\n");
    };
    line("v=0");
    line(&format!("o=- {} 1 IN IP4 0.0.0.0", config.origin));
    line("s=-");
    line("t=0 0");
    line(&format!("a=group:BUNDLE {}", group.join(" ")));
    line("a=ice-lite");
    for s in &offer.sections {
        match chosen.iter().find(|c| std::ptr::eq(c.section, s)) {
            Some(c) => {
                let text = accepted(c, config, mid_extension, c.stream.mid == tagged);
                for l in text.lines() {
                    line(l);
                }
            }
            None => {
                let first_format = s.formats.first().map(String::as_str).unwrap_or("0");
                line(&format!("m={} 0 {} {first_format}", s.media, s.proto));
                line("c=IN IP4 0.0.0.0");
                if let Some(m) = &s.mid {
                    line(&format!("a=mid:{m}"));
                }
            }
        }
    }
    let mut video = None;
    let mut audio = None;
    for c in chosen {
        match c.stream.kind {
            MediaKind::Video => video = Some(c.stream),
            _ => audio = Some(c.stream),
        }
    }
    Ok((
        out,
        Negotiated {
            remote_ufrag: ufrag,
            fingerprints,
            setup,
            video,
            audio,
            mid_extension,
            transport_cc_extension,
            rejected_mids,
            rejected_ssrcs,
        },
    ))
}

/// One accepted section's lines, joined by `\n`.
fn accepted(c: &Chosen<'_>, config: &AnswerConfig<'_>, mid_id: Option<u8>, tagged: bool) -> String {
    let s = c.section;
    let st = &c.stream;
    let mut t = String::new();
    let mut l = |x: &str| {
        t.push_str(x);
        t.push('\n');
    };
    let pts = match st.rtx_payload_type {
        Some(r) => format!("{} {r}", st.payload_type),
        None => st.payload_type.to_string(),
    };
    l(&format!("m={} 9 {} {pts}", s.media, s.proto));
    l("c=IN IP4 0.0.0.0");
    l(&format!("a=mid:{}", st.mid));
    l("a=recvonly");
    l(&format!("a=ice-ufrag:{}", config.credentials.ufrag()));
    l(&format!("a=ice-pwd:{}", config.credentials.pwd()));
    l(&format!(
        "a=fingerprint:sha-256 {}",
        config.fingerprint.to_sdp()
    ));
    l("a=setup:passive");
    l("a=rtcp-mux");
    if let Some(e) = c.mid_ext.filter(|e| Some(e.id) == mid_id) {
        l(&format!(
            "a=extmap:{}{} {MID_URI}",
            e.id,
            receivable(e).unwrap_or_default()
        ));
    }
    if let Some(e) = c.twcc_ext {
        l(&format!(
            "a=extmap:{}{} {TRANSPORT_CC_URI}",
            e.id,
            receivable(e).unwrap_or_default()
        ));
    }
    let pt = st.payload_type;
    let rtpmap = |pt: u8| {
        s.rtpmap(pt).map(|r| match r.channels {
            Some(ch) => format!("a=rtpmap:{pt} {}/{}/{ch}", r.encoding, r.clock_rate),
            None => format!("a=rtpmap:{pt} {}/{}", r.encoding, r.clock_rate),
        })
    };
    if let Some(r) = rtpmap(pt) {
        l(&r);
    }
    for (on, fb) in [
        (st.nack, "nack"),
        (st.pli, "nack pli"),
        (st.fir, "ccm fir"),
        (st.transport_cc, "transport-cc"),
    ] {
        if on {
            l(&format!("a=rtcp-fb:{pt} {fb}"));
        }
    }
    match st.kind {
        MediaKind::Video => {
            if let Some(p) = s.fmtp(pt) {
                l(&format!("a=fmtp:{pt} {p}"));
            }
        }
        _ => {
            let rate = config
                .opus_max_bitrate
                .clamp(OPUS_BITRATE.0, OPUS_BITRATE.1);
            l(&format!(
                "a=fmtp:{pt} stereo=1;maxaveragebitrate={rate};usedtx=0"
            ));
        }
    }
    if let Some(rtx) = st.rtx_payload_type {
        if let Some(r) = rtpmap(rtx) {
            l(&r);
        }
        l(&format!("a=fmtp:{rtx} {}", s.fmtp(rtx).unwrap_or_default()));
    }
    if tagged {
        for (i, cand) in config.candidates.iter().take(MAX_CANDIDATES).enumerate() {
            let n = u32::try_from(i).unwrap_or(0);
            let mut x = String::new();
            let _ = match cand.transport {
                // RFC 8445 section 5.1.2.1: host type preference 126, a
                // distinct local preference, component 1.
                Transport::Udp => write!(
                    x,
                    "a=candidate:{} 1 udp {} {} {} typ host",
                    i + 1,
                    (126 << 24) | ((65_535 - n) << 8) | 255,
                    cand.address.ip(),
                    cand.address.port()
                ),
                // RFC 6544 section 4.2: a lower type preference than UDP,
                // and passive's direction preference (4) in the local one.
                Transport::Tcp => write!(
                    x,
                    "a=candidate:{} 1 tcp {} {} {} typ host tcptype passive",
                    i + 1,
                    (90 << 24) | (((4 << 13) | (8191 - n.min(8191))) << 8) | 255,
                    cand.address.ip(),
                    cand.address.port()
                ),
            };
            l(&x);
        }
        l("a=end-of-candidates");
    }
    t
}
