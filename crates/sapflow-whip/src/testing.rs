//! Test helpers, for tests, the fuzz harness and the browser harness only:
//! the W0 fixtures, a synthetic offer, a STUN client, H.264 and Opus
//! packetisers, and a scripted publisher that drives a [`Session`] through
//! ICE, the `sapflow-dtls` test client and SRTP.
//!
//! The publisher shares this crate's STUN code and `sapflow-dtls`'s record
//! and handshake code, so it cannot catch a misreading they share; the
//! real browsers (the example harness and its recorded fixtures) are the
//! independent check.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs
)]

use crate::ice::{Credentials, Kind, Transport, Tuple, classify};
use crate::sdp::{self, AnswerConfig, Candidate, Negotiated, Offer};
use crate::session::{Event, Session, SessionConfig, Transmit};
use crate::stun::{self, Builder, Message};
use sapflow_dtls::testing::{Client, Options};
use sapflow_dtls::{Fingerprint, Identity};
use sapflow_rtp::rtp::RtpBuilder;
use sapflow_rtp::srtp::{Profile, Role, split_keying_material};
use sapflow_rtp::{Inbound, Outbound};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use sapflow_dtls::testing::{TEST_TIME, hex, identity, unhex};

/// A W0 fixture under `fixtures/whip/w0/`, as text.
pub fn fixture(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/whip/w0")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p:?}: {e}"))
}

/// A file under this crate's `tests/fixtures/`.
pub fn crate_fixture(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// The synthetic offer's ICE password.
pub const OFFER_PWD: &str = "0123456789abcdefghijklmn";
/// The synthetic offer's SSRCs: audio, video, video RTX.
pub const AUDIO_SSRC: u32 = 0x1001;
pub const VIDEO_SSRC: u32 = 0x2001;
pub const RTX_SSRC: u32 = 0x2002;
/// Its payload types and extension IDs.
pub const OPUS_PT: u8 = 111;
pub const H264_PT: u8 = 96;
pub const RTX_PT: u8 = 97;
pub const MID_ID: u8 = 4;
pub const TWCC_ID: u8 = 3;

/// A Chrome-shaped offer: audio (mid 0) then video (mid 1), BUNDLE, per
/// section credentials, `actpass`, the MID and transport-cc extensions,
/// H.264 Constrained Baseline with RTX, and SSRCs.
pub fn synthetic_offer(fingerprint: &Fingerprint, ufrag: &str) -> String {
    let fp = fingerprint.to_sdp();
    let transport = format!(
        "a=ice-ufrag:{ufrag}\r\na=ice-pwd:{OFFER_PWD}\r\na=fingerprint:sha-256 {fp}\r\na=setup:actpass\r\n"
    );
    let ext = format!(
        "a=extmap:{MID_ID} {}\r\na=extmap:{TWCC_ID} {}\r\n",
        sdp::MID_URI,
        sdp::TRANSPORT_CC_URI
    );
    format!(
        "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0 1\r\n\
         a=msid-semantic: WMS\r\n\
         m=audio 9 UDP/TLS/RTP/SAVPF {OPUS_PT} 0\r\nc=IN IP4 0.0.0.0\r\n{transport}a=mid:0\r\n{ext}\
         a=sendonly\r\na=rtcp-mux\r\na=rtpmap:{OPUS_PT} opus/48000/2\r\na=rtcp-fb:{OPUS_PT} transport-cc\r\n\
         a=fmtp:{OPUS_PT} minptime=10;useinbandfec=1\r\na=ssrc:{AUDIO_SSRC} cname:x\r\n\
         m=video 9 UDP/TLS/RTP/SAVPF {H264_PT} {RTX_PT}\r\nc=IN IP4 0.0.0.0\r\n{transport}a=mid:1\r\n{ext}\
         a=sendonly\r\na=rtcp-mux\r\na=rtpmap:{H264_PT} H264/90000\r\na=rtcp-fb:{H264_PT} nack\r\n\
         a=rtcp-fb:{H264_PT} nack pli\r\na=rtcp-fb:{H264_PT} ccm fir\r\na=rtcp-fb:{H264_PT} transport-cc\r\n\
         a=fmtp:{H264_PT} level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
         a=rtpmap:{RTX_PT} rtx/90000\r\na=fmtp:{RTX_PT} apt={H264_PT}\r\n\
         a=ssrc-group:FID {VIDEO_SSRC} {RTX_SSRC}\r\na=ssrc:{VIDEO_SSRC} cname:x\r\na=ssrc:{RTX_SSRC} cname:x\r\n"
    )
}

/// A datagram that makes a session panic in test builds, for panic
/// isolation tests. Its first byte reads as none of STUN, DTLS, RTP or
/// RTCP, so the router sends it to the owner of the tuple it came from.
pub const PANIC_DATAGRAM: &[u8] = b"\xffsapflow-whip test: panic here";

/// The panic [`PANIC_DATAGRAM`] causes.
pub fn injected_panic() -> ! {
    panic!("a test asked this session to panic")
}

/// The documentation address the tests publish from.
pub fn tuple(port: u16) -> Tuple {
    Tuple::udp(SocketAddr::from(([192, 0, 2, 7], port)))
}

/// Our side for tests: seeded credentials, the answer and the session.
pub struct Accepted {
    pub session: Session,
    pub answer: String,
    pub ufrag: String,
    pub pwd: String,
    pub negotiated: Negotiated,
}

/// Parses `offer`, answers it with seeded credentials and identity, and
/// makes a seeded session.
pub fn accept(offer: &str, config: SessionConfig, now: Instant, seed: u8) -> Accepted {
    let parsed = Offer::parse(offer.as_bytes()).expect("offer parses");
    let creds = Credentials::generate_seeded([seed; 32]).unwrap();
    let id = server_identity();
    let candidates = [Candidate {
        transport: Transport::Udp,
        address: "192.0.2.1:47200".parse().unwrap(),
    }];
    let (answer, negotiated) = sdp::answer(
        &parsed,
        &AnswerConfig {
            credentials: &creds,
            fingerprint: id.fingerprint(),
            candidates: &candidates,
            opus_max_bitrate: 128_000,
            origin: 1,
        },
    )
    .expect("answer");
    let (ufrag, pwd) = (creds.ufrag().to_owned(), creds.pwd().to_owned());
    let session = Session::new_seeded(config, id, creds, negotiated.clone(), now, [seed; 32])
        .expect("session");
    Accepted {
        session,
        answer,
        ufrag,
        pwd,
        negotiated,
    }
}

/// The seed of the browser harness's session, credentials and session ID,
/// so its recorded runs replay offline.
pub const HARNESS_SEED: [u8; 32] = *b"sapflow-whip gate 2 replay seed.";

/// The seeded server identity the tests and the harness use.
pub fn server_identity() -> Arc<Identity> {
    identity(0x5f)
}

/// Builds Binding requests as a controlling full agent would.
#[derive(Debug, Clone)]
pub struct StunClient {
    pub username: Vec<u8>,
    pub key: Vec<u8>,
    pub tiebreaker: u64,
    next: u32,
}

impl StunClient {
    pub fn new(server_ufrag: &str, server_pwd: &str, client_ufrag: &str) -> Self {
        Self {
            username: format!("{server_ufrag}:{client_ufrag}").into_bytes(),
            key: server_pwd.as_bytes().to_vec(),
            tiebreaker: 0x0102_0304_0506_0708,
            next: 1,
        }
    }

    fn transaction(&mut self) -> [u8; 12] {
        let mut t = [0x5a; 12];
        t[8..].copy_from_slice(&self.next.to_be_bytes());
        self.next += 1;
        t
    }

    /// A check, as Chrome sends one (with its unknown 0xC057).
    pub fn check(&mut self, use_candidate: bool) -> Vec<u8> {
        self.request(use_candidate, false)
    }

    /// A check carrying ICE-CONTROLLED instead of ICE-CONTROLLING.
    pub fn request(&mut self, use_candidate: bool, controlled: bool) -> Vec<u8> {
        let t = self.transaction();
        let mut b = Builder::new(stun::BINDING_REQUEST, &t)
            .attribute(stun::attr::USERNAME, &self.username)
            .attribute(0xc057, &[0, 1, 0, 0x0a])
            .attribute(stun::attr::PRIORITY, &0x6e7f_1eff_u32.to_be_bytes());
        let role = if controlled {
            stun::attr::ICE_CONTROLLED
        } else {
            stun::attr::ICE_CONTROLLING
        };
        b = b.attribute(role, &self.tiebreaker.to_be_bytes());
        if use_candidate {
            b = b.attribute(stun::attr::USE_CANDIDATE, &[]);
        }
        b.integrity(&self.key).fingerprint()
    }

    /// Whether `b` is a STUN message with valid integrity under our key
    /// and a valid fingerprint.
    pub fn verify(&self, b: &[u8]) -> bool {
        Message::parse(b).is_ok_and(|m| m.check_integrity(b, &self.key) && m.check_fingerprint(b))
    }
}

/// A sequence parameter set for 1280x720 Constrained Baseline (generated,
/// never from a camera).
pub const SPS: &[u8] = &[0x67, 0x42, 0xe0, 0x1f, 0x8d, 0x68, 0x05, 0x00, 0x5b, 0xa1];
/// A picture parameter set.
pub const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

/// An IDR slice NAL unit of `len` bytes with `first_mb_in_slice` 0.
pub fn idr_slice(len: usize, fill: u8) -> Vec<u8> {
    let mut n = vec![0x65, 0x88];
    n.resize(len.max(2), fill);
    n
}

/// A non-IDR slice NAL unit.
pub fn p_slice(len: usize, fill: u8) -> Vec<u8> {
    let mut n = vec![0x41, 0x9a];
    n.resize(len.max(2), fill);
    n
}

/// RFC 6184 payloads for one access unit's NAL units: small ones gathered
/// into a STAP-A, large ones split into FU-A, the rest single NAL unit
/// packets.
pub fn packetise(nals: &[&[u8]], mtu: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut stap: Vec<&[u8]> = Vec::new();
    let flush = |stap: &mut Vec<&[u8]>, out: &mut Vec<Vec<u8>>| {
        match stap.len() {
            0 => {}
            1 => out.push(stap[0].to_vec()),
            _ => {
                let nri = stap.iter().map(|n| n[0] & 0x60).max().unwrap_or(0);
                let mut p = vec![nri | sapflow_rtp::h264::STAP_A];
                for n in stap.iter() {
                    p.extend_from_slice(&(n.len() as u16).to_be_bytes());
                    p.extend_from_slice(n);
                }
                out.push(p);
            }
        }
        stap.clear();
    };
    for &n in nals {
        let stap_len: usize = 1 + stap.iter().map(|x| 2 + x.len()).sum::<usize>();
        if n.len() + 2 + stap_len <= mtu && n.len() < 64 {
            stap.push(n);
            continue;
        }
        flush(&mut stap, &mut out);
        if n.len() <= mtu {
            out.push(n.to_vec());
            continue;
        }
        let indicator = (n[0] & 0xe0) | sapflow_rtp::h264::FU_A;
        let body = &n[1..];
        let chunks: Vec<&[u8]> = body.chunks(mtu - 2).collect();
        for (i, c) in chunks.iter().enumerate() {
            let mut header = n[0] & 0x1f;
            if i == 0 {
                header |= 0x80;
            }
            if i + 1 == chunks.len() {
                header |= 0x40;
            }
            let mut p = vec![indicator, header];
            p.extend_from_slice(c);
            out.push(p);
        }
    }
    flush(&mut stap, &mut out);
    out
}

/// What the depacketiser makes of NAL units: 4-byte lengths, SPS and PPS
/// left out.
pub fn length_prefixed(nals: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in nals {
        if matches!(n[0] & 0x1f, 7 | 8) {
            continue;
        }
        v.extend_from_slice(&(n.len() as u32).to_be_bytes());
        v.extend_from_slice(n);
    }
    v
}

/// A 20 ms stereo CELT Opus packet (TOC config 31, code 0).
pub fn opus_packet(fill: u8) -> Vec<u8> {
    let mut p = vec![(31 << 3) | 0x04];
    p.resize(80, fill);
    p
}

/// A scripted publisher: STUN checks, the `sapflow-dtls` test client, and
/// SRTP protect of RTP and RTCP, with SRTCP unprotect of what the session
/// sends back.
pub struct Publisher {
    pub tuple: Tuple,
    pub client: Client,
    pub stun: Option<StunClient>,
    pub ufrag: String,
    srtp: Option<(Outbound, Inbound)>,
    /// RTCP from the session, decrypted.
    pub rtcp: Vec<Vec<u8>>,
    /// STUN responses received.
    pub stun_answers: Vec<Vec<u8>>,
    /// Bytes the session sent to other tuples.
    pub misdirected: usize,
    pub video_seq: u16,
    pub audio_seq: u16,
    pub rtx_seq: u16,
    pub twcc_seq: u16,
    pub video_ts: u32,
    pub audio_ts: u32,
    /// Put the MID and transport-cc extensions on packets.
    pub extensions: bool,
    /// The MID video packets carry.
    pub video_mid: Vec<u8>,
    /// Video RTP clock per frame (90 kHz): 3600 is 25 fps.
    pub video_step: u32,
}

impl Publisher {
    /// A publisher offering these `use_srtp` profiles.
    pub fn new(seed: u8, srtp_profiles: &[u16]) -> Self {
        let options = Options {
            srtp_profiles: Some(srtp_profiles.to_vec()),
            ..Options::default()
        };
        Self {
            tuple: tuple(40_000 + u16::from(seed)),
            client: Client::new(options, identity(seed), seed),
            stun: None,
            ufrag: format!("pub{seed:02x}"),
            srtp: None,
            rtcp: Vec::new(),
            stun_answers: Vec::new(),
            misdirected: 0,
            video_seq: 65_500,
            audio_seq: 1000,
            rtx_seq: 7000,
            twcc_seq: 0,
            video_ts: 90_000,
            audio_ts: 48_000,
            extensions: true,
            video_mid: b"1".to_vec(),
            video_step: 3600,
        }
    }

    /// Its offer.
    pub fn offer(&self) -> String {
        synthetic_offer(&self.client.fingerprint(), &self.ufrag)
    }

    /// Learns our credentials from the answer side.
    pub fn answered(&mut self, a: &Accepted) {
        self.client.expect_server(server_identity().fingerprint());
        self.stun = Some(StunClient::new(&a.ufrag, &a.pwd, &self.ufrag));
    }

    pub fn check(&mut self, use_candidate: bool) -> Vec<u8> {
        self.stun.as_mut().expect("answered").check(use_candidate)
    }

    /// Takes everything the session queued; handles what was sent to us.
    /// Returns the DTLS datagrams the client answers with.
    pub fn receive(&mut self, s: &mut Session) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        while let Some(t) = s.poll_transmit() {
            replies.extend(self.take(t));
        }
        replies
    }

    /// Handles one datagram sent to us (counted as misdirected if it is
    /// for another tuple). Returns the DTLS datagrams the client answers
    /// with.
    pub fn take(&mut self, t: Transmit) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        if t.tuple != self.tuple {
            self.misdirected += t.bytes.len();
            return replies;
        }
        match classify(&t.bytes) {
            Kind::Stun => self.stun_answers.push(t.bytes),
            Kind::Dtls => {
                if let Ok(r) = self.client.handle(&t.bytes) {
                    replies.extend(r);
                }
                self.receive_keys();
            }
            Kind::Rtcp => {
                let mut b = t.bytes.clone();
                let (_, inbound) = self.srtp.as_mut().expect("SRTCP after keys");
                let n = inbound
                    .unprotect_rtcp(&mut b)
                    .expect("our feedback unprotects");
                b.truncate(n);
                self.rtcp.push(b);
            }
            k => panic!("session sent {k:?}"),
        }
        replies
    }

    /// Derives SRTP keys once the client has connected.
    pub fn receive_keys(&mut self) {
        if self.srtp.is_some() || !self.client.connected() {
            return;
        }
        let profile = Profile::from_dtls_id(self.client.profile().unwrap()).unwrap();
        let km = self.client.keying_material.as_ref().unwrap();
        let keys = split_keying_material(profile, Role::Client, km).unwrap();
        self.srtp = Some((
            Outbound::new(&keys.local).unwrap(),
            Inbound::new(&keys.remote, sapflow_rtp::Config::default()).unwrap(),
        ));
    }

    pub fn has_keys(&self) -> bool {
        self.srtp.is_some()
    }

    /// Nominates with one check, then runs the DTLS handshake to the end.
    pub fn connect(&mut self, s: &mut Session, now: Instant) {
        let c = self.check(true);
        s.handle_datagram(now, self.tuple, &c);
        let mut pending = self.client.start();
        for _ in 0..16 {
            for d in std::mem::take(&mut pending) {
                s.handle_datagram(now, self.tuple, &d);
            }
            pending = self.receive(s);
            if self.has_keys() && pending.is_empty() {
                break;
            }
        }
        assert!(self.has_keys(), "handshake completes");
    }

    fn protect_rtp(&mut self, b: RtpBuilder) -> Vec<u8> {
        let (out, _) = self.srtp.as_mut().expect("keys");
        let mut buf = b.build();
        let len = buf.len();
        buf.resize(len + out.profile().rtp_overhead(), 0);
        let n = out.protect_rtp(&mut buf, len).unwrap();
        buf.truncate(n);
        buf
    }

    /// Protects an RTCP compound.
    pub fn protect_rtcp(&mut self, rtcp: &[u8]) -> Vec<u8> {
        self.try_protect_rtcp(rtcp).expect("protects")
    }

    /// Protects an RTCP compound, or `None` when SRTCP refuses it (shorter
    /// than a header and SSRC).
    pub fn try_protect_rtcp(&mut self, rtcp: &[u8]) -> Option<Vec<u8>> {
        let (out, _) = self.srtp.as_mut()?;
        let mut buf = rtcp.to_vec();
        buf.resize(rtcp.len() + out.profile().rtcp_overhead(), 0);
        let n = out.protect_rtcp(&mut buf, rtcp.len()).ok()?;
        buf.truncate(n);
        Some(buf)
    }

    fn extension(&mut self, mid: &[u8]) -> Option<(u16, Vec<u8>)> {
        if !self.extensions {
            return None;
        }
        let seq = self.twcc_seq.to_be_bytes();
        self.twcc_seq = self.twcc_seq.wrapping_add(1);
        Some(RtpBuilder::one_byte_elements(&[
            (MID_ID, mid),
            (TWCC_ID, &seq),
        ]))
    }

    /// The SRTP packets of one video access unit of these NAL units, and
    /// advances the clock by one frame.
    pub fn video(&mut self, nals: &[&[u8]], ssrc: u32) -> Vec<Vec<u8>> {
        let payloads = packetise(nals, 1100);
        let n = payloads.len();
        let ts = self.video_ts;
        let mut out = Vec::new();
        for (i, payload) in payloads.into_iter().enumerate() {
            let mid = self.video_mid.clone();
            let extension = self.extension(&mid);
            let b = RtpBuilder {
                marker: i + 1 == n,
                payload_type: H264_PT,
                sequence: self.video_seq,
                timestamp: ts,
                ssrc,
                extension,
                payload,
                ..RtpBuilder::default()
            };
            self.video_seq = self.video_seq.wrapping_add(1);
            out.push(self.protect_rtp(b));
        }
        self.video_ts = self.video_ts.wrapping_add(self.video_step);
        out
    }

    /// A keyframe: SPS and PPS in a STAP-A, then the IDR slice in FU-A.
    pub fn keyframe(&mut self, idr_len: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
        let idr = idr_slice(idr_len, 0x11);
        let nals: [&[u8]; 3] = [SPS, PPS, &idr];
        (self.video(&nals, VIDEO_SSRC), length_prefixed(&nals))
    }

    /// A delta frame in one packet or FU-A.
    pub fn delta(&mut self, len: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
        let p = p_slice(len, 0x22);
        let nals: [&[u8]; 1] = [&p];
        (self.video(&nals, VIDEO_SSRC), length_prefixed(&nals))
    }

    /// One Opus packet, and advances the clock by 20 ms.
    /// An audio packet with this payload, as `audio` sends one: for a
    /// payload the depacketiser must refuse.
    pub fn audio_payload(&mut self, payload: Vec<u8>) -> Vec<u8> {
        let extension = self.extension(b"0");
        let b = RtpBuilder {
            marker: false,
            payload_type: OPUS_PT,
            sequence: self.audio_seq,
            timestamp: self.audio_ts,
            ssrc: AUDIO_SSRC,
            extension,
            payload,
            ..RtpBuilder::default()
        };
        self.audio_seq = self.audio_seq.wrapping_add(1);
        self.audio_ts = self.audio_ts.wrapping_add(960);
        self.protect_rtp(b)
    }

    pub fn audio(&mut self, fill: u8) -> Vec<u8> {
        let extension = self.extension(b"0");
        let b = RtpBuilder {
            marker: false,
            payload_type: OPUS_PT,
            sequence: self.audio_seq,
            timestamp: self.audio_ts,
            ssrc: AUDIO_SSRC,
            extension,
            payload: opus_packet(fill),
            ..RtpBuilder::default()
        };
        self.audio_seq = self.audio_seq.wrapping_add(1);
        self.audio_ts = self.audio_ts.wrapping_add(960);
        self.protect_rtp(b)
    }

    /// An RTX retransmission of a video packet (RFC 4588): the original
    /// sequence number in front of the original payload.
    pub fn rtx(
        &mut self,
        original_seq: u16,
        payload: &[u8],
        timestamp: u32,
        marker: bool,
    ) -> Vec<u8> {
        let mut p = original_seq.to_be_bytes().to_vec();
        p.extend_from_slice(payload);
        let extension = self.extension(b"1");
        let b = RtpBuilder {
            marker,
            payload_type: RTX_PT,
            sequence: self.rtx_seq,
            timestamp,
            ssrc: RTX_SSRC,
            extension,
            payload: p,
            ..RtpBuilder::default()
        };
        self.rtx_seq = self.rtx_seq.wrapping_add(1);
        self.protect_rtp(b)
    }

    /// Protected sender reports for both streams, on one clock: `ntp` is
    /// the NTP time for the current RTP timestamps.
    pub fn sender_reports(&mut self, ntp: u64) -> Vec<Vec<u8>> {
        let sr = |ssrc: u32, rtp: u32| {
            let mut b = vec![0x80, 200, 0, 6];
            b.extend_from_slice(&ssrc.to_be_bytes());
            b.extend_from_slice(&ntp.to_be_bytes());
            b.extend_from_slice(&rtp.to_be_bytes());
            b.extend_from_slice(&[0; 8]);
            b
        };
        let v = sr(VIDEO_SSRC, self.video_ts);
        let a = sr(AUDIO_SSRC, self.audio_ts);
        vec![self.protect_rtcp(&v), self.protect_rtcp(&a)]
    }
}

/// Runs a session's timers up to `until`, in order.
pub fn run_until(s: &mut Session, until: Instant) {
    for _ in 0..100_000 {
        match s.poll_timeout() {
            Some(t) if t <= until => s.handle_timeout(t),
            _ => break,
        }
    }
    s.handle_timeout(until);
}

/// Every event queued now.
pub fn events(s: &mut Session) -> Vec<Event> {
    std::iter::from_fn(|| s.poll_event()).collect()
}

/// RTCP packet types in a compound (first byte of each header skipped).
pub fn rtcp_types(compound: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= compound.len() {
        let fmt = compound[at] & 0x1f;
        let pt = compound[at + 1];
        let words = u16::from_be_bytes([compound[at + 2], compound[at + 3]]) as usize;
        out.push((pt, fmt));
        at += 4 + words * 4;
    }
    out
}

/// 20 ms.
pub const FRAME: Duration = Duration::from_millis(20);
