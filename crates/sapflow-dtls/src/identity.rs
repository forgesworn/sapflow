//! Certificates: our self-signed ECDSA P-256 certificate, SHA-256
//! fingerprints (RFC 8122 section 5) and the peer's certificate key.
//!
//! WebRTC authenticates the DTLS peer by the fingerprint signalled in SDP,
//! not by a chain (RFC 8827 section 6.5), so certificate dates, issuers,
//! extensions and the self-signature are not checked. The peer's
//! certificate is accepted only if the SHA-256 of its DER matches the
//! expected fingerprint, which is compared before the DER is parsed.

use crate::Error;
use crate::crypto::{Entropy, sha256};
use der::Decode;
use der::Encode;
use der::asn1::{ObjectIdentifier, UtcTime};
use p256::ecdsa::{SigningKey, VerifyingKey};
use std::fmt;
use std::time::Duration;
use subtle::ConstantTimeEq;
use x509_cert::Certificate;

/// `id-ecPublicKey` (RFC 5480).
const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
/// `secp256r1` / `prime256v1` (RFC 5480).
const SECP256R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");

/// A SHA-256 certificate fingerprint, as in `a=fingerprint:sha-256`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// The fingerprint of a DER certificate.
    pub fn of_certificate(der: &[u8]) -> Self {
        Self(sha256(&[der]))
    }

    /// A fingerprint from its 32 bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The 32 bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parses the value of an SDP `sha-256` fingerprint: 32 colon-separated
    /// pairs of hex digits, upper or lower case (RFC 8122 section 5). The
    /// hash function token is the caller's to check.
    pub fn parse_sdp(value: &str) -> Option<Self> {
        let mut out = [0u8; 32];
        let mut pairs = value.split(':');
        for byte in &mut out {
            let pair = pairs.next()?.as_bytes();
            let [hi, lo] = <[u8; 2]>::try_from(pair).ok()?;
            *byte = (hex_digit(hi)? << 4) | hex_digit(lo)?;
        }
        pairs.next().is_none().then_some(Self(out))
    }

    /// The SDP form: upper-case hex pairs separated by colons.
    pub fn to_sdp(&self) -> String {
        let mut s = String::with_capacity(95);
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                s.push(':');
            }
            s.push_str(&format!("{b:02X}"));
        }
        s
    }

    /// Constant-time equality.
    pub fn ct_eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_sdp())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// Our certificate and its private key. Generate one per process and share
/// it between sessions; its fingerprint goes in every SDP answer. The
/// private key is zeroed on drop, and `Debug` shows only the fingerprint.
pub struct Identity {
    key: SigningKey,
    certificate: Vec<u8>,
    fingerprint: Fingerprint,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// A new ECDSA P-256 key and a self-signed certificate for it, valid
    /// from a day before `unix_time` to 366 days after it. Peers do not
    /// check the dates, and the server makes a new identity each time it
    /// starts; the year is margin for a long-running process. `unix_time`
    /// is the wall clock, passed in so the crate keeps no clock of its own.
    pub fn generate(unix_time: Duration) -> Result<Self, Error> {
        Self::generate_with(&mut Entropy::System, unix_time)
    }

    /// As [`Identity::generate`], from a seed, so tests and fuzzing replay
    /// byte for byte. Not for production use.
    #[cfg(any(test, feature = "testing"))]
    pub fn generate_seeded(seed: [u8; 32], unix_time: Duration) -> Result<Self, Error> {
        Self::generate_with(
            &mut Entropy::Seeded {
                seed: zeroize::Zeroizing::new(seed),
                counter: 0,
            },
            unix_time,
        )
    }

    pub(crate) fn generate_with(entropy: &mut Entropy, unix_time: Duration) -> Result<Self, Error> {
        let key = signing_key(entropy)?;
        let mut serial = *entropy.array::<16>()?;
        // Positive and minimally encoded: top bit clear, next bit set.
        if let Some(first) = serial.first_mut() {
            *first = (*first & 0x7f) | 0x40;
        }
        let certificate = self_signed(&key, &serial, unix_time)?;
        let fingerprint = Fingerprint::of_certificate(&certificate);
        Ok(Self {
            key,
            certificate,
            fingerprint,
        })
    }

    /// Our certificate, DER.
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate
    }

    /// The SHA-256 fingerprint of [`Identity::certificate_der`].
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    pub(crate) fn key(&self) -> &SigningKey {
        &self.key
    }
}

/// A random P-256 signing key. A 32-byte draw is outside the scalar range
/// with probability about 2^-32; a few draws are allowed before giving up.
pub(crate) fn signing_key(entropy: &mut Entropy) -> Result<SigningKey, Error> {
    for _ in 0..4 {
        let bytes = entropy.array::<32>()?;
        if let Ok(k) = SigningKey::from_slice(bytes.as_slice()) {
            return Ok(k);
        }
    }
    Err(Error::Entropy)
}

/// A DER TLV with a definite length.
fn tlv(tag: u8, content: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(content.len() + 4);
    out.push(tag);
    let n = content.len();
    match n {
        0..=0x7f => out.push(u8::try_from(n).map_err(|_| Error::Internal)?),
        0x80..=0xff => {
            out.extend_from_slice(&[0x81, u8::try_from(n).map_err(|_| Error::Internal)?])
        }
        0x100..=0xffff => {
            out.push(0x82);
            out.extend_from_slice(&u16::try_from(n).map_err(|_| Error::Internal)?.to_be_bytes());
        }
        _ => return Err(Error::Internal),
    }
    out.extend_from_slice(content);
    Ok(out)
}

fn seq(parts: &[&[u8]]) -> Result<Vec<u8>, Error> {
    tlv(0x30, &parts.concat())
}

fn oid(o: &ObjectIdentifier) -> Result<Vec<u8>, Error> {
    o.to_der().map_err(|_| Error::Internal)
}

fn utc(t: Duration) -> Result<Vec<u8>, Error> {
    // UTCTime holds 1950 to 2049; clamp a clock outside that, since the
    // dates are not checked.
    const MAX: u64 = 2_524_607_999; // 2049-12-31T23:59:59Z
    let secs = t.as_secs().clamp(0, MAX);
    UtcTime::from_unix_duration(Duration::from_secs(secs))
        .and_then(|u| u.to_der())
        .map_err(|_| Error::Internal)
}

/// A v3 certificate with no extensions, subject and issuer `CN=sapflow`,
/// signed with ecdsa-with-SHA256 (RFC 5758 section 3.2, no parameters).
fn self_signed(key: &SigningKey, serial: &[u8], unix_time: Duration) -> Result<Vec<u8>, Error> {
    let ecdsa_sha256 = seq(&[&oid(&ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2"))?])?;
    let cn = ObjectIdentifier::new_unwrap("2.5.4.3");
    let name = seq(&[&tlv(0x31, &seq(&[&oid(&cn)?, &tlv(0x0c, b"sapflow")?])?)?])?;
    let day = Duration::from_secs(86_400);
    let validity = seq(&[
        &utc(unix_time.saturating_sub(day))?,
        &utc(unix_time.saturating_add(366 * day))?,
    ])?;
    let point = key.verifying_key().to_sec1_point(false);
    let mut bits = vec![0u8];
    bits.extend_from_slice(point.as_bytes());
    let spki = seq(&[
        &seq(&[&oid(&ID_EC_PUBLIC_KEY)?, &oid(&SECP256R1)?])?,
        &tlv(0x03, &bits)?,
    ])?;
    let version = tlv(0xa0, &tlv(0x02, &[2])?)?;
    let tbs = seq(&[
        &version,
        &tlv(0x02, serial)?,
        &ecdsa_sha256,
        &name,
        &validity,
        &name,
        &spki,
    ])?;
    let sig = sign_der(key, &sha256(&[&tbs]))?;
    let mut sig_bits = vec![0u8];
    sig_bits.extend_from_slice(&sig);
    seq(&[&tbs, &ecdsa_sha256, &tlv(0x03, &sig_bits)?])
}

/// An ECDSA signature over a SHA-256 digest, DER-encoded (RFC 8422
/// section 5.4). Deterministic (RFC 6979), so no randomness is needed.
pub(crate) fn sign_der(key: &SigningKey, digest: &[u8; 32]) -> Result<Vec<u8>, Error> {
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    let sig: p256::ecdsa::DerSignature = key.sign_prehash(digest).map_err(|_| Error::Internal)?;
    Ok(sig.as_bytes().to_vec())
}

/// Verifies a DER ECDSA signature over a SHA-256 digest.
pub(crate) fn verify_der(
    key: &VerifyingKey,
    digest: &[u8; 32],
    der_sig: &[u8],
) -> Result<(), Error> {
    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    let sig = p256::ecdsa::DerSignature::from_bytes(der_sig).map_err(|_| Error::Decode)?;
    key.verify_prehash(digest, &sig)
        .map_err(|_| Error::BadSignature)
}

/// The leaf certificate's key, after its fingerprint has matched. Only an
/// ECDSA P-256 key is accepted.
pub(crate) fn peer_key(der: &[u8]) -> Result<VerifyingKey, Error> {
    let cert = Certificate::from_der(der).map_err(|_| Error::BadCertificate)?;
    let spki = cert.tbs_certificate().subject_public_key_info();
    if spki.algorithm.oid != ID_EC_PUBLIC_KEY {
        return Err(Error::UnsupportedCertificate);
    }
    let curve = spki
        .algorithm
        .parameters
        .as_ref()
        .and_then(|p| p.decode_as::<ObjectIdentifier>().ok());
    if curve != Some(SECP256R1) {
        return Err(Error::UnsupportedCertificate);
    }
    let point = spki
        .subject_public_key
        .as_bytes()
        .ok_or(Error::BadCertificate)?;
    VerifyingKey::from_sec1_bytes(point).map_err(|_| Error::BadCertificate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_certificate_parses_and_self_verifies() {
        let id = Identity::generate_seeded([3; 32], Duration::from_secs(1_793_000_000))
            .expect("identity");
        let key = peer_key(id.certificate_der()).expect("our certificate parses");
        assert_eq!(&key, id.key().verifying_key());
        let cert = Certificate::from_der(id.certificate_der()).expect("x509");
        let tbs = cert.tbs_certificate().to_der().expect("tbs");
        let sig = cert.signature().as_bytes().expect("bits");
        verify_der(&key, &sha256(&[&tbs]), sig).expect("self-signature verifies");
        assert_eq!(
            id.fingerprint(),
            Fingerprint::of_certificate(id.certificate_der())
        );
        let again = Identity::generate_seeded([3; 32], Duration::from_secs(1_793_000_000))
            .expect("identity");
        assert_eq!(
            again.certificate_der(),
            id.certificate_der(),
            "seeded is reproducible"
        );
        assert!(!format!("{id:?}").contains("key:"));
        let v = cert.tbs_certificate().validity();
        let span = v.not_after.to_unix_duration() - v.not_before.to_unix_duration();
        assert_eq!(
            span,
            Duration::from_secs(367 * 86_400),
            "a day before to a year after"
        );
        assert_eq!(
            v.not_before.to_unix_duration(),
            Duration::from_secs(1_793_000_000 - 86_400)
        );
    }

    #[test]
    fn far_clocks_still_give_a_certificate() {
        for t in [0, u64::MAX / 4] {
            let id = Identity::generate_seeded([4; 32], Duration::from_secs(t)).expect("identity");
            peer_key(id.certificate_der()).expect("parses");
        }
    }

    #[test]
    fn fingerprint_sdp_round_trip() {
        let f = Fingerprint::from_bytes(core::array::from_fn(|i| i as u8 * 7));
        let s = f.to_sdp();
        assert_eq!(s.len(), 95);
        assert_eq!(Fingerprint::parse_sdp(&s), Some(f));
        assert_eq!(Fingerprint::parse_sdp(&s.to_lowercase()), Some(f));
        assert_eq!(Fingerprint::parse_sdp(&s[..92]), None);
        assert_eq!(Fingerprint::parse_sdp(&format!("{s}:00")), None);
        assert_eq!(Fingerprint::parse_sdp(&s.replace(':', "-")), None);
        assert_eq!(Fingerprint::parse_sdp(&s.replacen('0', "g", 1)), None);
        assert_eq!(Fingerprint::parse_sdp(""), None);
        assert!(f.ct_eq(&f));
        assert!(!f.ct_eq(&Fingerprint::from_bytes([0; 32])));
    }

    #[test]
    fn peer_key_refuses_garbage() {
        assert_eq!(peer_key(&[]), Err(Error::BadCertificate));
        assert_eq!(
            peer_key(&[0x30, 0x03, 0x02, 0x01, 0x01]),
            Err(Error::BadCertificate)
        );
    }
}
