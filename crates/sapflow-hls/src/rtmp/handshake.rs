//! The RTMP handshake, server side: C0 C1 in, S0 S1 S2 out, C2 in.
//!
//! Two forms exist. The original (Adobe's specification) echoes C1 as S2.
//! The "digest" form, introduced with Flash Player 9 and never specified
//! by Adobe, hides an HMAC-SHA256 of the packet inside C1 and S1 at an
//! offset derived from the packet, and signs S2 with a key derived from
//! C1's digest. ffmpeg 8 sends a digest C1 (observed); librtmp (OBS) does
//! too and, as we understand it, checks a digest S1 and S2 strictly when
//! S1 carries a server version, falling back to the original form when it
//! does not. See docs/reference.md.
//!
//! So: a C1 with a valid digest gets a digest S1 and a signed S2, the way
//! nginx-rtmp and mediamtx answer; anything else gets the original form.
//! C2 is consumed but not checked (nginx-rtmp, rml_rtmp and mediamtx do
//! not check it either): it proves nothing a server relies on.
//!
//! The scheme is public obfuscation, not security; the constants are the
//! ones every implementation carries (here from rml_rtmp, MIT).

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Length of C1, C2, S1 and S2.
pub const PACKET: usize = 1536;
const DIGEST: usize = 32;
/// The only C0 accepted: plain RTMP (6 would be RTMPE).
const VERSION: u8 = 3;
/// S1's server version; any non-zero first byte means "digest form". The
/// same as nginx-rtmp's.
const SERVER_VERSION: [u8; 4] = [0x0d, 0x0e, 0x0a, 0x0d];

const FP_KEY: &[u8] = b"Genuine Adobe Flash Player 001";
const FMS_KEY: &[u8] = b"Genuine Adobe Flash Media Server 001";
const KEY_TAIL: [u8; 32] = [
    0xf0, 0xee, 0xc2, 0x4a, 0x80, 0x68, 0xbe, 0xe8, 0x2e, 0x00, 0xd0, 0xd1, 0x02, 0x9e, 0x7e, 0x57,
    0x6e, 0xec, 0x5d, 0x2d, 0x29, 0x80, 0x6f, 0xab, 0x93, 0xb8, 0xe6, 0x36, 0xcf, 0xeb, 0x31, 0xae,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeError {
    /// C0 asked for something other than plain RTMP.
    UnsupportedVersion(u8),
    /// Bytes fed after the handshake finished.
    AlreadyDone,
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(f, "unsupported RTMP version {v}"),
            Self::AlreadyDone => f.write_str("handshake already finished"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// Which form the client used; for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Original,
    Digest,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    /// Feed more bytes.
    Continue,
    /// Finished. `leftover` is the start of the chunk stream (clients may
    /// send their first command straight after C2).
    Done { leftover: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    C0C1,
    C2,
    Done,
}

pub struct Handshake {
    stage: Stage,
    buf: Vec<u8>,
    rng: SplitMix,
    form: Option<Form>,
}

impl Handshake {
    /// `seed` fills S1 and S2's random bytes. They need not be secret; the
    /// seed is a parameter so tests are reproducible.
    pub fn new(seed: u64) -> Self {
        Self {
            stage: Stage::C0C1,
            buf: Vec::new(),
            rng: SplitMix(seed),
            form: None,
        }
    }

    pub fn form(&self) -> Option<Form> {
        self.form
    }

    /// Takes bytes from the client and appends any reply to `out`.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<Progress, HandshakeError> {
        if self.stage == Stage::Done {
            return Err(HandshakeError::AlreadyDone);
        }
        self.buf.extend_from_slice(input);
        if self.stage == Stage::C0C1 {
            if let Some(&v) = self.buf.first()
                && v != VERSION
            {
                return Err(HandshakeError::UnsupportedVersion(v));
            }
            let Some(c1) = self.buf.get(1..1 + PACKET) else {
                return Ok(Progress::Continue);
            };
            let mut packet = [0u8; PACKET];
            packet.copy_from_slice(c1);
            self.buf.drain(..1 + PACKET);
            self.respond(&packet, out);
            self.stage = Stage::C2;
        }
        if self.buf.len() < PACKET {
            return Ok(Progress::Continue);
        }
        self.buf.drain(..PACKET);
        self.stage = Stage::Done;
        Ok(Progress::Done {
            leftover: std::mem::take(&mut self.buf),
        })
    }

    fn respond(&mut self, c1: &[u8; PACKET], out: &mut Vec<u8>) {
        out.push(VERSION);
        let mut s1 = [0u8; PACKET];
        // Bytes 0..4 are our time (zero), 4..8 the version.
        self.rng.fill(&mut s1[8..]);
        match client_digest(c1) {
            Some(c1_digest) => {
                self.form = Some(Form::Digest);
                s1[4..8].copy_from_slice(&SERVER_VERSION);
                let at = offset_b(&s1);
                let d = digest_around(&s1, at, FMS_KEY);
                s1[at..at + DIGEST].copy_from_slice(&d);
                out.extend_from_slice(&s1);

                let mut s2 = [0u8; PACKET];
                self.rng.fill(&mut s2[..PACKET - DIGEST]);
                let key = hmac(&full_key(FMS_KEY), &[&c1_digest]);
                let sig = hmac(&key, &[&s2[..PACKET - DIGEST]]);
                s2[PACKET - DIGEST..].copy_from_slice(&sig);
                out.extend_from_slice(&s2);
            }
            None => {
                self.form = Some(Form::Original);
                out.extend_from_slice(&s1);
                out.extend_from_slice(c1);
            }
        }
    }
}

/// The digest offset when it is described by bytes 8..12 (the scheme
/// clients usually use). Always within the packet: at most 739 + 32.
fn offset_a(p: &[u8; PACKET]) -> usize {
    let sum: usize = p[8..12].iter().map(|&b| usize::from(b)).sum();
    sum % 728 + 12
}

/// The digest offset when it is described by bytes 772..776 (the scheme
/// servers usually use). At most 1503 + 32 = 1535.
fn offset_b(p: &[u8; PACKET]) -> usize {
    let sum: usize = p[772..776].iter().map(|&b| usize::from(b)).sum();
    sum % 728 + 776
}

/// HMAC of the packet with the 32 bytes at `at` left out.
fn digest_around(p: &[u8; PACKET], at: usize, key: &[u8]) -> [u8; DIGEST] {
    hmac(key, &[&p[..at], &p[at + DIGEST..]])
}

/// C1's digest, if it carries a valid one under either offset scheme.
fn client_digest(c1: &[u8; PACKET]) -> Option<[u8; DIGEST]> {
    [offset_a(c1), offset_b(c1)].into_iter().find_map(|at| {
        let mut stored = [0u8; DIGEST];
        stored.copy_from_slice(&c1[at..at + DIGEST]);
        (digest_around(c1, at, FP_KEY) == stored).then_some(stored)
    })
}

fn full_key(text: &[u8]) -> Vec<u8> {
    let mut k = text.to_vec();
    k.extend_from_slice(&KEY_TAIL);
    k
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; DIGEST] {
    // HMAC takes keys of any length, so this cannot fail.
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return [0; DIGEST];
    };
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

/// Filler for S1 and S2. Not cryptographic; it need not be.
struct SplitMix(u64);

impl SplitMix {
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            let bytes = z.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A digest C1 the way ffmpeg and librtmp build one (scheme A).
    pub(crate) fn digest_c1(seed: u64) -> [u8; PACKET] {
        let mut c1 = [0u8; PACKET];
        SplitMix(seed).fill(&mut c1[8..]);
        c1[4..8].copy_from_slice(&[9, 0, 124, 2]);
        let at = offset_a(&c1);
        let d = digest_around(&c1, at, FP_KEY);
        c1[at..at + DIGEST].copy_from_slice(&d);
        c1
    }

    fn run(c1: &[u8; PACKET], extra: &[u8]) -> (Vec<u8>, Handshake, Vec<u8>) {
        let mut hs = Handshake::new(7);
        let mut out = Vec::new();
        let mut input = vec![3u8];
        input.extend_from_slice(c1);
        assert_eq!(hs.feed(&input, &mut out), Ok(Progress::Continue));
        assert_eq!(out.len(), 1 + 2 * PACKET);
        let mut c2 = vec![0u8; PACKET];
        c2.extend_from_slice(extra);
        let Ok(Progress::Done { leftover }) = hs.feed(&c2, &mut out) else {
            panic!("handshake did not finish");
        };
        (out, hs, leftover)
    }

    #[test]
    fn original_form_echoes_c1() {
        let mut c1 = [0u8; PACKET];
        SplitMix(1).fill(&mut c1[8..]);
        let (out, hs, leftover) = run(&c1, b"next");
        assert_eq!(hs.form(), Some(Form::Original));
        assert_eq!(out[0], 3);
        assert_eq!(&out[5..9], &[0, 0, 0, 0], "S1 carries no server version");
        assert_eq!(&out[1 + PACKET..], &c1[..], "S2 echoes C1");
        assert_eq!(leftover, b"next");
    }

    /// Checks S1 and S2 the way librtmp's `HandShake` does with FP9 on.
    #[test]
    fn digest_form_passes_librtmps_checks() {
        for seed in 0..20 {
            let c1 = digest_c1(seed);
            let (out, hs, _) = run(&c1, &[]);
            assert_eq!(hs.form(), Some(Form::Digest));
            let mut s1 = [0u8; PACKET];
            s1.copy_from_slice(&out[1..1 + PACKET]);
            assert_ne!(s1[4], 0, "server version present");
            // librtmp tries the client's scheme first, then the other.
            let ok = [offset_a(&s1), offset_b(&s1)]
                .into_iter()
                .any(|at| digest_around(&s1, at, FMS_KEY)[..] == s1[at..at + DIGEST]);
            assert!(ok, "S1 digest verifies");
            // ffmpeg, when playing, checks scheme B only.
            let at = offset_b(&s1);
            assert_eq!(digest_around(&s1, at, FMS_KEY)[..], s1[at..at + DIGEST]);
            // S2: HMAC(HMAC(FMS key + tail, C1 digest), S2 without its tail).
            let s2 = &out[1 + PACKET..];
            let at = offset_a(&c1);
            let key = hmac(&full_key(FMS_KEY), &[&c1[at..at + DIGEST]]);
            let sig = hmac(&key, &[&s2[..PACKET - DIGEST]]);
            assert_eq!(&s2[PACKET - DIGEST..], &sig[..]);
        }
    }

    #[test]
    fn digest_in_scheme_b_is_found() {
        let mut c1 = [0u8; PACKET];
        SplitMix(3).fill(&mut c1[8..]);
        c1[4] = 9;
        let at = offset_b(&c1);
        let d = digest_around(&c1, at, FP_KEY);
        c1[at..at + DIGEST].copy_from_slice(&d);
        assert_eq!(client_digest(&c1), Some(d));
    }

    #[test]
    fn byte_at_a_time_and_errors() {
        let c1 = digest_c1(5);
        let mut hs = Handshake::new(1);
        let mut out = Vec::new();
        let mut all = vec![3u8];
        all.extend_from_slice(&c1);
        all.extend_from_slice(&[0u8; PACKET]);
        all.extend_from_slice(b"xy");
        let mut done = None;
        for b in &all {
            match hs.feed(&[*b], &mut out).expect("no error") {
                Progress::Continue => {}
                Progress::Done { leftover } => {
                    done = Some(leftover);
                    break;
                }
            }
        }
        // The final byte that finished C2 leaves nothing over; "xy" was
        // never fed.
        assert_eq!(done, Some(Vec::new()));
        assert_eq!(out.len(), 1 + 2 * PACKET);
        assert_eq!(hs.feed(b"z", &mut out), Err(HandshakeError::AlreadyDone));

        let mut hs = Handshake::new(1);
        assert_eq!(
            hs.feed(&[6], &mut Vec::new()),
            Err(HandshakeError::UnsupportedVersion(6))
        );
        let mut hs = Handshake::new(1);
        assert_eq!(hs.feed(&[], &mut Vec::new()), Ok(Progress::Continue));
    }
}
