//! Why a handshake or association ended, and the alert each reason sends.

use std::fmt;

/// TLS alert descriptions sapflow sends (RFC 5246 section 7.2, RFC 5746
/// section 4.5).
pub mod alert {
    /// `close_notify`.
    pub const CLOSE_NOTIFY: u8 = 0;
    /// `unexpected_message`.
    pub const UNEXPECTED_MESSAGE: u8 = 10;
    /// `handshake_failure`.
    pub const HANDSHAKE_FAILURE: u8 = 40;
    /// `bad_certificate`.
    pub const BAD_CERTIFICATE: u8 = 42;
    /// `unsupported_certificate`.
    pub const UNSUPPORTED_CERTIFICATE: u8 = 43;
    /// `illegal_parameter`.
    pub const ILLEGAL_PARAMETER: u8 = 47;
    /// `decode_error`.
    pub const DECODE_ERROR: u8 = 50;
    /// `decrypt_error`.
    pub const DECRYPT_ERROR: u8 = 51;
    /// `protocol_version`.
    pub const PROTOCOL_VERSION: u8 = 70;
    /// `internal_error`.
    pub const INTERNAL_ERROR: u8 = 80;
    /// `no_renegotiation`, always a warning.
    pub const NO_RENEGOTIATION: u8 = 100;
    /// Alert level `warning`.
    pub const WARNING: u8 = 1;
    /// Alert level `fatal`.
    pub const FATAL: u8 = 2;
}

/// A bound that the peer reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// A handshake message declared a length above
    /// [`crate::Config::max_message_len`].
    MessageSize,
    /// One handshake message arrived in more than
    /// [`crate::Config::max_fragments`] fragments.
    Fragments,
    /// Handshake messages being reassembled would hold more than
    /// [`crate::Config::max_buffered`] bytes.
    Buffered,
    /// More certificates in the peer's chain than [`crate::MAX_CHAIN`].
    Chain,
    /// The peer kept asking for a HelloVerifyRequest.
    HelloVerify,
    /// A record sequence number would wrap.
    Sequence,
}

/// Why the handshake failed or the association ended. Every variant except
/// [`Error::Timeout`], [`Error::PeerAlert`] and [`Error::PeerClosed`] sends
/// one fatal alert ([`Error::alert`]) before the server goes silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The ClientHello does not offer DTLS 1.2: its version is below 1.2,
    /// or it is not a DTLS version.
    Version,
    /// `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` is not offered.
    NoCipherSuite,
    /// `secp256r1` is not in `supported_groups`, or the extension is absent.
    NoGroup,
    /// `ecdsa_secp256r1_sha256` is not in `signature_algorithms`, or the
    /// extension is absent, or a CertificateVerify used another scheme.
    NoSignatureScheme,
    /// No `extended_master_secret` extension (RFC 7627).
    NoExtendedMasterSecret,
    /// Neither `renegotiation_info` nor the SCSV (RFC 5746).
    NoRenegotiationSignal,
    /// A `renegotiation_info` extension that is not empty in an initial
    /// handshake (RFC 5746 section 3.6).
    RenegotiationInfo,
    /// No `use_srtp`, or neither `SRTP_AEAD_AES_128_GCM` nor
    /// `SRTP_AES128_CM_HMAC_SHA1_80` offered.
    NoSrtpProfile,
    /// A `use_srtp` extension with a non-empty `srtp_mki`.
    SrtpMki,
    /// The null compression method is not offered.
    Compression,
    /// `ec_point_formats` without the uncompressed format (RFC 8422
    /// section 5.1.2).
    PointFormat,
    /// The same extension twice.
    DuplicateExtension,
    /// A handshake message that does not parse.
    Decode,
    /// A handshake message, or ChangeCipherSpec, out of order.
    UnexpectedMessage,
    /// An empty Certificate message.
    NoCertificate,
    /// The peer's leaf certificate does not parse, or its key is invalid.
    BadCertificate,
    /// The peer's certificate key is not ECDSA P-256.
    UnsupportedCertificate,
    /// The SHA-256 of the peer's certificate is not the expected
    /// fingerprint from the offer.
    FingerprintMismatch,
    /// The CertificateVerify signature does not verify.
    BadSignature,
    /// The ClientKeyExchange point is not a valid uncompressed P-256 point.
    BadKeyShare,
    /// The client's Finished does not match.
    BadFinished,
    /// The peer reached a bound.
    Limit(Limit),
    /// The handshake made no progress after
    /// [`crate::Config::max_transmissions`] sends of our flight.
    Timeout,
    /// The peer sent a fatal alert (or `close_notify` during the
    /// handshake), with this description.
    PeerAlert(u8),
    /// The peer closed the association with `close_notify`.
    PeerClosed,
    /// The system random source failed.
    Entropy,
    /// A bug: a value sapflow built did not fit its encoding.
    Internal,
}

impl Error {
    /// The fatal alert description this error sends, if any.
    pub const fn alert(self) -> Option<u8> {
        use alert::*;
        Some(match self {
            Self::Version => PROTOCOL_VERSION,
            Self::NoCipherSuite
            | Self::NoGroup
            | Self::NoSignatureScheme
            | Self::NoExtendedMasterSecret
            | Self::NoRenegotiationSignal
            | Self::RenegotiationInfo
            | Self::NoSrtpProfile
            | Self::SrtpMki
            | Self::NoCertificate
            | Self::Limit(_) => HANDSHAKE_FAILURE,
            Self::Compression
            | Self::PointFormat
            | Self::DuplicateExtension
            | Self::BadKeyShare => ILLEGAL_PARAMETER,
            Self::Decode => DECODE_ERROR,
            Self::UnexpectedMessage => UNEXPECTED_MESSAGE,
            Self::BadCertificate | Self::FingerprintMismatch => BAD_CERTIFICATE,
            Self::UnsupportedCertificate => UNSUPPORTED_CERTIFICATE,
            Self::BadSignature | Self::BadFinished => DECRYPT_ERROR,
            Self::Entropy | Self::Internal => INTERNAL_ERROR,
            Self::Timeout | Self::PeerAlert(_) | Self::PeerClosed => return None,
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DTLS: {self:?}")
    }
}

impl std::error::Error for Error {}

/// A [`crate::Config`] value is out of range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError(pub &'static str);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DTLS configuration: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}
