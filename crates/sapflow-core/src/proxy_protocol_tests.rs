use super::*;
use std::net::{SocketAddrV4, SocketAddrV6};

const SIG: [u8; 12] = [
    0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a,
];

/// TCP over IPv4, worked by hand from section 2.2: 192.0.2.1:56324 to
/// 198.51.100.7:443, address length 12.
const TCP4: [u8; 28] = [
    0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a, // signature
    0x21, // version 2, PROXY
    0x11, // AF_INET, STREAM
    0x00, 0x0c, // 12 bytes follow
    192, 0, 2, 1, // source address
    198, 51, 100, 7, // destination address
    0xdc, 0x04, // source port 56324
    0x01, 0xbb, // destination port 443
];

/// TCP over IPv6: [2001:db8::1]:50000 to [2001:db8::2]:443, length 36.
const TCP6: [u8; 52] = [
    0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a, //
    0x21, 0x21, 0x00, 0x24, //
    0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, //
    0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, //
    0xc3, 0x50, 0x01, 0xbb,
];

/// LOCAL with UNSPEC and no address block, as a health check sends it.
const LOCAL: [u8; 16] = [
    0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a, //
    0x20, 0x00, 0x00, 0x00,
];

/// TCP4 with a CRC32C TLV, its value computed independently (a bitwise
/// CRC-32C over the header with the value zeroed) as 0xd45a11b9 and stored
/// in network byte order.
const TCP4_CRC_BE: [u8; 35] = [
    0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a, //
    0x21, 0x11, 0x00, 0x13, //
    192, 0, 2, 1, 198, 51, 100, 7, 0xdc, 0x04, 0x01, 0xbb, //
    0x03, 0x00, 0x04, 0xd4, 0x5a, 0x11, 0xb9,
];

fn v4(a: [u8; 4], port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(a), port))
}

fn v6(text: &str, port: u16) -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(text.parse().expect("IPv6"), port, 0, 0))
}

fn complete(input: &[u8]) -> (Header<'_>, usize) {
    match parse(input) {
        Ok(Parsed::Complete { header, consumed }) => (header, consumed),
        other => panic!("expected a header, got {other:?}"),
    }
}

/// A header with `body` after the fixed part, its length filled in.
fn header(version_command: u8, family_protocol: u8, body: &[u8]) -> Vec<u8> {
    let mut out = SIG.to_vec();
    out.push(version_command);
    out.push(family_protocol);
    out.extend_from_slice(&u16::try_from(body.len()).expect("length").to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn tlv(kind: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&u16::try_from(value.len()).expect("length").to_be_bytes());
    out.extend_from_slice(value);
    out
}

const TCP4_ADDRESSES: [u8; 12] = [192, 0, 2, 1, 198, 51, 100, 7, 0xdc, 0x04, 0x01, 0xbb];

/// TCP4 with `tlvs` after the addresses.
fn tcp4_with(tlvs: &[u8]) -> Vec<u8> {
    let mut body = TCP4_ADDRESSES.to_vec();
    body.extend_from_slice(tlvs);
    header(0x21, 0x11, &body)
}

/// TCP4 with `before`, a CRC32C TLV and `after`, the CRC filled in.
fn tcp4_with_crc(before: &[u8], after: &[u8], little_endian: bool) -> Vec<u8> {
    let mut tlvs = before.to_vec();
    tlvs.extend(tlv(PP2_TYPE_CRC32C, &[0; 4]));
    tlvs.extend_from_slice(after);
    let mut out = tcp4_with(&tlvs);
    let crc = Crc32c::new().update(&out).finish();
    let at = FIXED_LEN + TCP4_ADDRESSES.len() + before.len() + 3;
    let bytes = if little_endian {
        crc.to_le_bytes()
    } else {
        crc.to_be_bytes()
    };
    out.get_mut(at..at + 4)
        .expect("CRC value")
        .copy_from_slice(&bytes);
    out
}

/// Every prefix of `input` parses as `NeedMore` or exactly as the whole
/// does, and a `NeedMore` never asks for more than a header that completes.
fn assert_prefix_consistent(input: &[u8]) {
    let whole = parse(input);
    for split in 0..input.len() {
        let prefix = input.get(..split).expect("prefix");
        match parse(prefix) {
            Ok(Parsed::NeedMore { additional }) => {
                assert!(additional >= 1, "split {split}");
                if let Ok(Parsed::Complete { consumed, .. }) = whole {
                    assert!(
                        split + additional <= consumed,
                        "split {split} asks too much"
                    );
                }
            }
            partial => assert_eq!(partial, whole, "split {split} of {input:02x?}"),
        }
    }
}

#[test]
fn spec_tcp4_example_gives_both_endpoints() {
    let (header, consumed) = complete(&TCP4);
    assert_eq!(consumed, 28);
    assert_eq!(
        header.command(),
        Command::Proxy {
            source: v4([192, 0, 2, 1], 56324),
            destination: v4([198, 51, 100, 7], 443),
        }
    );
    let peer = v4([203, 0, 113, 9], 40000);
    assert_eq!(header.client_address(peer), v4([192, 0, 2, 1], 56324));
    assert_eq!(header.tlvs().count(), 0);
    assert!(!header.has_crc32c());
}

#[test]
fn spec_tcp6_example_gives_both_endpoints() {
    let (header, consumed) = complete(&TCP6);
    assert_eq!(consumed, 52);
    assert_eq!(
        header.command(),
        Command::Proxy {
            source: v6("2001:db8::1", 50000),
            destination: v6("2001:db8::2", 443),
        }
    );
}

#[test]
fn signature_constant_is_the_documented_bytes() {
    assert_eq!(SIGNATURE, SIG);
}

#[test]
fn local_uses_the_peer_and_skips_the_whole_address_block() {
    let peer = v4([203, 0, 113, 9], 40000);
    let (local, consumed) = complete(&LOCAL);
    assert_eq!((local.command(), consumed), (Command::Local, 16));
    assert_eq!(local.client_address(peer), peer);

    // The length field is always honoured, even for LOCAL, and the family
    // (here an undefined one) and anything in the block are ignored.
    let mut body = TCP4_ADDRESSES.to_vec();
    body.extend(tlv(PP2_TYPE_CRC32C, &[0xde, 0xad, 0xbe, 0xef]));
    for family in [0x00, 0x11, 0x21, 0x31, 0xff] {
        let input = header(0x20, family, &body);
        let (local, consumed) = complete(&input);
        assert_eq!(consumed, 16 + 19, "family {family:#x}");
        assert_eq!(local.command(), Command::Local);
        assert_eq!(local.tlvs().count(), 0);
        assert!(!local.has_crc32c());
        assert_eq!(local.client_address(peer), peer);
    }
}

#[test]
fn bytes_after_the_header_are_left_for_the_next_protocol() {
    let mut input = TCP4.to_vec();
    input.extend_from_slice(&[0x00, 0x10, 0x16, 0x03, 0x01]);
    let (header, consumed) = complete(&input);
    assert_eq!(consumed, TCP4.len());
    assert_eq!(header, complete(&TCP4).0);
}

#[test]
fn tlvs_are_exposed_in_order_with_alpn_and_authority() {
    let mut tlvs = tlv(PP2_TYPE_ALPN, b"h2");
    tlvs.extend(tlv(PP2_TYPE_AUTHORITY, b"example.com"));
    tlvs.extend(tlv(PP2_TYPE_NOOP, b""));
    tlvs.extend(tlv(PP2_TYPE_UNIQUE_ID, &[7; 128]));
    tlvs.extend(tlv(PP2_TYPE_SSL, &[0x01, 0, 0, 0, 0]));
    tlvs.extend(tlv(0xe0, b"custom"));
    let input = tcp4_with(&tlvs);
    let (header, consumed) = complete(&input);
    assert_eq!(consumed, input.len());
    assert_eq!(header.alpn(), Some(&b"h2"[..]));
    assert_eq!(header.authority(), Some(&b"example.com"[..]));
    assert_eq!(header.unique_id(), Some(&[7; 128][..]));
    assert_eq!(header.tlv(PP2_TYPE_SSL), Some(&[0x01, 0, 0, 0, 0][..]));
    assert_eq!(header.tlv(PP2_TYPE_NETNS), None);
    let kinds: Vec<u8> = header.tlvs().map(|tlv| tlv.kind).collect();
    assert_eq!(kinds, [0x01, 0x02, 0x04, 0x05, 0x20, 0xe0]);
    assert_eq!(
        header.tlvs().last(),
        Some(Tlv {
            kind: 0xe0,
            value: b"custom"
        })
    );
}

#[test]
fn tlvs_after_an_ipv6_block_are_walked() {
    let mut body = TCP6.get(16..).expect("body").to_vec();
    body.extend(tlv(PP2_TYPE_ALPN, b"stun.turn"));
    let input = header(0x21, 0x21, &body);
    assert_eq!(complete(&input).0.alpn(), Some(&b"stun.turn"[..]));
}

#[test]
fn crc32c_check_value_and_table_match_a_bitwise_reference() {
    assert_eq!(Crc32c::new().update(b"123456789").finish(), 0xe306_9283);
    assert_eq!(Crc32c::new().update(b"").finish(), 0);
    let bitwise = |data: &[u8]| {
        let mut crc = u32::MAX;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0x82f6_3b78
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    };
    let data: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
    for end in [0, 1, 3, 4, 17, 255, 256, 511] {
        let slice = data.get(..end).expect("slice");
        assert_eq!(Crc32c::new().update(slice).finish(), bitwise(slice));
    }
    // Feeding in pieces gives the same value as one update.
    assert_eq!(
        Crc32c::new().update(b"1234").update(b"56789").finish(),
        0xe306_9283
    );
}

#[test]
fn crc32c_independent_fixture_verifies_in_either_byte_order() {
    let (header, _) = complete(&TCP4_CRC_BE);
    assert!(header.has_crc32c());
    let mut little = TCP4_CRC_BE;
    little
        .get_mut(31..35)
        .expect("CRC value")
        .copy_from_slice(&[0xb9, 0x11, 0x5a, 0xd4]);
    assert!(complete(&little).0.has_crc32c());
    // The builder agrees with the independent value.
    assert_eq!(tcp4_with_crc(&[], &[], false), TCP4_CRC_BE.to_vec());
}

#[test]
fn crc32c_covers_the_whole_header_including_other_tlvs() {
    let before = tlv(PP2_TYPE_ALPN, b"h2");
    let after = tlv(PP2_TYPE_AUTHORITY, b"example.com");
    for little_endian in [false, true] {
        let input = tcp4_with_crc(&before, &after, little_endian);
        let (header, _) = complete(&input);
        assert!(header.has_crc32c());
        assert_eq!(header.alpn(), Some(&b"h2"[..]));
        // Flipping any bit of the header, the checksum included, never gives
        // a header: a changed length waits for more bytes or is refused, a
        // changed type is refused for its own reason, and the rest fail the
        // checksum.
        for at in 0..input.len() {
            for bit in [0x01, 0x80] {
                // PROXY becomes LOCAL, whose block is discarded unread, or
                // the CRC32C TLV becomes an unknown type: neither can be
                // caught by a checksum carried in the header itself.
                let crc_type = FIXED_LEN + TCP4_ADDRESSES.len() + before.len();
                if (at, bit) == (12, 0x01) || (at == crc_type && bit == 0x80) {
                    continue;
                }
                let mut flipped = input.clone();
                if let Some(byte) = flipped.get_mut(at) {
                    *byte ^= bit;
                }
                assert!(
                    !matches!(parse(&flipped), Ok(Parsed::Complete { .. })),
                    "byte {at} bit {bit:#x} accepted"
                );
            }
        }
    }
    let mut wrong = TCP4_CRC_BE;
    if let Some(byte) = wrong.get_mut(20) {
        *byte ^= 1;
    }
    assert_eq!(parse(&wrong), Err(Error::Crc32cMismatch));
}

#[test]
fn need_more_counts_are_lower_bounds() {
    assert_eq!(parse(&[]), Ok(Parsed::NeedMore { additional: 16 }));
    assert_eq!(
        parse(TCP4.get(..5).expect("prefix")),
        Ok(Parsed::NeedMore { additional: 11 })
    );
    assert_eq!(
        parse(TCP4.get(..16).expect("prefix")),
        Ok(Parsed::NeedMore { additional: 12 })
    );
    assert_eq!(
        parse(TCP4.get(..27).expect("prefix")),
        Ok(Parsed::NeedMore { additional: 1 })
    );
    assert_eq!(parse(b"PRO"), Ok(Parsed::NeedMore { additional: 2 }));
}

#[test]
fn refusals_are_typed() {
    let cases: Vec<(Vec<u8>, Error)> = vec![
        (b"GET / HTTP/1.1\r\n".to_vec(), Error::Signature),
        (vec![0x16, 0x03, 0x01, 0x00], Error::Signature),
        (vec![0x0d, 0x0a, 0x0d, 0x0b], Error::Signature),
        (
            b"PROXY TCP4 192.0.2.1 198.51.100.7 56324 443\r\n".to_vec(),
            Error::Version1,
        ),
        (b"PROXY".to_vec(), Error::Version1),
        (header(0x11, 0x11, &TCP4_ADDRESSES), Error::Version(1)),
        (header(0x31, 0x11, &TCP4_ADDRESSES), Error::Version(3)),
        (header(0x01, 0x11, &TCP4_ADDRESSES), Error::Version(0)),
        (header(0x22, 0x11, &TCP4_ADDRESSES), Error::Command(2)),
        (header(0x2f, 0x11, &TCP4_ADDRESSES), Error::Command(0xf)),
        (
            header(0x21, 0x41, &TCP4_ADDRESSES),
            Error::InvalidFamily(0x41),
        ),
        (
            header(0x21, 0x13, &TCP4_ADDRESSES),
            Error::InvalidFamily(0x13),
        ),
        (
            header(0x21, 0xff, &TCP4_ADDRESSES),
            Error::InvalidFamily(0xff),
        ),
        (header(0x21, 0x00, &[]), Error::Unsupported(0x00)),
        (
            header(0x21, 0x00, &TCP4_ADDRESSES),
            Error::Unsupported(0x00),
        ),
        (
            header(0x21, 0x01, &TCP4_ADDRESSES),
            Error::Unsupported(0x01),
        ),
        (
            header(0x21, 0x10, &TCP4_ADDRESSES),
            Error::Unsupported(0x10),
        ),
        (header(0x21, 0x20, &[0; 36]), Error::Unsupported(0x20)),
        (
            header(0x21, 0x12, &TCP4_ADDRESSES),
            Error::Unsupported(0x12),
        ),
        (header(0x21, 0x22, &[0; 36]), Error::Unsupported(0x22)),
        (header(0x21, 0x31, &[0; 216]), Error::Unsupported(0x31)),
        (header(0x21, 0x32, &[0; 216]), Error::Unsupported(0x32)),
        (header(0x21, 0x30, &[0; 216]), Error::Unsupported(0x30)),
        (header(0x21, 0x11, &[0; 11]), Error::AddressTooShort(11)),
        (header(0x21, 0x11, &[]), Error::AddressTooShort(0)),
        (header(0x21, 0x21, &[0; 35]), Error::AddressTooShort(35)),
        (header(0x21, 0x21, &[0; 12]), Error::AddressTooShort(12)),
        (header(0x21, 0x11, &[0; 521]), Error::TooLong(521)),
        (header(0x20, 0x00, &[0; 521]), Error::TooLong(521)),
        (tcp4_with(&[0x04]), Error::TlvOverrun),
        (tcp4_with(&[0x04, 0x00]), Error::TlvOverrun),
        (
            tcp4_with(&[0x01, 0x00, 0x03, b'h', b'2']),
            Error::TlvOverrun,
        ),
        (tcp4_with(&[0x01, 0xff, 0xff]), Error::TlvOverrun),
        (
            tcp4_with(&[tlv(PP2_TYPE_NOOP, b""), vec![0x02, 0x00]].concat()),
            Error::TlvOverrun,
        ),
        (
            tcp4_with(&tlv(PP2_TYPE_NOOP, b"").repeat(33)),
            Error::TooManyTlvs,
        ),
        (
            tcp4_with(&[tlv(PP2_TYPE_ALPN, b"h2"), tlv(PP2_TYPE_ALPN, b"h2")].concat()),
            Error::DuplicateTlv(PP2_TYPE_ALPN),
        ),
        (
            tcp4_with(&[tlv(PP2_TYPE_AUTHORITY, b"a"), tlv(PP2_TYPE_AUTHORITY, b"b")].concat()),
            Error::DuplicateTlv(PP2_TYPE_AUTHORITY),
        ),
        (
            tcp4_with(&[tlv(PP2_TYPE_UNIQUE_ID, b"a"), tlv(PP2_TYPE_UNIQUE_ID, b"a")].concat()),
            Error::DuplicateTlv(PP2_TYPE_UNIQUE_ID),
        ),
        (
            tcp4_with(&[tlv(PP2_TYPE_CRC32C, &[0; 4]), tlv(PP2_TYPE_CRC32C, &[0; 4])].concat()),
            Error::DuplicateTlv(PP2_TYPE_CRC32C),
        ),
        (
            tcp4_with(&tlv(PP2_TYPE_CRC32C, &[0; 3])),
            Error::TlvLength(PP2_TYPE_CRC32C),
        ),
        (
            tcp4_with(&tlv(PP2_TYPE_CRC32C, &[0; 5])),
            Error::TlvLength(PP2_TYPE_CRC32C),
        ),
        (
            tcp4_with(&tlv(PP2_TYPE_UNIQUE_ID, &[0; 129])),
            Error::TlvLength(PP2_TYPE_UNIQUE_ID),
        ),
        (
            tcp4_with(&tlv(PP2_TYPE_CRC32C, &[0; 4])),
            Error::Crc32cMismatch,
        ),
    ];
    for (input, error) in &cases {
        assert_eq!(parse(input), Err(*error), "{input:02x?}");
        assert_prefix_consistent(input);
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn refusals_come_as_soon_as_their_bytes_arrive() {
    // One wrong byte is enough to refuse a signature.
    assert_eq!(parse(&[0x47]), Err(Error::Signature));
    assert_eq!(parse(&[0x0d, 0x0a, 0x0e]), Err(Error::Signature));
    // Version and command at the 13th byte, family at the 14th.
    let mut prefix = SIG.to_vec();
    prefix.push(0x11);
    assert_eq!(parse(&prefix), Err(Error::Version(1)));
    prefix.pop();
    prefix.push(0x23);
    assert_eq!(parse(&prefix), Err(Error::Command(3)));
    prefix.pop();
    prefix.extend_from_slice(&[0x21, 0x12]);
    assert_eq!(parse(&prefix), Err(Error::Unsupported(0x12)));
    // An overlong or too-short length with just the fixed part.
    let fixed = |length: u16| {
        let mut out = SIG.to_vec();
        out.extend_from_slice(&[0x21, 0x11]);
        out.extend_from_slice(&length.to_be_bytes());
        out
    };
    assert_eq!(parse(&fixed(u16::MAX)), Err(Error::TooLong(u16::MAX)));
    assert_eq!(parse(&fixed(521)), Err(Error::TooLong(521)));
    assert_eq!(parse(&fixed(11)), Err(Error::AddressTooShort(11)));
    assert_eq!(parse(&fixed(520)), Ok(Parsed::NeedMore { additional: 520 }));
}

#[test]
fn the_header_cap_is_536_bytes_in_all() {
    // 12 address bytes, then NOOP padding up to exactly 536.
    let padding = MAX_HEADER_LEN - FIXED_LEN - TCP4_ADDRESSES.len() - 3;
    let input = tcp4_with(&tlv(PP2_TYPE_NOOP, &vec![0; padding]));
    assert_eq!(input.len(), MAX_HEADER_LEN);
    assert_eq!(complete(&input).1, MAX_HEADER_LEN);
    let input = tcp4_with(&tlv(PP2_TYPE_NOOP, &vec![0; padding + 1]));
    assert_eq!(parse(&input), Err(Error::TooLong(521)));
    // The largest address block, UNIX, still leaves room for TLVs.
    const { assert!(MAX_HEADER_LEN - FIXED_LEN - 216 >= 300) };
}

#[test]
fn up_to_max_tlvs_are_accepted() {
    let input = tcp4_with(&tlv(PP2_TYPE_NOOP, b"").repeat(MAX_TLVS));
    assert_eq!(complete(&input).0.tlvs().count(), MAX_TLVS);
}

#[test]
fn every_split_point_of_every_accepted_header_is_consistent() {
    let mut accepted: Vec<Vec<u8>> = vec![
        TCP4.to_vec(),
        TCP6.to_vec(),
        LOCAL.to_vec(),
        TCP4_CRC_BE.to_vec(),
        tcp4_with_crc(
            &tlv(PP2_TYPE_ALPN, b"h2"),
            &tlv(PP2_TYPE_NOOP, b"pad"),
            true,
        ),
        tcp4_with(
            &[
                tlv(PP2_TYPE_ALPN, b"h2"),
                tlv(PP2_TYPE_AUTHORITY, b"example.com"),
            ]
            .concat(),
        ),
        header(0x20, 0x11, &TCP4_ADDRESSES),
    ];
    // With media bytes after the header too.
    accepted.push([TCP4.as_slice(), b"\x16\x03\x01"].concat());
    for input in &accepted {
        assert!(matches!(parse(input), Ok(Parsed::Complete { .. })));
        assert_prefix_consistent(input);
    }
}

#[test]
fn trust_compares_canonical_addresses_and_ignores_ports() {
    let proxy: IpAddr = Ipv4Addr::new(192, 0, 2, 10).into();
    let mapped: IpAddr = "::ffff:192.0.2.10".parse().expect("mapped");
    let other: IpAddr = Ipv4Addr::new(192, 0, 2, 11).into();
    let v6_proxy: IpAddr = "2001:db8::10".parse().expect("IPv6");
    assert!(is_trusted_proxy(proxy, proxy));
    assert!(is_trusted_proxy(mapped, proxy));
    assert!(is_trusted_proxy(proxy, mapped));
    assert!(!is_trusted_proxy(other, proxy));
    assert!(!is_trusted_proxy(
        "::ffff:192.0.2.11".parse().expect("mapped"),
        proxy
    ));
    assert!(is_trusted_proxy(v6_proxy, v6_proxy));
    assert!(!is_trusted_proxy(v6_proxy, proxy));
    // The IPv4-compatible form is not a mapped address and does not match.
    assert!(!is_trusted_proxy(
        "::192.0.2.10".parse().expect("compatible"),
        proxy
    ));
}

#[test]
fn parse_trusted_refuses_other_peers_before_reading_bytes() {
    let proxy: IpAddr = Ipv4Addr::new(192, 0, 2, 10).into();
    let stranger: IpAddr = Ipv4Addr::new(203, 0, 113, 5).into();
    for input in [&TCP4[..], &LOCAL[..], &[][..], b"\x16\x03\x01"] {
        assert_eq!(
            parse_trusted(stranger, proxy, input),
            Err(Error::UntrustedPeer)
        );
    }
    let mapped: IpAddr = "::ffff:192.0.2.10".parse().expect("mapped");
    assert_eq!(parse_trusted(mapped, proxy, &TCP4), parse(&TCP4));
    assert_eq!(parse_trusted(proxy, proxy, b"GET /"), Err(Error::Signature));
}

#[test]
fn client_address_canonicalises_a_mapped_source() {
    let mut body = vec![0; 10];
    body.extend_from_slice(&[0xff, 0xff, 192, 0, 2, 1]);
    body.extend_from_slice(TCP6.get(32..48).expect("destination"));
    body.extend_from_slice(&[0xc3, 0x50, 0x01, 0xbb]);
    let input = header(0x21, 0x21, &body);
    let (header, _) = complete(&input);
    let Command::Proxy { source, .. } = header.command() else {
        panic!("PROXY expected");
    };
    assert_eq!(source, v6("::ffff:192.0.2.1", 50000));
    let peer = v4([203, 0, 113, 9], 40000);
    assert_eq!(header.client_address(peer), v4([192, 0, 2, 1], 50000));
}
