//! PROXY protocol version 2 headers, parsed without I/O.
//!
//! A layer-4 front proxy can prefix each TCP connection it forwards with a
//! binary PROXY v2 header carrying the client's real address (HAProxy's
//! "proxy-protocol.txt", section 2.2). [`parse`] takes the bytes received so
//! far and answers [`Parsed::NeedMore`], a complete [`Header`] with the
//! number of bytes it occupies, or a typed [`Error`]. A v2 header carries its
//! own length, so the parser never needs a byte past the header, and the
//! caller hands everything after `consumed` to the next protocol.
//!
//! # Trust
//!
//! The specification forbids guessing whether a header is present: a
//! listener is configured to expect one, and must accept it only from known
//! proxies. Call [`parse_trusted`] (or check [`is_trusted_proxy`] yourself)
//! before reading anything from the connection. A peer that is not the
//! trusted proxy is refused with [`Error::UntrustedPeer`]; drop the
//! connection rather than treating its bytes as media. Addresses compare in
//! canonical form, so an IPv4-mapped IPv6 peer (`::ffff:192.0.2.1`) matches
//! an IPv4 proxy (`192.0.2.1`). Ports are not compared.
//!
//! # What is accepted
//!
//! - **PROXY over TCP/IPv4 (`\x11`) and TCP/IPv6 (`\x21`)**: the source and
//!   destination are returned; extra bytes after the addresses are walked as
//!   TLVs.
//! - **LOCAL**, whatever its family and protocol, UNSPEC included: the
//!   proxy's own connection, such as a health check. The whole address block
//!   is skipped, its family byte ignored and its TLVs not read, and
//!   [`Header::client_address`] returns the peer.
//!
//! Section 2.2 pulls two ways on a CRC32C TLV under LOCAL: a receiver that
//! supports the checksum must verify one that is present, but under LOCAL it
//! must also discard the address block, family included, so it cannot know
//! where any TLVs start. The block is discarded and the CRC32C left
//! unchecked. That costs little: a LOCAL header resolves to the TCP peer,
//! which has already been checked to be the trusted proxy.
//!
//! Everything else is refused:
//!
//! - **PROXY with UNSPEC, UDP or UNIX** ([`Error::Unsupported`]). This
//!   takes two steps in section 2.2. A receiver need not implement a valid
//!   family and protocol other than UNSPEC, provided it falls back to UNSPEC
//!   for them; and under PROXY, a receiver "is free to accept the connection
//!   anyway and use the real endpoints addresses or to reject the
//!   connection". So UDP and UNIX fall back to UNSPEC, and UNSPEC under PROXY
//!   is rejected. The specification's section 9 sample code instead keeps the
//!   local address; this parser deliberately does not. Falling back to the
//!   peer would attribute every such client to the proxy itself, collapsing
//!   per-publisher limits and logs onto one address.
//! - **Family or protocol nibbles the specification does not define**
//!   ([`Error::InvalidFamily`]), commands other than LOCAL and PROXY
//!   ([`Error::Command`]) and versions other than 2 ([`Error::Version`]): the
//!   specification says to reject them.
//! - **Version 1 text headers** ([`Error::Version1`]) and anything else that
//!   does not start with the 12-byte signature ([`Error::Signature`]).
//!
//! # Bounds
//!
//! - A whole header is at most [`MAX_HEADER_LEN`] (536) bytes, refused as
//!   soon as its length field arrives, without waiting for its body. This is
//!   a policy cap taken from the specification's design note that both
//!   formats fit the smallest TCP segment any host must accept (576 - 40 =
//!   536 bytes); it is not a MUST on senders. A TLS-terminating sender that
//!   adds UNIQUE_ID (131 bytes with its TLV header), a 255-byte AUTHORITY
//!   (258) and the full SSL sub-TLVs could exceed it, at around 600 bytes or
//!   more after a 52-byte IPv6 fixed part and address block. A layer-4
//!   forward of ICE-TCP, which terminates no TLS, sends none of those.
//! - At most [`MAX_TLVS`] TLVs, each wholly inside the header. A TLV that
//!   overruns it, or one or two stray trailing bytes, is an error.
//! - `CRC32C`, `ALPN`, `AUTHORITY` and `UNIQUE_ID` may each appear once.
//!   `CRC32C` must be four bytes and `UNIQUE_ID` at most 128.
//! - A [`Header`] and its TLVs borrow the input rather than copying it.
//!
//! # CRC32c
//!
//! When a `PP2_TYPE_CRC32C` TLV is present, the CRC-32C (Castagnoli, RFC 4960
//! Appendix B) of the whole header with that TLV's value zeroed must match it.
//! The specification calls the value "a 32-bit number" without saying in
//! which byte order it is stored. Network order is the document's convention
//! for every other field, while RFC 4960's Appendix C sample code puts the
//! low byte first on the wire. Either placement is accepted, and will be
//! until a capture from the real front proxy settles which one it sends. The
//! check guards against corruption, not forgery, so the one-bit loss does
//! not matter; trust comes from the peer address.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// The 12 bytes every version 2 header starts with.
pub const SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

/// The fixed part of a header: signature, version and command, family and
/// protocol, and the 16-bit length of what follows.
pub const FIXED_LEN: usize = 16;

/// The longest whole header accepted, in bytes: the 536-byte segment the
/// specification designs every header to fit.
pub const MAX_HEADER_LEN: usize = 536;

/// The most TLVs a header may carry, padding included.
pub const MAX_TLVS: usize = 32;

/// The longest `PP2_TYPE_UNIQUE_ID` value the specification allows.
pub const MAX_UNIQUE_ID_LEN: usize = 128;

/// `PP2_TYPE_ALPN`: the negotiated application protocol.
pub const PP2_TYPE_ALPN: u8 = 0x01;
/// `PP2_TYPE_AUTHORITY`: the host name the client asked for, such as TLS SNI.
pub const PP2_TYPE_AUTHORITY: u8 = 0x02;
/// `PP2_TYPE_CRC32C`: a checksum of the whole header.
pub const PP2_TYPE_CRC32C: u8 = 0x03;
/// `PP2_TYPE_NOOP`: padding, ignored.
pub const PP2_TYPE_NOOP: u8 = 0x04;
/// `PP2_TYPE_UNIQUE_ID`: an opaque connection identifier from the proxy.
pub const PP2_TYPE_UNIQUE_ID: u8 = 0x05;
/// `PP2_TYPE_SSL`: TLS details, with sub-TLVs. Exposed raw, not parsed.
pub const PP2_TYPE_SSL: u8 = 0x20;
/// `PP2_TYPE_NETNS`: the proxy's network namespace name.
pub const PP2_TYPE_NETNS: u8 = 0x30;

/// What the header says about the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// The proxy's own connection, such as a health check: use the
    /// connection's real endpoints.
    Local,
    /// A relayed TCP connection from `source` to `destination`, as the proxy
    /// saw it. The addresses are as sent, not canonicalised.
    Proxy {
        /// The client's address and port.
        source: SocketAddr,
        /// The address and port the client connected to.
        destination: SocketAddr,
    },
}

/// A complete, validated header. It borrows the input it was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header<'a> {
    command: Command,
    tlvs: &'a [u8],
    crc32c: bool,
}

impl<'a> Header<'a> {
    /// LOCAL or PROXY, with the addresses for PROXY.
    pub fn command(&self) -> Command {
        self.command
    }

    /// The address to treat as the client's: the header's source for PROXY,
    /// or `peer` (the TCP peer, which is the proxy) for LOCAL. An
    /// IPv4-mapped IPv6 address is returned as IPv4, so that limits and logs
    /// keyed on the address see one form; any other address is returned as
    /// given, an IPv6 scope id included.
    pub fn client_address(&self, peer: SocketAddr) -> SocketAddr {
        let address = match self.command {
            Command::Local => peer,
            Command::Proxy { source, .. } => source,
        };
        match address {
            SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
                Some(v4) => SocketAddr::new(v4.into(), v6.port()),
                None => address,
            },
            SocketAddr::V4(_) => address,
        }
    }

    /// The TLVs after the addresses, in order. Empty for LOCAL, whose
    /// address block is skipped whole.
    pub fn tlvs(&self) -> Tlvs<'a> {
        Tlvs { rest: self.tlvs }
    }

    /// The first TLV of `kind`, if any.
    pub fn tlv(&self, kind: u8) -> Option<&'a [u8]> {
        self.tlvs()
            .find(|tlv| tlv.kind == kind)
            .map(|tlv| tlv.value)
    }

    /// The `PP2_TYPE_ALPN` value, such as `h2`.
    pub fn alpn(&self) -> Option<&'a [u8]> {
        self.tlv(PP2_TYPE_ALPN)
    }

    /// The `PP2_TYPE_AUTHORITY` value. The specification says UTF-8; the
    /// bytes are returned unchecked.
    pub fn authority(&self) -> Option<&'a [u8]> {
        self.tlv(PP2_TYPE_AUTHORITY)
    }

    /// The `PP2_TYPE_UNIQUE_ID` value, at most 128 bytes.
    pub fn unique_id(&self) -> Option<&'a [u8]> {
        self.tlv(PP2_TYPE_UNIQUE_ID)
    }

    /// Whether the header carried a `PP2_TYPE_CRC32C` TLV. A header whose
    /// checksum did not match is never returned.
    pub fn has_crc32c(&self) -> bool {
        self.crc32c
    }
}

/// One TLV from a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tlv<'a> {
    /// The type byte, such as [`PP2_TYPE_ALPN`].
    pub kind: u8,
    /// The value, without the type and length bytes.
    pub value: &'a [u8],
}

/// The TLVs of a [`Header`], from [`Header::tlvs`].
#[derive(Debug, Clone)]
pub struct Tlvs<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Tlvs<'a> {
    type Item = Tlv<'a>;

    fn next(&mut self) -> Option<Tlv<'a>> {
        // The region was validated by `parse`; stop quietly on anything else.
        let (tlv, rest) = split_tlv(self.rest).ok()??;
        self.rest = rest;
        Some(tlv)
    }
}

/// The parser's answer when the input is not refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Parsed<'a> {
    /// The input is a valid start of a header, but incomplete, or one to four
    /// bytes of `PROXY` not yet enough to label a version 1 header. At least
    /// `additional` more bytes are needed before a header can be returned;
    /// call [`parse`] again with everything received so far.
    NeedMore {
        /// A lower bound on the further bytes needed, at least one.
        additional: usize,
    },
    /// A complete header occupying the first `consumed` bytes of the input.
    /// What follows belongs to the next protocol.
    Complete {
        /// The header.
        header: Header<'a>,
        /// The header's length: 16 plus its declared length.
        consumed: usize,
    },
}

/// Why a header was refused. Each one means: drop the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The TCP peer is not the trusted proxy, so its bytes are not read.
    UntrustedPeer,
    /// The input starts with `PROXY`: a version 1 text header, which is not
    /// accepted.
    Version1,
    /// The input does not start with the version 2 signature.
    Signature,
    /// The version nibble is not 2.
    Version(u8),
    /// The command nibble is neither LOCAL (0) nor PROXY (1).
    Command(u8),
    /// A family or protocol nibble the specification does not define.
    InvalidFamily(u8),
    /// A defined family and protocol other than TCP over IPv4 or IPv6:
    /// UNSPEC, UDP or UNIX.
    Unsupported(u8),
    /// The declared length makes the header longer than [`MAX_HEADER_LEN`].
    TooLong(u16),
    /// The declared length is shorter than the address block for the
    /// family and protocol (12 bytes for IPv4, 36 for IPv6).
    AddressTooShort(u16),
    /// A TLV's header or value runs past the end of the header.
    TlvOverrun,
    /// More than [`MAX_TLVS`] TLVs.
    TooManyTlvs,
    /// A TLV that may appear only once appeared again.
    DuplicateTlv(u8),
    /// A TLV with a length its type does not allow: `CRC32C` must be 4
    /// bytes and `UNIQUE_ID` at most 128.
    TlvLength(u8),
    /// The `CRC32C` value does not match the header.
    Crc32cMismatch,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UntrustedPeer => {
                f.write_str("PROXY header from a peer that is not the trusted proxy")
            }
            Self::Version1 => {
                f.write_str("PROXY protocol version 1 header; only version 2 is accepted")
            }
            Self::Signature => f.write_str("no PROXY protocol version 2 signature"),
            Self::Version(v) => write!(f, "PROXY protocol version {v}, not 2"),
            Self::Command(c) => write!(f, "unknown PROXY command {c:#x}"),
            Self::InvalidFamily(b) => write!(f, "undefined PROXY family and protocol {b:#04x}"),
            Self::Unsupported(b) => write!(
                f,
                "unsupported PROXY family and protocol {b:#04x}; only TCP over IPv4 or IPv6"
            ),
            Self::TooLong(n) => write!(
                f,
                "PROXY header length {n} exceeds {MAX_HEADER_LEN} bytes in all"
            ),
            Self::AddressTooShort(n) => write!(
                f,
                "PROXY header length {n} is shorter than its address block"
            ),
            Self::TlvOverrun => f.write_str("PROXY TLV runs past the end of the header"),
            Self::TooManyTlvs => write!(f, "more than {MAX_TLVS} PROXY TLVs"),
            Self::DuplicateTlv(t) => write!(f, "duplicate PROXY TLV {t:#04x}"),
            Self::TlvLength(t) => {
                write!(f, "PROXY TLV {t:#04x} has a length its type does not allow")
            }
            Self::Crc32cMismatch => f.write_str("PROXY header CRC32c does not match"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether `peer`, the TCP peer of a connection, is `trusted`, the proxy
/// whose headers are believed. Both are compared in canonical form, so an
/// IPv4-mapped IPv6 address matches its IPv4 address. Ports are ignored.
pub fn is_trusted_proxy(peer: IpAddr, trusted: IpAddr) -> bool {
    peer.to_canonical() == trusted.to_canonical()
}

/// [`parse`], but only for a connection whose TCP peer is `trusted`. Any
/// other peer is refused with [`Error::UntrustedPeer`] before a byte is read,
/// whatever `input` holds, and its connection should be dropped.
pub fn parse_trusted(peer: IpAddr, trusted: IpAddr, input: &[u8]) -> Result<Parsed<'_>, Error> {
    if !is_trusted_proxy(peer, trusted) {
        return Err(Error::UntrustedPeer);
    }
    parse(input)
}

const V1_PREFIX: &[u8; 5] = b"PROXY";

/// Parses a version 2 header from the start of `input`, which holds the
/// bytes received so far on a connection.
///
/// The signature, version, command, family and length are each checked as
/// soon as their bytes arrive, so those refusals never wait for the rest of
/// the header. The one wait on input that cannot become a v2 header is up to
/// five bytes of `PROXY`, to tell [`Error::Version1`] from
/// [`Error::Signature`]. The addresses and TLVs are checked once the whole header, at
/// most [`MAX_HEADER_LEN`] bytes, is in. For any input, a prefix of it
/// parses either as [`Parsed::NeedMore`] or exactly as the whole does.
pub fn parse(input: &[u8]) -> Result<Parsed<'_>, Error> {
    let have = input.len().min(SIGNATURE.len());
    if input.get(..have) != SIGNATURE.get(..have) {
        // Tell a version 1 header apart, which takes at most five bytes.
        let seen = input.len().min(V1_PREFIX.len());
        if input.get(..seen) != V1_PREFIX.get(..seen) {
            return Err(Error::Signature);
        }
        if seen < V1_PREFIX.len() {
            return Ok(need(V1_PREFIX.len(), input));
        }
        return Err(Error::Version1);
    }
    let Some(&version_command) = input.get(12) else {
        return Ok(need(FIXED_LEN, input));
    };
    if version_command >> 4 != 2 {
        return Err(Error::Version(version_command >> 4));
    }
    let proxy = match version_command & 0x0f {
        0 => false,
        1 => true,
        other => return Err(Error::Command(other)),
    };
    let Some(&family_protocol) = input.get(13) else {
        return Ok(need(FIXED_LEN, input));
    };
    // LOCAL ignores the family, so only PROXY checks it.
    let address_len = if proxy {
        address_len(family_protocol)?
    } else {
        0
    };
    let Some(&[high, low]) = input.get(14..16) else {
        return Ok(need(FIXED_LEN, input));
    };
    let declared = u16::from_be_bytes([high, low]);
    let total = FIXED_LEN + usize::from(declared);
    if total > MAX_HEADER_LEN {
        return Err(Error::TooLong(declared));
    }
    if usize::from(declared) < address_len {
        return Err(Error::AddressTooShort(declared));
    }
    let Some(header) = input.get(..total) else {
        return Ok(need(total, input));
    };
    if !proxy {
        let header = Header {
            command: Command::Local,
            tlvs: &[],
            crc32c: false,
        };
        return Ok(Parsed::Complete {
            header,
            consumed: total,
        });
    }
    let (addresses, tlvs) = header
        .get(FIXED_LEN..)
        .and_then(|body| body.split_at_checked(address_len))
        .ok_or(Error::AddressTooShort(declared))?;
    let command =
        addresses_of(family_protocol, addresses).ok_or(Error::AddressTooShort(declared))?;
    let crc_at = walk(tlvs)?.map(|at| FIXED_LEN + address_len + at);
    if let Some(at) = crc_at {
        check_crc32c(header, at)?;
    }
    Ok(Parsed::Complete {
        header: Header {
            command,
            tlvs,
            crc32c: crc_at.is_some(),
        },
        consumed: total,
    })
}

fn need(total: usize, input: &[u8]) -> Parsed<'static> {
    Parsed::NeedMore {
        additional: total.saturating_sub(input.len()).max(1),
    }
}

/// The address block length for a PROXY header's family and protocol, or
/// why it is refused.
fn address_len(family_protocol: u8) -> Result<usize, Error> {
    let (family, protocol) = (family_protocol >> 4, family_protocol & 0x0f);
    if family > 3 || protocol > 2 {
        return Err(Error::InvalidFamily(family_protocol));
    }
    match family_protocol {
        0x11 => Ok(12),
        0x21 => Ok(36),
        _ => Err(Error::Unsupported(family_protocol)),
    }
}

fn addresses_of(family_protocol: u8, block: &[u8]) -> Option<Command> {
    let (source, destination, rest): (IpAddr, IpAddr, _) = if family_protocol == 0x11 {
        let (source, rest) = block.split_first_chunk::<4>()?;
        let (destination, rest) = rest.split_first_chunk::<4>()?;
        (
            Ipv4Addr::from(*source).into(),
            Ipv4Addr::from(*destination).into(),
            rest,
        )
    } else {
        let (source, rest) = block.split_first_chunk::<16>()?;
        let (destination, rest) = rest.split_first_chunk::<16>()?;
        (
            Ipv6Addr::from(*source).into(),
            Ipv6Addr::from(*destination).into(),
            rest,
        )
    };
    let (&[s0, s1, d0, d1], _) = rest.split_first_chunk::<4>()?;
    Some(Command::Proxy {
        source: SocketAddr::new(source, u16::from_be_bytes([s0, s1])),
        destination: SocketAddr::new(destination, u16::from_be_bytes([d0, d1])),
    })
}

/// Splits the first TLV off `region`: `Ok(None)` when it is empty.
fn split_tlv(region: &[u8]) -> Result<Option<(Tlv<'_>, &[u8])>, Error> {
    if region.is_empty() {
        return Ok(None);
    }
    let (&[kind, high, low], after) = region.split_first_chunk::<3>().ok_or(Error::TlvOverrun)?;
    let (value, rest) = after
        .split_at_checked(usize::from(u16::from_be_bytes([high, low])))
        .ok_or(Error::TlvOverrun)?;
    Ok(Some((Tlv { kind, value }, rest)))
}

/// Checks every TLV in `region` and returns the offset in it of the CRC32c
/// value, if there is one.
fn walk(region: &[u8]) -> Result<Option<usize>, Error> {
    let mut rest = region;
    let mut count = 0;
    // One bit per single-instance type; those types are all below 8.
    let mut seen = 0u8;
    let mut crc_at = None;
    while let Some((tlv, next)) = split_tlv(rest)? {
        count += 1;
        if count > MAX_TLVS {
            return Err(Error::TooManyTlvs);
        }
        if matches!(
            tlv.kind,
            PP2_TYPE_ALPN | PP2_TYPE_AUTHORITY | PP2_TYPE_CRC32C | PP2_TYPE_UNIQUE_ID
        ) {
            let bit = 1u8 << tlv.kind;
            if seen & bit != 0 {
                return Err(Error::DuplicateTlv(tlv.kind));
            }
            seen |= bit;
        }
        match tlv.kind {
            PP2_TYPE_CRC32C if tlv.value.len() != 4 => return Err(Error::TlvLength(tlv.kind)),
            PP2_TYPE_CRC32C => crc_at = Some(region.len() - next.len() - 4),
            PP2_TYPE_UNIQUE_ID if tlv.value.len() > MAX_UNIQUE_ID_LEN => {
                return Err(Error::TlvLength(tlv.kind));
            }
            _ => {}
        }
        rest = next;
    }
    Ok(crc_at)
}

/// Checks the CRC32c whose four-byte value starts at `at` in `header`.
fn check_crc32c(header: &[u8], at: usize) -> Result<(), Error> {
    let (before, rest) = header.split_at_checked(at).ok_or(Error::TlvOverrun)?;
    let (&received, after) = rest.split_first_chunk::<4>().ok_or(Error::TlvOverrun)?;
    let computed = Crc32c::new()
        .update(before)
        .update(&[0; 4])
        .update(after)
        .finish();
    if computed == u32::from_be_bytes(received) || computed == u32::from_le_bytes(received) {
        Ok(())
    } else {
        Err(Error::Crc32cMismatch)
    }
}

/// CRC-32C (Castagnoli), reflected, as RFC 4960 Appendix B computes it.
struct Crc32c(u32);

impl Crc32c {
    fn new() -> Self {
        Self(u32::MAX)
    }

    fn update(mut self, data: &[u8]) -> Self {
        for &byte in data {
            let i = usize::from((self.0 as u8) ^ byte);
            self.0 = (self.0 >> 8) ^ CRC32C_TABLE.get(i).copied().unwrap_or(0);
        }
        self
    }

    fn finish(self) -> u32 {
        !self.0
    }
}

/// The reflected Castagnoli polynomial, 0x1EDC6F41 bit-reversed.
const CRC32C_POLY: u32 = 0x82f6_3b78;

const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ CRC32C_POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

#[cfg(test)]
#[path = "proxy_protocol_tests.rs"]
mod tests;
