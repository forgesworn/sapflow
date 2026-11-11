//! SDP offers (RFC 8866) as WHIP publishers send them, and the answer
//! (RFC 8829 as profiled in `docs/whip-ingest.md`, "SDP answer and stream
//! matching").
//!
//! [`Offer::parse`] reads the lines the profile needs, within stated
//! bounds ([`Limits`]), and keeps nothing else:
//!
//! - candidates (`a=candidate`, `a=end-of-candidates`,
//!   `a=remote-candidates`), connection and origin addresses are skipped
//!   unread, so the publisher's addresses are never stored or logged;
//! - the peer's `a=ice-pwd` is checked for its grammar, then dropped: a
//!   lite agent never sends a check, so it never needs it;
//! - `a=group` semantics other than BUNDLE (OBS's `LS`) are ignored;
//! - `a=extmap` may carry a direction suffix (`a=extmap:<id>/<direction>`);
//! - ICE credentials, fingerprints, the setup role, the direction and
//!   header extensions may be given at session level, where they are
//!   inherited, or per section, where the section's own value wins.
//!
//! [`answer`] then chooses what to accept and builds the answer, or refuses
//! with a typed reason. A [`Refusal::Malformed`] offer does not parse (HTTP
//! 400); a [`Refusal::Profile`] one is well formed but outside the profile
//! (HTTP 422).

mod answer;
#[cfg(test)]
mod tests;

pub use answer::{
    AnswerConfig, Candidate, H264Profile, MID_URI, Negotiated, Stream, TRANSPORT_CC_URI, answer,
};

use sapflow_dtls::Fingerprint;
use std::fmt;

/// Bounds on an offer. Every one has a refusal path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest offer in bytes. Default 16 KiB (`docs/whip-ingest.md`,
    /// Resources).
    pub max_offer: usize,
    /// Most lines. Default 1024 (Chrome 157's offer with candidates is
    /// about 200).
    pub max_lines: usize,
    /// Longest line in bytes. Default 1024.
    pub max_line: usize,
    /// Most media sections. Default 16.
    pub max_sections: usize,
    /// Most formats on one `m=` line. Default 64 (Chrome 157 offers 27
    /// video payloads).
    pub max_formats: usize,
    /// Most distinct SSRCs per section. Default 16.
    pub max_ssrcs: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_offer: 16 * 1024,
            max_lines: 1024,
            max_line: 1024,
            max_sections: 16,
            max_formats: 64,
            max_ssrcs: 16,
        }
    }
}

/// Why an offer does not parse (HTTP 400).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Larger than [`Limits::max_offer`].
    TooLarge,
    /// Not UTF-8.
    NotUtf8,
    /// More than [`Limits::max_lines`] lines.
    TooManyLines,
    /// A line longer than [`Limits::max_line`].
    LineTooLong,
    /// A control character inside a line: a CR other than the one before
    /// LF, NUL or any other C0 control but tab (RFC 8866 section 9). Some
    /// values are repeated in the answer, so none is let through.
    ControlCharacter,
    /// More than [`Limits::max_sections`] media sections.
    TooManySections,
    /// More than [`Limits::max_formats`] formats in a section.
    TooManyFormats,
    /// More than [`Limits::max_ssrcs`] SSRCs in a section.
    TooManySsrcs,
    /// A line that is not `<letter>=<value>`.
    Line,
    /// The first line is not `v=0`.
    Version,
    /// An `m=` line that does not parse, or an RTP format that is not a
    /// payload type.
    MediaLine,
    /// An `a=rtpmap` that does not parse.
    Rtpmap,
    /// An `a=rtcp-fb` that does not parse.
    RtcpFb,
    /// An `a=extmap` that does not parse, or an ID outside 1 to 255.
    Extmap,
    /// An `a=ssrc` or `a=ssrc-group` that does not parse.
    Ssrc,
    /// A `sha-256` `a=fingerprint` whose value is not 32 hex pairs.
    Fingerprint,
    /// An `a=ice-ufrag` or `a=ice-pwd` outside RFC 8839's grammar.
    IceCredential,
    /// An `a=setup` value other than `actpass`, `active`, `passive` or
    /// `holdconn`.
    Setup,
    /// An `a=mid` that is empty, longer than 32 bytes, repeated in a
    /// section, or used by two sections.
    Mid,
    /// An `a=group:BUNDLE` with no identification tags.
    Group,
}

/// Why a well-formed offer is outside the profile (HTTP 422).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileError {
    /// The offer says `a=ice-lite`; two lite agents cannot connect.
    PeerIceLite,
    /// No `a=group:BUNDLE`.
    NoBundle,
    /// More than one `a=group:BUNDLE`.
    SeveralBundles,
    /// Neither an audio nor a video section that sends, inside the BUNDLE
    /// group.
    NoMedia,
    /// The video section has no H.264 payload with `packetization-mode=1`
    /// and a Constrained Baseline, Baseline, Main or High profile.
    NoH264,
    /// The audio section has no `opus/48000/2`.
    NoOpus,
    /// Simulcast (`a=simulcast` or `a=ssrc-group:SIM`) in an accepted
    /// section.
    Simulcast,
    /// No `a=ice-ufrag` or `a=ice-pwd` for an accepted section.
    NoIceCredentials,
    /// No `sha-256` fingerprint (none at all, or only other hash
    /// functions).
    NoFingerprint,
    /// More than four `sha-256` fingerprints.
    TooManyFingerprints,
    /// No `a=setup`.
    NoSetup,
    /// `a=setup:passive` or `holdconn`: sapflow is the DTLS server only.
    Setup,
    /// An accepted section without `a=rtcp-mux`.
    NoRtcpMux,
    /// Accepted sections disagree on ICE credentials, fingerprints or
    /// setup, which BUNDLE shares.
    InconsistentTransport,
    /// Accepted sections give the MID or transport-cc header extension
    /// different IDs, or use one of those IDs for another extension.
    InconsistentExtensions,
    /// Two accepted payloads share a payload type.
    PayloadTypeClash,
    /// Two accepted streams share an SSRC.
    SsrcClash,
}

/// Why an offer was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Does not parse: HTTP 400.
    Malformed(ParseError),
    /// Outside the profile: HTTP 422.
    Profile(ProfileError),
}

impl Refusal {
    /// The HTTP status for this refusal.
    pub const fn status(self) -> u16 {
        match self {
            Self::Malformed(_) => 400,
            Self::Profile(_) => 422,
        }
    }

    /// A short plain-text reason for the response body. It names the
    /// rule, never anything from the offer.
    pub const fn reason(self) -> &'static str {
        use ParseError as P;
        use ProfileError as R;
        match self {
            Self::Malformed(e) => match e {
                P::TooLarge => "offer too large",
                P::NotUtf8 => "offer is not UTF-8",
                P::TooManyLines => "too many lines",
                P::LineTooLong => "line too long",
                P::ControlCharacter => "control character in a line",
                P::TooManySections => "too many media sections",
                P::TooManyFormats => "too many formats",
                P::TooManySsrcs => "too many SSRCs",
                P::Line => "malformed line",
                P::Version => "missing v=0",
                P::MediaLine => "malformed m= line",
                P::Rtpmap => "malformed a=rtpmap",
                P::RtcpFb => "malformed a=rtcp-fb",
                P::Extmap => "malformed a=extmap",
                P::Ssrc => "malformed a=ssrc",
                P::Fingerprint => "malformed a=fingerprint",
                P::IceCredential => "malformed ICE credentials",
                P::Setup => "malformed a=setup",
                P::Mid => "malformed or repeated a=mid",
                P::Group => "malformed a=group",
            },
            Self::Profile(e) => match e {
                R::PeerIceLite => "the offer is ICE-lite",
                R::NoBundle => "no BUNDLE group",
                R::SeveralBundles => "more than one BUNDLE group",
                R::NoMedia => "no audio or video to receive",
                R::NoH264 => "no H.264 in packetization-mode 1 with a supported profile",
                R::NoOpus => "no Opus",
                R::Simulcast => "simulcast is not supported",
                R::NoIceCredentials => "no ICE credentials",
                R::NoFingerprint => "no sha-256 fingerprint",
                R::TooManyFingerprints => "more than four sha-256 fingerprints",
                R::NoSetup => "no a=setup",
                R::Setup => "a=setup must be actpass or active",
                R::NoRtcpMux => "rtcp-mux is required",
                R::InconsistentTransport => "bundled sections disagree on the transport",
                R::InconsistentExtensions => "bundled sections disagree on header extensions",
                R::PayloadTypeClash => "payload types clash",
                R::SsrcClash => "SSRCs clash",
            },
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

impl std::error::Error for Refusal {}

impl From<ParseError> for Refusal {
    fn from(e: ParseError) -> Self {
        Self::Malformed(e)
    }
}

impl From<ProfileError> for Refusal {
    fn from(e: ProfileError) -> Self {
        Self::Profile(e)
    }
}

/// A media direction attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `sendrecv`.
    SendRecv,
    /// `sendonly`.
    SendOnly,
    /// `recvonly`.
    RecvOnly,
    /// `inactive`.
    Inactive,
}

impl Direction {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sendrecv" => Self::SendRecv,
            "sendonly" => Self::SendOnly,
            "recvonly" => Self::RecvOnly,
            "inactive" => Self::Inactive,
            _ => return None,
        })
    }
}

/// The offerer's DTLS role (RFC 8842).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// `actpass`.
    ActPass,
    /// `active`.
    Active,
    /// `passive`.
    Passive,
    /// `holdconn`.
    HoldConn,
}

/// A media section's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// `m=audio`.
    Audio,
    /// `m=video`.
    Video,
    /// `m=application` (data channels).
    Application,
    /// Anything else.
    Other,
}

/// `a=rtpmap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rtpmap {
    /// Payload type.
    pub payload_type: u8,
    /// Encoding name as written (compare case-insensitively).
    pub encoding: String,
    /// Clock rate.
    pub clock_rate: u32,
    /// Channels, if given.
    pub channels: Option<u16>,
}

/// `a=extmap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extmap {
    /// ID, 1 to 255.
    pub id: u8,
    /// The direction suffix, if any.
    pub direction: Option<Direction>,
    /// The extension URI.
    pub uri: String,
}

/// Values that may be given at session level or per section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Shared {
    ice_ufrag: Option<String>,
    /// Whether a valid `a=ice-pwd` was present (the value is not kept).
    ice_pwd: bool,
    /// `sha-256` fingerprints, deduplicated, in order.
    fingerprints: Vec<Fingerprint>,
    /// `a=fingerprint` lines with other hash functions.
    other_fingerprints: usize,
    setup: Option<Setup>,
    direction: Option<Direction>,
}

/// One `m=` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The kind.
    pub kind: MediaKind,
    /// The kind as written, for a rejected section's `m=` line.
    pub media: String,
    /// The port; 0 means disabled unless `a=bundle-only` is set.
    pub port: u16,
    /// The transport protocol as written.
    pub proto: String,
    /// The formats as written.
    pub formats: Vec<String>,
    /// `a=mid`.
    pub mid: Option<String>,
    /// `a=rtcp-mux`.
    pub rtcp_mux: bool,
    /// `a=bundle-only`.
    pub bundle_only: bool,
    /// `a=rtpmap` lines.
    pub rtpmaps: Vec<Rtpmap>,
    /// `a=fmtp` lines: payload type and parameters as written.
    pub fmtps: Vec<(u8, String)>,
    /// `a=rtcp-fb` lines: payload type (`None` for `*`) and the feedback
    /// type and subtype, single-spaced.
    pub rtcp_fbs: Vec<(Option<u8>, String)>,
    /// `a=extmap` lines, with session-level ones the section does not
    /// override.
    pub extmaps: Vec<Extmap>,
    /// Distinct SSRCs from `a=ssrc`, in order.
    pub ssrcs: Vec<u32>,
    /// `a=ssrc-group:FID` pairs: media SSRC, then RTX SSRC.
    pub fid: Vec<(u32, u32)>,
    /// `a=simulcast` or `a=ssrc-group:SIM` was present.
    pub simulcast: bool,
    shared: Shared,
}

impl Section {
    /// The rtpmap of a payload type.
    pub fn rtpmap(&self, pt: u8) -> Option<&Rtpmap> {
        self.rtpmaps.iter().find(|r| r.payload_type == pt)
    }

    /// The fmtp parameters of a payload type.
    pub fn fmtp(&self, pt: u8) -> Option<&str> {
        self.fmtps
            .iter()
            .find(|(p, _)| *p == pt)
            .map(|(_, s)| s.as_str())
    }

    /// Whether the offer lists this feedback for the payload type (or for
    /// `*`).
    pub fn has_feedback(&self, pt: u8, feedback: &str) -> bool {
        self.rtcp_fbs
            .iter()
            .any(|(p, f)| p.is_none_or(|p| p == pt) && f.eq_ignore_ascii_case(feedback))
    }

    /// The extmap for a URI.
    pub fn extmap(&self, uri: &str) -> Option<&Extmap> {
        self.extmaps.iter().find(|e| e.uri == uri)
    }

    /// Payload types in offer order (the `m=` line), skipping formats that
    /// are not numbers.
    pub fn payload_types(&self) -> impl Iterator<Item = u8> + '_ {
        self.formats.iter().filter_map(|f| f.parse().ok())
    }
}

/// A parsed offer. It holds no address and no peer password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    session: Shared,
    session_extmaps: Vec<Extmap>,
    /// `a=ice-lite` at session level.
    pub ice_lite: bool,
    /// Each `a=group:BUNDLE`'s identification tags.
    pub bundles: Vec<Vec<String>>,
    /// The media sections, in order.
    pub sections: Vec<Section>,
}

/// Splits `a=<name>[:<value>]`.
fn attribute(v: &str) -> (&str, &str) {
    v.split_once(':').unwrap_or((v, ""))
}

fn ice_chars(s: &str, min: usize) -> bool {
    (min..=256).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
}

fn parse_payload_type(s: &str) -> Option<u8> {
    s.parse::<u8>().ok().filter(|pt| *pt <= 127)
}

impl Offer {
    /// Parses an offer within the default [`Limits`].
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with(bytes, &Limits::default())
    }

    /// Parses an offer within `limits`.
    pub fn parse_with(bytes: &[u8], limits: &Limits) -> Result<Self, ParseError> {
        if bytes.len() > limits.max_offer {
            return Err(ParseError::TooLarge);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ParseError::NotUtf8)?;
        let mut offer = Offer {
            session: Shared::default(),
            session_extmaps: Vec::new(),
            ice_lite: false,
            bundles: Vec::new(),
            sections: Vec::new(),
        };
        let mut lines = 0usize;
        let mut first = true;
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                continue;
            }
            lines += 1;
            if lines > limits.max_lines {
                return Err(ParseError::TooManyLines);
            }
            if line.len() > limits.max_line {
                return Err(ParseError::LineTooLong);
            }
            if line.bytes().any(|c| c < 0x20 && c != b'\t') {
                return Err(ParseError::ControlCharacter);
            }
            let (kind, value) = line.split_once('=').ok_or(ParseError::Line)?;
            if kind.len() != 1 || !kind.bytes().all(|c| c.is_ascii_lowercase()) {
                return Err(ParseError::Line);
            }
            if first {
                if line != "v=0" {
                    return Err(ParseError::Version);
                }
                first = false;
                continue;
            }
            match kind {
                "m" => offer.media_line(value, limits)?,
                "a" => offer.attribute_line(value, limits)?,
                // o=, c= and every other line type: not needed, not kept.
                _ => {}
            }
        }
        if first {
            return Err(ParseError::Version);
        }
        let mut mids: Vec<&str> = Vec::new();
        for s in &offer.sections {
            if let Some(m) = &s.mid {
                if mids.contains(&m.as_str()) {
                    return Err(ParseError::Mid);
                }
                mids.push(m);
            }
        }
        // Session-level extmaps apply to sections that do not set the ID.
        for s in &mut offer.sections {
            for e in &offer.session_extmaps {
                if !s.extmaps.iter().any(|x| x.id == e.id) {
                    s.extmaps.push(e.clone());
                }
            }
        }
        Ok(offer)
    }

    fn media_line(&mut self, value: &str, limits: &Limits) -> Result<(), ParseError> {
        if self.sections.len() >= limits.max_sections {
            return Err(ParseError::TooManySections);
        }
        let mut f = value.split(' ').filter(|s| !s.is_empty());
        let media = f.next().ok_or(ParseError::MediaLine)?;
        let port = f.next().ok_or(ParseError::MediaLine)?;
        let port = port.split_once('/').map_or(port, |(p, _)| p);
        let port: u16 = port.parse().map_err(|_| ParseError::MediaLine)?;
        let proto = f.next().ok_or(ParseError::MediaLine)?;
        let mut formats = Vec::new();
        for fmt in f {
            if formats.len() >= limits.max_formats {
                return Err(ParseError::TooManyFormats);
            }
            formats.push(fmt.to_owned());
        }
        if formats.is_empty() {
            return Err(ParseError::MediaLine);
        }
        let rtp = proto.contains("RTP");
        if rtp && formats.iter().any(|f| parse_payload_type(f).is_none()) {
            return Err(ParseError::MediaLine);
        }
        let kind = match media {
            "audio" => MediaKind::Audio,
            "video" => MediaKind::Video,
            "application" => MediaKind::Application,
            _ => MediaKind::Other,
        };
        self.sections.push(Section {
            kind,
            media: media.to_owned(),
            port,
            proto: proto.to_owned(),
            formats,
            mid: None,
            rtcp_mux: false,
            bundle_only: false,
            rtpmaps: Vec::new(),
            fmtps: Vec::new(),
            rtcp_fbs: Vec::new(),
            extmaps: Vec::new(),
            ssrcs: Vec::new(),
            fid: Vec::new(),
            simulcast: false,
            shared: Shared::default(),
        });
        Ok(())
    }

    fn attribute_line(&mut self, value: &str, limits: &Limits) -> Result<(), ParseError> {
        let (name, v) = attribute(value);
        // Attributes that may be shared go to the section, or to the
        // session before the first m= line.
        let shared = match self.sections.last_mut() {
            Some(s) => &mut s.shared,
            None => &mut self.session,
        };
        match name {
            "ice-ufrag" => {
                if !ice_chars(v, 4) {
                    return Err(ParseError::IceCredential);
                }
                if shared.ice_ufrag.is_none() {
                    shared.ice_ufrag = Some(v.to_owned());
                }
                return Ok(());
            }
            "ice-pwd" => {
                if !ice_chars(v, 22) {
                    return Err(ParseError::IceCredential);
                }
                shared.ice_pwd = true;
                return Ok(());
            }
            "fingerprint" => {
                let (hash, fp) = v.split_once(' ').ok_or(ParseError::Fingerprint)?;
                if hash.eq_ignore_ascii_case("sha-256") {
                    let fp = Fingerprint::parse_sdp(fp.trim()).ok_or(ParseError::Fingerprint)?;
                    // One more than the most accepted is enough to refuse.
                    if !shared.fingerprints.contains(&fp)
                        && shared.fingerprints.len() <= sapflow_dtls::MAX_EXPECTED_FINGERPRINTS
                    {
                        shared.fingerprints.push(fp);
                    }
                } else {
                    shared.other_fingerprints += 1;
                }
                return Ok(());
            }
            "setup" => {
                let s = match v {
                    "actpass" => Setup::ActPass,
                    "active" => Setup::Active,
                    "passive" => Setup::Passive,
                    "holdconn" => Setup::HoldConn,
                    _ => return Err(ParseError::Setup),
                };
                shared.setup.get_or_insert(s);
                return Ok(());
            }
            _ => {}
        }
        if let Some(d) = Direction::parse(name) {
            shared.direction = Some(d);
            return Ok(());
        }
        let Some(s) = self.sections.last_mut() else {
            match name {
                "ice-lite" => self.ice_lite = true,
                "group" => {
                    let mut tags = v.split(' ').filter(|t| !t.is_empty());
                    if tags.next() == Some("BUNDLE") {
                        let mids: Vec<String> =
                            tags.take(limits.max_sections).map(str::to_owned).collect();
                        if mids.is_empty() {
                            return Err(ParseError::Group);
                        }
                        if self.bundles.len() < limits.max_sections {
                            self.bundles.push(mids);
                        }
                    }
                }
                "extmap" => {
                    let e = parse_extmap(v)?;
                    if self.session_extmaps.len() < limits.max_formats {
                        self.session_extmaps.push(e);
                    }
                }
                _ => {}
            }
            return Ok(());
        };
        match name {
            "mid" => {
                if v.is_empty() || v.len() > 32 || s.mid.is_some() {
                    return Err(ParseError::Mid);
                }
                s.mid = Some(v.to_owned());
            }
            "rtcp-mux" => s.rtcp_mux = true,
            "bundle-only" => s.bundle_only = true,
            "simulcast" => s.simulcast = true,
            "rtpmap" => {
                let (pt, enc) = v.split_once(' ').ok_or(ParseError::Rtpmap)?;
                let pt = parse_payload_type(pt).ok_or(ParseError::Rtpmap)?;
                let mut parts = enc.trim().split('/');
                let encoding = parts
                    .next()
                    .filter(|e| !e.is_empty())
                    .ok_or(ParseError::Rtpmap)?;
                let clock_rate = parts
                    .next()
                    .and_then(|c| c.parse().ok())
                    .ok_or(ParseError::Rtpmap)?;
                let channels = match parts.next() {
                    Some(c) => Some(c.parse().map_err(|_| ParseError::Rtpmap)?),
                    None => None,
                };
                if s.rtpmap(pt).is_none() && s.rtpmaps.len() < limits.max_formats {
                    s.rtpmaps.push(Rtpmap {
                        payload_type: pt,
                        encoding: encoding.to_owned(),
                        clock_rate,
                        channels,
                    });
                }
            }
            "fmtp" => {
                // A format that is not a payload type (an old data channel
                // form) is not ours to read.
                if let Some((pt, params)) = v.split_once(' ')
                    && let Some(pt) = parse_payload_type(pt)
                    && s.fmtp(pt).is_none()
                    && s.fmtps.len() < limits.max_formats
                {
                    s.fmtps.push((pt, params.trim().to_owned()));
                }
            }
            "rtcp-fb" => {
                let (pt, fb) = v.split_once(' ').ok_or(ParseError::RtcpFb)?;
                let pt = if pt == "*" {
                    None
                } else {
                    Some(parse_payload_type(pt).ok_or(ParseError::RtcpFb)?)
                };
                let fb = fb
                    .split(' ')
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if fb.is_empty() {
                    return Err(ParseError::RtcpFb);
                }
                if s.rtcp_fbs.len() < limits.max_lines {
                    s.rtcp_fbs.push((pt, fb));
                }
            }
            "extmap" => {
                let e = parse_extmap(v)?;
                if !s.extmaps.iter().any(|x| x.id == e.id) && s.extmaps.len() < limits.max_formats {
                    s.extmaps.push(e);
                }
            }
            "ssrc" => {
                let id = v.split(' ').next().unwrap_or_default();
                let ssrc: u32 = id.parse().map_err(|_| ParseError::Ssrc)?;
                if !s.ssrcs.contains(&ssrc) {
                    if s.ssrcs.len() >= limits.max_ssrcs {
                        return Err(ParseError::TooManySsrcs);
                    }
                    s.ssrcs.push(ssrc);
                }
            }
            "ssrc-group" => {
                let mut f = v.split(' ').filter(|x| !x.is_empty());
                let semantics = f.next().ok_or(ParseError::Ssrc)?;
                let ids = f
                    .take(limits.max_ssrcs + 1)
                    .map(|x| x.parse::<u32>().map_err(|_| ParseError::Ssrc))
                    .collect::<Result<Vec<_>, _>>()?;
                if ids.len() > limits.max_ssrcs {
                    return Err(ParseError::TooManySsrcs);
                }
                match (semantics, ids.as_slice()) {
                    ("FID", [media, rtx]) => {
                        if s.fid.len() < limits.max_ssrcs {
                            s.fid.push((*media, *rtx));
                        }
                    }
                    ("FID", _) => return Err(ParseError::Ssrc),
                    ("SIM", _) => s.simulcast = true,
                    _ => {}
                }
            }
            // Candidates and everything else: skipped unread.
            _ => {}
        }
        Ok(())
    }

    /// The offer's session-level `a=ice-lite`.
    pub fn is_ice_lite(&self) -> bool {
        self.ice_lite
    }

    /// The ICE ufrag that applies to a section.
    pub fn ice_ufrag<'a>(&'a self, s: &'a Section) -> Option<&'a str> {
        s.shared
            .ice_ufrag
            .as_deref()
            .or(self.session.ice_ufrag.as_deref())
    }

    /// Whether an `a=ice-pwd` applies to a section.
    pub fn has_ice_pwd(&self, s: &Section) -> bool {
        s.shared.ice_pwd || self.session.ice_pwd
    }

    /// The `sha-256` fingerprints that apply to a section, and how many
    /// lines with other hash functions came with them: the section's own
    /// `a=fingerprint` lines if it has any, else the session's.
    pub fn fingerprints<'a>(&'a self, s: &'a Section) -> (&'a [Fingerprint], usize) {
        let own = &s.shared;
        if own.fingerprints.is_empty() && own.other_fingerprints == 0 {
            (&self.session.fingerprints, self.session.other_fingerprints)
        } else {
            (&own.fingerprints, own.other_fingerprints)
        }
    }

    /// The setup role that applies to a section.
    pub fn setup(&self, s: &Section) -> Option<Setup> {
        s.shared.setup.or(self.session.setup)
    }

    /// The direction that applies to a section (default `sendrecv`).
    pub fn direction(&self, s: &Section) -> Direction {
        s.shared
            .direction
            .or(self.session.direction)
            .unwrap_or(Direction::SendRecv)
    }
}

fn parse_extmap(v: &str) -> Result<Extmap, ParseError> {
    let mut f = v.split(' ').filter(|x| !x.is_empty());
    let head = f.next().ok_or(ParseError::Extmap)?;
    let uri = f.next().ok_or(ParseError::Extmap)?;
    let (id, direction) = match head.split_once('/') {
        Some((id, d)) => (id, Some(Direction::parse(d).ok_or(ParseError::Extmap)?)),
        None => (head, None),
    };
    let id: u8 = id.parse().map_err(|_| ParseError::Extmap)?;
    if id == 0 {
        return Err(ParseError::Extmap);
    }
    Ok(Extmap {
        id,
        direction,
        uri: uri.to_owned(),
    })
}

/// Parses `key=value;key=value` format parameters.
pub fn fmtp_params(params: &str) -> impl Iterator<Item = (&str, &str)> {
    params.split(';').filter_map(|p| {
        let p = p.trim();
        if p.is_empty() {
            return None;
        }
        Some(p.split_once('=').unwrap_or((p, "")))
    })
}

/// One format parameter's value.
pub fn fmtp_param<'a>(params: &'a str, key: &str) -> Option<&'a str> {
    fmtp_params(params)
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim())
}
