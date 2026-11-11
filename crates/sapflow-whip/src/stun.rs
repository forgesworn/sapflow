//! The STUN subset ICE-lite needs (RFC 8489): Binding requests, success
//! and error responses, and the attributes USERNAME, MESSAGE-INTEGRITY
//! (HMAC-SHA1 with short-term credentials), FINGERPRINT, XOR-MAPPED-ADDRESS,
//! PRIORITY, USE-CANDIDATE, ICE-CONTROLLING, ICE-CONTROLLED, ERROR-CODE and
//! UNKNOWN-ATTRIBUTES.
//!
//! [`Message::parse`] checks the header and walks every attribute within
//! the stated length. Following RFC 8489 section 14:
//!
//! - only the first of a repeated attribute is read;
//! - attributes after MESSAGE-INTEGRITY are ignored, except FINGERPRINT,
//!   and nothing may follow FINGERPRINT;
//! - unknown comprehension-optional attributes (0x8000 and above) are
//!   skipped; unknown comprehension-required ones before
//!   MESSAGE-INTEGRITY are listed, for a 420 answer.
//!
//! The message does not own its bytes, so integrity and fingerprint checks
//! take the same buffer the message was parsed from.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use subtle::ConstantTimeEq;

/// The fixed header.
pub const HEADER_LEN: usize = 20;
/// The magic cookie (RFC 8489 section 5).
pub const MAGIC_COOKIE: u32 = 0x2112_a442;
/// XORed into the CRC-32 of FINGERPRINT (RFC 8489 section 14.7).
pub const FINGERPRINT_XOR: u32 = 0x5354_554e;
/// Length of a MESSAGE-INTEGRITY attribute, header included.
pub const INTEGRITY_LEN: usize = 24;
/// Length of a FINGERPRINT attribute, header included.
pub const FINGERPRINT_LEN: usize = 8;
/// Most attributes read from one message; more is refused.
pub const MAX_ATTRIBUTES: usize = 32;
/// Most unknown comprehension-required attribute types listed in a 420.
pub const MAX_UNKNOWN: usize = 8;
/// Longest USERNAME value accepted (RFC 8489 section 14.3 allows 513
/// bytes; ICE's is two ufrags of at most 256 characters and a colon).
pub const MAX_USERNAME: usize = 513;

/// Binding request.
pub const BINDING_REQUEST: u16 = 0x0001;
/// Binding indication.
pub const BINDING_INDICATION: u16 = 0x0011;
/// Binding success response.
pub const BINDING_SUCCESS: u16 = 0x0101;
/// Binding error response.
pub const BINDING_ERROR: u16 = 0x0111;

/// Attribute types (RFC 8489 section 18.3, RFC 8445 section 16.1).
pub mod attr {
    /// USERNAME.
    pub const USERNAME: u16 = 0x0006;
    /// MESSAGE-INTEGRITY.
    pub const MESSAGE_INTEGRITY: u16 = 0x0008;
    /// ERROR-CODE.
    pub const ERROR_CODE: u16 = 0x0009;
    /// UNKNOWN-ATTRIBUTES.
    pub const UNKNOWN_ATTRIBUTES: u16 = 0x000a;
    /// XOR-MAPPED-ADDRESS.
    pub const XOR_MAPPED_ADDRESS: u16 = 0x0020;
    /// PRIORITY.
    pub const PRIORITY: u16 = 0x0024;
    /// USE-CANDIDATE.
    pub const USE_CANDIDATE: u16 = 0x0025;
    /// SOFTWARE (comprehension-optional; never sent).
    pub const SOFTWARE: u16 = 0x8022;
    /// FINGERPRINT.
    pub const FINGERPRINT: u16 = 0x8028;
    /// ICE-CONTROLLED.
    pub const ICE_CONTROLLED: u16 = 0x8029;
    /// ICE-CONTROLLING.
    pub const ICE_CONTROLLING: u16 = 0x802a;
}

/// Why bytes are not a STUN message this module reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StunError {
    /// Shorter than the header, or the first two bits are not zero.
    NotStun,
    /// The magic cookie is wrong (an RFC 3489 message, or not STUN).
    Cookie,
    /// The length field is not a multiple of 4 or does not match the
    /// buffer.
    Length,
    /// An attribute runs past the end of the message.
    Truncated,
    /// More than [`MAX_ATTRIBUTES`] attributes.
    TooManyAttributes,
    /// An attribute follows FINGERPRINT.
    AfterFingerprint,
    /// A known attribute has the wrong length or a value that does not
    /// parse.
    BadAttribute(u16),
}

impl fmt::Display for StunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "STUN: {self:?}")
    }
}

impl std::error::Error for StunError {}

/// A parsed STUN message, borrowing its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Message<'a> {
    /// The message type (class and method).
    pub msg_type: u16,
    /// The 96-bit transaction ID.
    pub transaction: [u8; 12],
    /// USERNAME, if present.
    pub username: Option<&'a [u8]>,
    /// PRIORITY.
    pub priority: Option<u32>,
    /// USE-CANDIDATE was present.
    pub use_candidate: bool,
    /// ICE-CONTROLLING's tiebreaker.
    pub ice_controlling: Option<u64>,
    /// ICE-CONTROLLED's tiebreaker.
    pub ice_controlled: Option<u64>,
    /// XOR-MAPPED-ADDRESS, decoded.
    pub xor_mapped: Option<SocketAddr>,
    /// ERROR-CODE: the code (300 to 699) and reason phrase.
    pub error: Option<(u16, &'a [u8])>,
    /// UNKNOWN-ATTRIBUTES, as listed (at most [`MAX_UNKNOWN`]).
    pub unknown_listed: Vec<u16>,
    /// Unknown comprehension-required attribute types before
    /// MESSAGE-INTEGRITY (at most [`MAX_UNKNOWN`]).
    pub unknown_required: Vec<u16>,
    /// Offset of the MESSAGE-INTEGRITY attribute, and its HMAC.
    pub integrity: Option<(usize, [u8; 20])>,
    /// Offset of the FINGERPRINT attribute, and its value.
    pub fingerprint: Option<(usize, u32)>,
    /// Attributes walked.
    pub attributes: usize,
}

impl fmt::Debug for Message<'_> {
    /// The transaction ID, USERNAME and any address are left out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("msg_type", &format_args!("{:#06x}", self.msg_type))
            .field("use_candidate", &self.use_candidate)
            .field("ice_controlling", &self.ice_controlling.is_some())
            .field("ice_controlled", &self.ice_controlled.is_some())
            .field("error", &self.error.map(|(c, _)| c))
            .field("unknown_required", &self.unknown_required)
            .field("integrity", &self.integrity.is_some())
            .field("fingerprint", &self.fingerprint.is_some())
            .finish_non_exhaustive()
    }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(s))
}

fn be64(b: &[u8]) -> Option<u64> {
    let s: [u8; 8] = b.try_into().ok()?;
    Some(u64::from_be_bytes(s))
}

/// Whether the first bytes could start a STUN message: the two top bits
/// zero and the magic cookie in place (RFC 7983's first byte 0 to 3, plus
/// the cookie).
pub fn looks_like_stun(b: &[u8]) -> bool {
    b.len() >= HEADER_LEN
        && b.first().is_some_and(|x| x >> 6 == 0)
        && be32(b, 4) == Some(MAGIC_COOKIE)
}

/// The USERNAME of a Binding request, without checking anything else.
/// The UDP owner uses it to find which session a check from a new address
/// is for, before the session authenticates it.
pub fn request_username(b: &[u8]) -> Option<&[u8]> {
    let m = Message::parse(b).ok()?;
    (m.msg_type == BINDING_REQUEST)
        .then_some(m.username)
        .flatten()
}

impl<'a> Message<'a> {
    /// Parses and checks the framing of a STUN message that fills `b`.
    pub fn parse(b: &'a [u8]) -> Result<Self, StunError> {
        let first = *b.first().ok_or(StunError::NotStun)?;
        if b.len() < HEADER_LEN || first >> 6 != 0 {
            return Err(StunError::NotStun);
        }
        let msg_type = be16(b, 0).ok_or(StunError::NotStun)?;
        let length = usize::from(be16(b, 2).ok_or(StunError::NotStun)?);
        if be32(b, 4) != Some(MAGIC_COOKIE) {
            return Err(StunError::Cookie);
        }
        if !length.is_multiple_of(4) || HEADER_LEN + length != b.len() {
            return Err(StunError::Length);
        }
        let transaction: [u8; 12] = b
            .get(8..HEADER_LEN)
            .and_then(|t| t.try_into().ok())
            .ok_or(StunError::NotStun)?;
        let mut m = Message {
            msg_type,
            transaction,
            username: None,
            priority: None,
            use_candidate: false,
            ice_controlling: None,
            ice_controlled: None,
            xor_mapped: None,
            error: None,
            unknown_listed: Vec::new(),
            unknown_required: Vec::new(),
            integrity: None,
            fingerprint: None,
            attributes: 0,
        };
        let mut seen: Vec<u16> = Vec::new();
        let mut at = HEADER_LEN;
        while at < b.len() {
            if m.fingerprint.is_some() {
                return Err(StunError::AfterFingerprint);
            }
            if m.attributes >= MAX_ATTRIBUTES {
                return Err(StunError::TooManyAttributes);
            }
            m.attributes += 1;
            let ty = be16(b, at).ok_or(StunError::Truncated)?;
            let len = usize::from(be16(b, at + 2).ok_or(StunError::Truncated)?);
            let start = at + 4;
            let value = b.get(start..start + len).ok_or(StunError::Truncated)?;
            let next = start + len.next_multiple_of(4);
            if next > b.len() {
                return Err(StunError::Truncated);
            }
            let after_integrity = m.integrity.is_some();
            let first_time = !seen.contains(&ty);
            if first_time && seen.len() < MAX_ATTRIBUTES {
                seen.push(ty);
            }
            if ty == attr::FINGERPRINT {
                let v = be32(value, 0)
                    .filter(|_| len == 4)
                    .ok_or(StunError::BadAttribute(ty))?;
                m.fingerprint = Some((at, v));
            } else if after_integrity || !first_time {
                // Ignored (RFC 8489 sections 14 and 14.5).
            } else {
                m.read(ty, value)?;
                if ty == attr::MESSAGE_INTEGRITY {
                    m.integrity = Some((
                        at,
                        value.try_into().map_err(|_| StunError::BadAttribute(ty))?,
                    ));
                }
            }
            at = next;
        }
        Ok(m)
    }

    fn read(&mut self, ty: u16, v: &'a [u8]) -> Result<(), StunError> {
        let bad = StunError::BadAttribute(ty);
        match ty {
            attr::USERNAME => {
                if v.len() > MAX_USERNAME {
                    return Err(bad);
                }
                self.username = Some(v);
            }
            attr::MESSAGE_INTEGRITY => {
                if v.len() != 20 {
                    return Err(bad);
                }
            }
            attr::PRIORITY => self.priority = Some(be32(v, 0).filter(|_| v.len() == 4).ok_or(bad)?),
            attr::USE_CANDIDATE => {
                if !v.is_empty() {
                    return Err(bad);
                }
                self.use_candidate = true;
            }
            attr::ICE_CONTROLLING => self.ice_controlling = Some(be64(v).ok_or(bad)?),
            attr::ICE_CONTROLLED => self.ice_controlled = Some(be64(v).ok_or(bad)?),
            attr::XOR_MAPPED_ADDRESS => {
                self.xor_mapped = Some(decode_xor_address(v, &self.transaction).ok_or(bad)?)
            }
            attr::ERROR_CODE => {
                let class = u16::from(*v.get(2).ok_or(bad)? & 0x07);
                let number = u16::from(*v.get(3).ok_or(bad)?);
                if !(3..=6).contains(&class) || number > 99 {
                    return Err(bad);
                }
                self.error = Some((class * 100 + number, v.get(4..).unwrap_or_default()));
            }
            attr::UNKNOWN_ATTRIBUTES => {
                if !v.len().is_multiple_of(2) {
                    return Err(bad);
                }
                self.unknown_listed = v
                    .chunks_exact(2)
                    .filter_map(|c| be16(c, 0))
                    .take(MAX_UNKNOWN)
                    .collect();
            }
            t if t < 0x8000 => {
                if self.unknown_required.len() < MAX_UNKNOWN {
                    self.unknown_required.push(t);
                }
            }
            // Comprehension-optional and unknown (SOFTWARE, Chrome's
            // 0xC057): skipped.
            _ => {}
        }
        Ok(())
    }

    /// Whether MESSAGE-INTEGRITY is present and is the HMAC-SHA1, under
    /// `key`, of the message up to it with the length field ending at it
    /// (RFC 8489 section 14.5). Constant time in the HMAC comparison. `b`
    /// is the buffer this message was parsed from.
    pub fn check_integrity(&self, b: &[u8], key: &[u8]) -> bool {
        let Some((at, tag)) = self.integrity else {
            return false;
        };
        let Some(computed) = integrity(b, at, key) else {
            return false;
        };
        computed.ct_eq(&tag).into()
    }

    /// Whether FINGERPRINT is present and matches (RFC 8489 section 14.7).
    pub fn check_fingerprint(&self, b: &[u8]) -> bool {
        let Some((at, value)) = self.fingerprint else {
            return false;
        };
        b.get(..at)
            .is_some_and(|covered| crc32(covered) ^ FINGERPRINT_XOR == value)
    }
}

/// The HMAC-SHA1 over `b[..at]`, with the length field set to end after a
/// MESSAGE-INTEGRITY attribute placed at `at`.
fn integrity(b: &[u8], at: usize, key: &[u8]) -> Option<[u8; 20]> {
    let head = b.get(..2)?;
    let rest = b.get(4..at)?;
    let length = u16::try_from(at.checked_sub(HEADER_LEN)? + INTEGRITY_LEN).ok()?;
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(key).ok()?;
    mac.update(head);
    mac.update(&length.to_be_bytes());
    mac.update(rest);
    Some(mac.finalize().into_bytes().into())
}

/// XOR-MAPPED-ADDRESS value for `addr` (RFC 8489 section 14.2).
pub fn encode_xor_address(addr: SocketAddr, transaction: &[u8; 12]) -> Vec<u8> {
    let cookie = MAGIC_COOKIE.to_be_bytes();
    let port = addr.port() ^ (MAGIC_COOKIE >> 16) as u16;
    let mut v = vec![0];
    match addr.ip() {
        IpAddr::V4(ip) => {
            v.push(0x01);
            v.extend_from_slice(&port.to_be_bytes());
            v.extend(ip.octets().iter().zip(cookie).map(|(a, c)| a ^ c));
        }
        IpAddr::V6(ip) => {
            v.push(0x02);
            v.extend_from_slice(&port.to_be_bytes());
            let pad = cookie.iter().chain(transaction.iter());
            v.extend(ip.octets().iter().zip(pad).map(|(a, c)| a ^ c));
        }
    }
    v
}

fn decode_xor_address(v: &[u8], transaction: &[u8; 12]) -> Option<SocketAddr> {
    let cookie = MAGIC_COOKIE.to_be_bytes();
    let port = be16(v, 2)? ^ (MAGIC_COOKIE >> 16) as u16;
    let ip = match (*v.get(1)?, v.len()) {
        (0x01, 8) => {
            let mut o = [0u8; 4];
            for ((d, s), c) in o.iter_mut().zip(v.get(4..)?).zip(cookie) {
                *d = s ^ c;
            }
            IpAddr::V4(Ipv4Addr::from(o))
        }
        (0x02, 20) => {
            let mut o = [0u8; 16];
            let pad = cookie.iter().chain(transaction.iter());
            for ((d, s), c) in o.iter_mut().zip(v.get(4..)?).zip(pad) {
                *d = s ^ c;
            }
            IpAddr::V6(Ipv6Addr::from(o))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Builds a STUN message attribute by attribute, then signs and
/// fingerprints it.
#[derive(Debug, Clone)]
pub struct Builder {
    buf: Vec<u8>,
    pad: u8,
}

impl Builder {
    /// A message of this type and transaction.
    pub fn new(msg_type: u16, transaction: &[u8; 12]) -> Self {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&msg_type.to_be_bytes());
        buf.extend_from_slice(&[0, 0]);
        buf.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        buf.extend_from_slice(transaction);
        Self { buf, pad: 0 }
    }

    /// Sets the padding byte (zero by default). RFC 5769's vectors pad with
    /// spaces; receivers ignore padding.
    pub fn padding(mut self, pad: u8) -> Self {
        self.pad = pad;
        self
    }

    fn set_length(&mut self, extra: usize) {
        let len = self.buf.len() - HEADER_LEN + extra;
        let len = u16::try_from(len).unwrap_or(u16::MAX).to_be_bytes();
        if let Some(l) = self.buf.get_mut(2..4) {
            l.copy_from_slice(&len);
        }
    }

    /// Appends an attribute, padded to a multiple of 4. A value longer
    /// than 65535 bytes is cut short; nothing sapflow sends comes near.
    pub fn attribute(mut self, ty: u16, value: &[u8]) -> Self {
        let value = value.get(..usize::from(u16::MAX)).unwrap_or(value);
        self.buf.extend_from_slice(&ty.to_be_bytes());
        self.buf
            .extend_from_slice(&(value.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(value);
        let padded = self.buf.len().next_multiple_of(4);
        self.buf.resize(padded, self.pad);
        self.set_length(0);
        self
    }

    /// XOR-MAPPED-ADDRESS for `addr`.
    pub fn xor_mapped_address(self, addr: SocketAddr) -> Self {
        let transaction = self.transaction();
        self.attribute(
            attr::XOR_MAPPED_ADDRESS,
            &encode_xor_address(addr, &transaction),
        )
    }

    /// ERROR-CODE with a code from 300 to 699 and a reason phrase.
    pub fn error_code(self, code: u16, reason: &str) -> Self {
        let mut v = vec![0, 0, (code / 100) as u8 & 0x07, (code % 100) as u8];
        v.extend_from_slice(reason.as_bytes());
        self.attribute(attr::ERROR_CODE, &v)
    }

    /// UNKNOWN-ATTRIBUTES listing these types.
    pub fn unknown_attributes(self, types: &[u16]) -> Self {
        let v: Vec<u8> = types.iter().flat_map(|t| t.to_be_bytes()).collect();
        self.attribute(attr::UNKNOWN_ATTRIBUTES, &v)
    }

    fn transaction(&self) -> [u8; 12] {
        self.buf
            .get(8..HEADER_LEN)
            .and_then(|t| t.try_into().ok())
            .unwrap_or([0; 12])
    }

    /// Appends MESSAGE-INTEGRITY under `key`.
    pub fn integrity(mut self, key: &[u8]) -> Self {
        let at = self.buf.len();
        self.set_length(INTEGRITY_LEN);
        let tag = integrity(&self.buf, at, key).unwrap_or([0; 20]);
        self.buf
            .extend_from_slice(&attr::MESSAGE_INTEGRITY.to_be_bytes());
        self.buf.extend_from_slice(&20u16.to_be_bytes());
        self.buf.extend_from_slice(&tag);
        self
    }

    /// Appends FINGERPRINT and returns the message.
    pub fn fingerprint(mut self) -> Vec<u8> {
        self.set_length(FINGERPRINT_LEN);
        let crc = crc32(&self.buf) ^ FINGERPRINT_XOR;
        self.buf.extend_from_slice(&attr::FINGERPRINT.to_be_bytes());
        self.buf.extend_from_slice(&4u16.to_be_bytes());
        self.buf.extend_from_slice(&crc.to_be_bytes());
        self.buf
    }

    /// The message as built so far, without FINGERPRINT.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// CRC-32 as used by FINGERPRINT (ISO 3309 and ITU-T V.42, the reflected
/// polynomial 0xEDB88320), from a table made at compile time.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in data {
        let i = usize::from((crc as u8) ^ byte);
        crc = (crc >> 8) ^ CRC_TABLE.get(i).copied().unwrap_or(0);
    }
    !crc
}

#[allow(clippy::indexing_slicing)] // Compile-time only, within bounds.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// The RFC 5769 section 2 test vectors, for the tests and the fuzz seeds.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
#[allow(missing_docs)]
pub mod tests_vectors {
    // RFC 5769 section 2.
    pub const PASSWORD: &[u8] = b"VOkJxbRl1RmTxUk/WvJxBt";
    pub const TID: [u8; 12] = [
        0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae,
    ];
    pub const REQUEST: &str =
        "000100582112a442b7e7a701bc34d686fa87dfae802200105354554e20746573742063
        6c69656e74002400046e0001ff80290008932ff9b151263b36000600096576746a3a6836765920202000
        0800149aeaa70cbfd8cb56781ef2b5b2d3f249c1b571a280280004e57a3bcf";
    pub const RESPONSE_V4: &str =
        "0101003c2112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220
        002000080001a147e112a643000800142b91f599fd9e90c38c7489f92af9ba53f06be7d780280004c07d4c96";
    pub const RESPONSE_V6: &str =
        "010100482112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220
        002000140002a1470113a9faa5d3f179bc25f4b5bed2b9d9
        00080014a382954e4be67bf11784c97c8292c275bfe3ed4180280004c8fb0b4c";
    pub const LONG_TERM: &str = "000100602112a44278ad3433c6ad72c029da412e
        00060012e3839ee38388e383aae38383e382afe382b90000
        0015001c662f2f3439396b39353464364f4c33346f4c39465354767936347341
        0014000b6578616d706c652e6f726700
        00080014f67024656dd64a3e02b8e0712e85c9a28ca89666";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("parses"))
            .collect()
    }

    use super::tests_vectors::*;

    #[test]
    fn rfc5769_sample_request() {
        let b = unhex(REQUEST);
        let m = Message::parse(&b).expect("parses");
        assert_eq!(m.msg_type, BINDING_REQUEST);
        assert_eq!(m.transaction, TID);
        assert_eq!(m.username, Some(&b"evtj:h6vY"[..]));
        assert_eq!(m.priority, Some(0x6e00_01ff));
        assert_eq!(m.ice_controlled, Some(0x932f_f9b1_5126_3b36));
        assert!(m.ice_controlling.is_none() && !m.use_candidate);
        assert!(m.unknown_required.is_empty(), "SOFTWARE is optional");
        assert!(m.check_fingerprint(&b));
        assert!(m.check_integrity(&b, PASSWORD));
        assert!(!m.check_integrity(&b, b"VOkJxbRl1RmTxUk/WvJxBu"));
        assert_eq!(request_username(&b), Some(&b"evtj:h6vY"[..]));
    }

    #[test]
    fn rfc5769_sample_request_flipped_bit() {
        let mut b = unhex(REQUEST);
        b[30] ^= 1;
        let m = Message::parse(&b).expect("parses");
        assert!(!m.check_fingerprint(&b));
        assert!(!m.check_integrity(&b, PASSWORD));
    }

    #[test]
    fn rfc5769_ipv4_response() {
        let b = unhex(RESPONSE_V4);
        let m = Message::parse(&b).expect("parses");
        assert_eq!(m.msg_type, BINDING_SUCCESS);
        let addr: SocketAddr = "192.0.2.1:32853".parse().expect("parses");
        assert_eq!(m.xor_mapped, Some(addr));
        assert!(m.check_fingerprint(&b) && m.check_integrity(&b, PASSWORD));
        let rebuilt = Builder::new(BINDING_SUCCESS, &TID)
            .padding(0x20)
            .attribute(attr::SOFTWARE, b"test vector")
            .xor_mapped_address(addr)
            .integrity(PASSWORD)
            .fingerprint();
        assert_eq!(rebuilt, b);
    }

    #[test]
    fn rfc5769_ipv6_response() {
        let b = unhex(RESPONSE_V6);
        let m = Message::parse(&b).expect("parses");
        let addr: SocketAddr = "[2001:db8:1234:5678:11:2233:4455:6677]:32853"
            .parse()
            .expect("parses");
        assert_eq!(m.xor_mapped, Some(addr));
        assert!(m.check_fingerprint(&b) && m.check_integrity(&b, PASSWORD));
        let rebuilt = Builder::new(BINDING_SUCCESS, &TID)
            .padding(0x20)
            .attribute(attr::SOFTWARE, b"test vector")
            .xor_mapped_address(addr)
            .integrity(PASSWORD)
            .fingerprint();
        assert_eq!(rebuilt, b);
    }

    #[test]
    fn rfc5769_long_term_request() {
        // The key is MD5(username ":" realm ":" SASLprep(password)), worked
        // out once from the vector's values; this crate never uses
        // long-term credentials, but the MESSAGE-INTEGRITY check is the
        // same HMAC.
        let b = unhex(LONG_TERM);
        let m = Message::parse(&b).expect("parses");
        assert_eq!(
            m.username,
            Some("\u{30DE}\u{30C8}\u{30EA}\u{30C3}\u{30AF}\u{30B9}".as_bytes())
        );
        let key = unhex("e8ca7ad59d5eb0518e312911d2dab2a9");
        assert!(m.check_integrity(&b, &key));
        assert!(!m.check_integrity(&b, PASSWORD));
        assert!(m.fingerprint.is_none());
        // NONCE (0x0015) and REALM (0x0014) are comprehension-required
        // and outside the subset, so they are listed for a 420.
        assert_eq!(m.unknown_required, [0x0015, 0x0014]);
    }

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn framing_refusals() {
        let b = unhex(REQUEST);
        assert_eq!(Message::parse(&b[..19]), Err(StunError::NotStun));
        let mut c = b.clone();
        c[0] |= 0x40;
        assert_eq!(Message::parse(&c), Err(StunError::NotStun));
        let mut c = b.clone();
        c[4] ^= 1;
        assert_eq!(Message::parse(&c), Err(StunError::Cookie));
        assert_eq!(Message::parse(&b[..b.len() - 4]), Err(StunError::Length));
        let mut c = b.clone();
        c[3] += 2;
        c.extend([0, 0]);
        assert_eq!(Message::parse(&c), Err(StunError::Length));
        // An attribute after FINGERPRINT.
        let mut c = b.clone();
        c.extend([0x80, 0x22, 0, 0]);
        let len = (c.len() - HEADER_LEN) as u16;
        c[2..4].copy_from_slice(&len.to_be_bytes());
        assert_eq!(Message::parse(&c), Err(StunError::AfterFingerprint));
        // An attribute whose length runs past the end.
        let mut c = Builder::new(BINDING_REQUEST, &TID).finish();
        c.extend([0, 6, 0, 9, b'a']);
        c.resize(c.len() + 3, 0);
        let len = (c.len() - HEADER_LEN) as u16;
        c[2..4].copy_from_slice(&len.to_be_bytes());
        assert_eq!(Message::parse(&c), Err(StunError::Truncated));
    }

    #[test]
    fn every_prefix_and_bit_flip_is_safe() {
        for vector in [REQUEST, RESPONSE_V4, RESPONSE_V6, LONG_TERM] {
            let b = unhex(vector);
            for n in 0..b.len() {
                let _ = Message::parse(&b[..n]);
            }
            for i in 0..b.len() * 8 {
                let mut c = b.clone();
                c[i / 8] ^= 1 << (i % 8);
                if let Ok(m) = Message::parse(&c) {
                    let _ = m.check_integrity(&c, PASSWORD);
                    let _ = m.check_fingerprint(&c);
                }
            }
        }
    }

    #[test]
    fn attributes_after_integrity_are_ignored() {
        let b = Builder::new(BINDING_REQUEST, &TID)
            .attribute(attr::USERNAME, b"abcd:efgh")
            .integrity(b"key")
            .attribute(attr::USE_CANDIDATE, &[])
            .attribute(0x0033, b"x")
            .fingerprint();
        let m = Message::parse(&b).expect("parses");
        assert!(!m.use_candidate, "after MESSAGE-INTEGRITY");
        assert!(m.unknown_required.is_empty());
        assert!(m.check_integrity(&b, b"key") && m.check_fingerprint(&b));
    }

    #[test]
    fn repeated_attribute_reads_the_first() {
        let b = Builder::new(BINDING_REQUEST, &TID)
            .attribute(attr::USERNAME, b"first:one")
            .attribute(attr::USERNAME, b"second:two")
            .fingerprint();
        let m = Message::parse(&b).expect("parses");
        assert_eq!(m.username, Some(&b"first:one"[..]));
    }

    #[test]
    fn error_code_round_trip() {
        let b = Builder::new(BINDING_ERROR, &TID)
            .error_code(487, "Role Conflict")
            .unknown_attributes(&[0x0033, 0x0044, 0x0055])
            .integrity(b"k")
            .fingerprint();
        let m = Message::parse(&b).expect("parses");
        assert_eq!(m.error, Some((487, &b"Role Conflict"[..])));
        assert_eq!(m.unknown_listed, [0x0033, 0x0044, 0x0055]);
        assert!(m.check_integrity(&b, b"k"));
    }

    #[test]
    fn optional_unknowns_skipped_required_listed() {
        let b = Builder::new(BINDING_REQUEST, &TID)
            .attribute(0xc057, &[0, 1, 0, 0])
            .attribute(0x0031, b"zz")
            .fingerprint();
        let m = Message::parse(&b).expect("parses");
        assert_eq!(m.unknown_required, [0x0031]);
    }

    #[test]
    fn looks_like_stun_needs_the_cookie() {
        let b = unhex(REQUEST);
        assert!(looks_like_stun(&b));
        assert!(!looks_like_stun(&b[..10]));
        let mut c = b.clone();
        c[5] ^= 1;
        assert!(!looks_like_stun(&c));
    }
}
