//! Handshake messages: the DTLS handshake header (RFC 6347 section 4.2.2),
//! the parsers for what a client sends (ClientHello, Certificate,
//! ClientKeyExchange, CertificateVerify, Finished) and the builders for
//! what the server sends.
//!
//! Parsers take a complete, reassembled message body and must consume it
//! exactly; anything left over is a decode error.

use crate::Error;
use crate::codec::{Reader, put_u16, put_u24, put_vec8, put_vec16, put_vec24};
use crate::record::{DTLS_1_0, DTLS_1_2};

/// Handshake header length.
pub const HEADER_LEN: usize = 12;

/// `client_hello`.
pub const CLIENT_HELLO: u8 = 1;
/// `server_hello`.
pub const SERVER_HELLO: u8 = 2;
/// `hello_verify_request`.
pub const HELLO_VERIFY_REQUEST: u8 = 3;
/// `certificate`.
pub const CERTIFICATE: u8 = 11;
/// `server_key_exchange`.
pub const SERVER_KEY_EXCHANGE: u8 = 12;
/// `certificate_request`.
pub const CERTIFICATE_REQUEST: u8 = 13;
/// `server_hello_done`.
pub const SERVER_HELLO_DONE: u8 = 14;
/// `certificate_verify`.
pub const CERTIFICATE_VERIFY: u8 = 15;
/// `client_key_exchange`.
pub const CLIENT_KEY_EXCHANGE: u8 = 16;
/// `finished`.
pub const FINISHED: u8 = 20;

/// `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (RFC 5289).
pub const SUITE: u16 = 0xc02b;
/// `TLS_EMPTY_RENEGOTIATION_INFO_SCSV` (RFC 5746).
pub const RENEGOTIATION_SCSV: u16 = 0x00ff;
/// `secp256r1` (RFC 8422).
pub const SECP256R1: u16 = 0x0017;
/// `ecdsa_secp256r1_sha256`: hash sha256 (4), signature ecdsa (3).
pub const ECDSA_P256_SHA256: u16 = 0x0403;
/// `ecdsa_sign` client certificate type (RFC 8422 section 5.5).
pub const ECDSA_SIGN: u8 = 64;
/// `named_curve` ECCurveType (RFC 8422 section 5.4).
pub const NAMED_CURVE: u8 = 3;

/// Extension types sapflow reads.
pub mod ext {
    /// `supported_groups` (`elliptic_curves`).
    pub const SUPPORTED_GROUPS: u16 = 10;
    /// `ec_point_formats`.
    pub const EC_POINT_FORMATS: u16 = 11;
    /// `signature_algorithms`.
    pub const SIGNATURE_ALGORITHMS: u16 = 13;
    /// `use_srtp`.
    pub const USE_SRTP: u16 = 14;
    /// `extended_master_secret`.
    pub const EXTENDED_MASTER_SECRET: u16 = 23;
    /// `renegotiation_info`.
    pub const RENEGOTIATION_INFO: u16 = 0xff01;
}

/// A DTLS-SRTP protection profile sapflow negotiates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SrtpProfile {
    /// `SRTP_AES128_CM_HMAC_SHA1_80` (RFC 5764), `0x0001`.
    Aes128CmHmacSha1_80,
    /// `SRTP_AEAD_AES_128_GCM` (RFC 7714), `0x0007`.
    AeadAes128Gcm,
}

impl SrtpProfile {
    /// The `use_srtp` identifier.
    pub const fn id(self) -> u16 {
        match self {
            Self::Aes128CmHmacSha1_80 => 0x0001,
            Self::AeadAes128Gcm => 0x0007,
        }
    }

    /// Exporter output length: both directions' master keys and salts
    /// (RFC 5764 section 4.2), 60 for CM-80 and 56 for GCM.
    pub const fn keying_material_len(self) -> usize {
        match self {
            Self::Aes128CmHmacSha1_80 => 2 * (16 + 14),
            Self::AeadAes128Gcm => 2 * (16 + 12),
        }
    }
}

/// One handshake fragment from a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment<'a> {
    /// `msg_type`.
    pub msg_type: u8,
    /// Total message length.
    pub length: u32,
    /// `message_seq`.
    pub seq: u16,
    /// `fragment_offset`.
    pub offset: u32,
    /// The fragment's bytes.
    pub data: &'a [u8],
}

/// The handshake fragments in a record body, in order. Stops at the first
/// truncated header or body, or a fragment that runs past its message's
/// declared length.
pub fn fragments(body: &[u8]) -> impl Iterator<Item = Fragment<'_>> {
    let mut r = Reader::new(body);
    std::iter::from_fn(move || {
        let msg_type = r.u8()?;
        let length = r.u24()?;
        let seq = r.u16()?;
        let offset = r.u24()?;
        let frag_len = r.u24()?;
        if u64::from(offset) + u64::from(frag_len) > u64::from(length) {
            return None;
        }
        let data = r.bytes(usize::try_from(frag_len).ok()?)?;
        Some(Fragment {
            msg_type,
            length,
            seq,
            offset,
            data,
        })
    })
}

/// The 12-byte header of an unfragmented message, as hashed into the
/// transcript (RFC 6347 section 4.2.6) and sent when it fits.
pub(crate) fn header(
    msg_type: u8,
    seq: u16,
    offset: usize,
    frag_len: usize,
    total: usize,
) -> Result<[u8; 12], Error> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.push(msg_type);
    let n = |v: usize| u32::try_from(v).map_err(|_| Error::Internal);
    put_u24(&mut h, n(total)?)?;
    put_u16(&mut h, seq);
    put_u24(&mut h, n(offset)?)?;
    put_u24(&mut h, n(frag_len)?)?;
    h.try_into().map_err(|_| Error::Internal)
}

/// What sapflow read from a `use_srtp` extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SrtpOffer {
    /// `SRTP_AEAD_AES_128_GCM` offered.
    pub aead_aes_128_gcm: bool,
    /// `SRTP_AES128_CM_HMAC_SHA1_80` offered.
    pub aes128_cm_sha1_80: bool,
    /// Length of `srtp_mki`.
    pub mki_len: usize,
}

/// A parsed ClientHello. Only what negotiation needs is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHello<'a> {
    /// `client_version`.
    pub version: u16,
    /// `random`.
    pub random: [u8; 32],
    /// `session_id`, ignored: sapflow does not resume sessions.
    pub session_id: &'a [u8],
    /// `cookie`.
    pub cookie: &'a [u8],
    /// `cipher_suites`, an even number of bytes.
    pub cipher_suites: &'a [u8],
    /// `compression_methods`.
    pub compression_methods: &'a [u8],
    /// `extended_master_secret` present.
    pub extended_master_secret: bool,
    /// `renegotiation_info` present, with the length of
    /// `renegotiated_connection`.
    pub renegotiation_info: Option<usize>,
    /// `supported_groups` present, and whether it lists `secp256r1`.
    pub secp256r1: Option<bool>,
    /// `ec_point_formats` present, and whether it lists `uncompressed`.
    pub uncompressed_points: Option<bool>,
    /// `signature_algorithms` present, and whether it lists
    /// `ecdsa_secp256r1_sha256`.
    pub ecdsa_p256_sha256: Option<bool>,
    /// `use_srtp`.
    pub use_srtp: Option<SrtpOffer>,
}

fn u16s(list: &[u8]) -> impl Iterator<Item = u16> + '_ {
    list.chunks_exact(2)
        .filter_map(|c| <[u8; 2]>::try_from(c).ok())
        .map(u16::from_be_bytes)
}

/// A `uint16` list with a 2-byte length prefix and at least one entry.
fn u16_list(data: &[u8]) -> Result<&[u8], Error> {
    let mut r = Reader::new(data);
    let list = r.vec16().ok_or(Error::Decode)?;
    if !r.is_empty() || list.is_empty() || list.len() % 2 != 0 {
        return Err(Error::Decode);
    }
    Ok(list)
}

impl<'a> ClientHello<'a> {
    /// Parses a ClientHello body (RFC 6347 section 4.2.1 with the
    /// extensions of RFC 5246 section 7.4.1.2). Unknown extensions are
    /// skipped; known ones must be well formed and appear once.
    pub fn parse(body: &'a [u8]) -> Result<Self, Error> {
        let mut r = Reader::new(body);
        let d = Error::Decode;
        let version = r.u16().ok_or(d)?;
        let random = r.array::<32>().ok_or(d)?;
        let session_id = r.vec8().ok_or(d)?;
        let cookie = r.vec8().ok_or(d)?;
        let cipher_suites = r.vec16().ok_or(d)?;
        let compression_methods = r.vec8().ok_or(d)?;
        if session_id.len() > 32
            || cipher_suites.is_empty()
            || cipher_suites.len() % 2 != 0
            || compression_methods.is_empty()
        {
            return Err(d);
        }
        let mut hello = Self {
            version,
            random,
            session_id,
            cookie,
            cipher_suites,
            compression_methods,
            extended_master_secret: false,
            renegotiation_info: None,
            secp256r1: None,
            uncompressed_points: None,
            ecdsa_p256_sha256: None,
            use_srtp: None,
        };
        if r.is_empty() {
            return Ok(hello);
        }
        let mut extensions = Reader::new(r.vec16().ok_or(d)?);
        if !r.is_empty() {
            return Err(d);
        }
        // Each extension is at least four bytes, so a 64 KB block holds at
        // most 16384 of them.
        let mut seen: Vec<u16> = Vec::new();
        while !extensions.is_empty() {
            let kind = extensions.u16().ok_or(d)?;
            let data = extensions.vec16().ok_or(d)?;
            seen.push(kind);
            hello.extension(kind, data)?;
        }
        seen.sort_unstable();
        if seen.windows(2).any(|w| w.first() == w.get(1)) {
            return Err(Error::DuplicateExtension);
        }
        Ok(hello)
    }

    fn extension(&mut self, kind: u16, data: &'a [u8]) -> Result<(), Error> {
        let d = Error::Decode;
        match kind {
            ext::EXTENDED_MASTER_SECRET => {
                if !data.is_empty() {
                    return Err(d);
                }
                self.extended_master_secret = true;
            }
            ext::RENEGOTIATION_INFO => {
                let mut r = Reader::new(data);
                let conn = r.vec8().ok_or(d)?;
                if !r.is_empty() {
                    return Err(d);
                }
                self.renegotiation_info = Some(conn.len());
            }
            ext::SUPPORTED_GROUPS => {
                self.secp256r1 = Some(u16s(u16_list(data)?).any(|g| g == SECP256R1));
            }
            ext::SIGNATURE_ALGORITHMS => {
                self.ecdsa_p256_sha256 =
                    Some(u16s(u16_list(data)?).any(|s| s == ECDSA_P256_SHA256));
            }
            ext::EC_POINT_FORMATS => {
                let mut r = Reader::new(data);
                let list = r.vec8().ok_or(d)?;
                if !r.is_empty() || list.is_empty() {
                    return Err(d);
                }
                self.uncompressed_points = Some(list.contains(&0));
            }
            ext::USE_SRTP => {
                let mut r = Reader::new(data);
                let profiles = r.vec16().ok_or(d)?;
                let mki = r.vec8().ok_or(d)?;
                if !r.is_empty() || profiles.is_empty() || profiles.len() % 2 != 0 {
                    return Err(d);
                }
                let mut offer = SrtpOffer {
                    mki_len: mki.len(),
                    ..SrtpOffer::default()
                };
                for p in u16s(profiles) {
                    match p {
                        0x0001 => offer.aes128_cm_sha1_80 = true,
                        0x0007 => offer.aead_aes_128_gcm = true,
                        _ => {}
                    }
                }
                self.use_srtp = Some(offer);
            }
            // supported_versions, key_share and everything else: ignored.
            _ => {}
        }
        Ok(())
    }

    fn offers_suite(&self, suite: u16) -> bool {
        u16s(self.cipher_suites).any(|s| s == suite)
    }

    /// Checks the hello against the profile, in a fixed order, and returns
    /// what the ServerHello will say.
    pub fn negotiate(&self) -> Result<Negotiated, Error> {
        // DTLS versions count down: 0xfefd is 1.2, 0xfefc is 1.3. Anything
        // at or above 1.2 is answered with 1.2 (RFC 5246 appendix E.1).
        let [major, minor] = self.version.to_be_bytes();
        if major != 0xfe || minor > 0xfd {
            return Err(Error::Version);
        }
        if !self.compression_methods.contains(&0) {
            return Err(Error::Compression);
        }
        if !self.offers_suite(SUITE) {
            return Err(Error::NoCipherSuite);
        }
        match self.renegotiation_info {
            Some(0) => {}
            Some(_) => return Err(Error::RenegotiationInfo),
            None if self.offers_suite(RENEGOTIATION_SCSV) => {}
            None => return Err(Error::NoRenegotiationSignal),
        }
        if !self.extended_master_secret {
            return Err(Error::NoExtendedMasterSecret);
        }
        if self.secp256r1 != Some(true) {
            return Err(Error::NoGroup);
        }
        if self.uncompressed_points == Some(false) {
            return Err(Error::PointFormat);
        }
        if self.ecdsa_p256_sha256 != Some(true) {
            return Err(Error::NoSignatureScheme);
        }
        let srtp = self.use_srtp.ok_or(Error::NoSrtpProfile)?;
        if srtp.mki_len != 0 {
            return Err(Error::SrtpMki);
        }
        let profile = if srtp.aead_aes_128_gcm {
            SrtpProfile::AeadAes128Gcm
        } else if srtp.aes128_cm_sha1_80 {
            SrtpProfile::Aes128CmHmacSha1_80
        } else {
            return Err(Error::NoSrtpProfile);
        };
        Ok(Negotiated {
            profile,
            point_formats: self.uncompressed_points.is_some(),
        })
    }
}

/// The result of negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiated {
    /// The SRTP profile chosen.
    pub profile: SrtpProfile,
    /// Whether the client sent `ec_point_formats`, so the ServerHello
    /// answers it (RFC 8422 section 5.2).
    pub point_formats: bool,
}

/// The leaf of a Certificate message (RFC 5246 section 7.4.2), which
/// comes first. A chain longer than `max_chain` is refused.
pub fn certificate_leaf(body: &[u8], max_chain: usize) -> Result<&[u8], Error> {
    let mut r = Reader::new(body);
    let list = r.vec24().ok_or(Error::Decode)?;
    if !r.is_empty() {
        return Err(Error::Decode);
    }
    let mut certs = Reader::new(list);
    let mut leaf = None;
    let mut count = 0usize;
    while !certs.is_empty() {
        let cert = certs.vec24().ok_or(Error::Decode)?;
        if cert.is_empty() {
            return Err(Error::Decode);
        }
        count += 1;
        if count > max_chain {
            return Err(Error::Limit(crate::Limit::Chain));
        }
        leaf.get_or_insert(cert);
    }
    leaf.ok_or(Error::NoCertificate)
}

/// The client's ECDH point from a ClientKeyExchange (RFC 8422 section
/// 5.7): one length byte and a 65-byte uncompressed P-256 point.
pub fn client_key_exchange(body: &[u8]) -> Result<&[u8], Error> {
    let mut r = Reader::new(body);
    let point = r.vec8().ok_or(Error::Decode)?;
    if !r.is_empty() || point.is_empty() {
        return Err(Error::Decode);
    }
    if point.len() != 65 || point.first() != Some(&4) {
        return Err(Error::BadKeyShare);
    }
    Ok(point)
}

/// A CertificateVerify's scheme and DER signature (RFC 5246 section
/// 7.4.8). The scheme must be `ecdsa_secp256r1_sha256`, as the
/// CertificateRequest asked.
pub fn certificate_verify(body: &[u8]) -> Result<&[u8], Error> {
    let mut r = Reader::new(body);
    let scheme = r.u16().ok_or(Error::Decode)?;
    let sig = r.vec16().ok_or(Error::Decode)?;
    if !r.is_empty() || sig.is_empty() {
        return Err(Error::Decode);
    }
    if scheme != ECDSA_P256_SHA256 {
        return Err(Error::NoSignatureScheme);
    }
    Ok(sig)
}

/// A Finished message's 12-byte `verify_data`.
pub fn finished(body: &[u8]) -> Result<[u8; 12], Error> {
    body.try_into().map_err(|_| Error::Decode)
}

/// HelloVerifyRequest (RFC 6347 section 4.2.1).
pub(crate) fn hello_verify_request(cookie: &[u8]) -> Result<Vec<u8>, Error> {
    let mut b = Vec::with_capacity(3 + cookie.len());
    put_u16(&mut b, DTLS_1_2);
    put_vec8(&mut b, cookie)?;
    Ok(b)
}

/// ServerHello: DTLS 1.2, our random, an empty session id (no resumption),
/// the suite, null compression, and only extensions the client asked for:
/// `extended_master_secret`, an empty `renegotiation_info` (also answering
/// the SCSV, RFC 5746 section 3.6), `use_srtp` with the chosen profile and
/// no MKI, and `ec_point_formats` if the client sent it.
pub(crate) fn server_hello(random: &[u8; 32], n: &Negotiated) -> Result<Vec<u8>, Error> {
    let mut b = Vec::with_capacity(80);
    put_u16(&mut b, DTLS_1_2);
    b.extend_from_slice(random);
    put_vec8(&mut b, &[])?;
    put_u16(&mut b, SUITE);
    b.push(0);
    let mut e = Vec::with_capacity(32);
    put_u16(&mut e, ext::EXTENDED_MASTER_SECRET);
    put_vec16(&mut e, &[])?;
    put_u16(&mut e, ext::RENEGOTIATION_INFO);
    put_vec16(&mut e, &[0])?;
    put_u16(&mut e, ext::USE_SRTP);
    let mut srtp = Vec::with_capacity(5);
    put_vec16(&mut srtp, &n.profile.id().to_be_bytes())?;
    put_vec8(&mut srtp, &[])?;
    put_vec16(&mut e, &srtp)?;
    if n.point_formats {
        put_u16(&mut e, ext::EC_POINT_FORMATS);
        put_vec16(&mut e, &[1, 0])?;
    }
    put_vec16(&mut b, &e)?;
    Ok(b)
}

/// Certificate with our one certificate.
pub(crate) fn certificate(der: &[u8]) -> Result<Vec<u8>, Error> {
    let mut list = Vec::with_capacity(der.len() + 3);
    put_vec24(&mut list, der)?;
    let mut b = Vec::with_capacity(list.len() + 3);
    put_vec24(&mut b, &list)?;
    Ok(b)
}

/// The `ServerECDHParams` for P-256 and our uncompressed point.
pub(crate) fn ecdh_params(point: &[u8]) -> Result<Vec<u8>, Error> {
    let mut b = Vec::with_capacity(4 + point.len());
    b.push(NAMED_CURVE);
    put_u16(&mut b, SECP256R1);
    put_vec8(&mut b, point)?;
    Ok(b)
}

/// ServerKeyExchange: the params and an `ecdsa_secp256r1_sha256` signature.
pub(crate) fn server_key_exchange(params: &[u8], der_sig: &[u8]) -> Result<Vec<u8>, Error> {
    let mut b = Vec::with_capacity(params.len() + 4 + der_sig.len());
    b.extend_from_slice(params);
    put_u16(&mut b, ECDSA_P256_SHA256);
    put_vec16(&mut b, der_sig)?;
    Ok(b)
}

/// CertificateRequest (RFC 5246 section 7.4.4 with errata 2864/2865):
/// `ecdsa_sign`, `ecdsa_secp256r1_sha256`, no certificate authorities.
pub(crate) fn certificate_request() -> Result<Vec<u8>, Error> {
    let mut b = Vec::with_capacity(8);
    put_vec8(&mut b, &[ECDSA_SIGN])?;
    put_vec16(&mut b, &ECDSA_P256_SHA256.to_be_bytes())?;
    put_vec16(&mut b, &[])?;
    Ok(b)
}

/// Whether a record version is one a first flight may carry.
pub(crate) fn first_flight_version(v: u16) -> bool {
    v == DTLS_1_2 || v == DTLS_1_0
}

#[cfg(test)]
mod tests;
