//! RFC 3711 Appendix B and RFC 7714 sections 16 and 17 test vectors, copied
//! from the RFC text, and behaviour tests for both profiles.

use super::transform::{self, CmKeys, GcmKeys};
use super::*;
use aes_gcm::{AeadInOut, Aes128Gcm, Aes256Gcm};

fn hex(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s
        .bytes()
        .filter(|b| b.is_ascii_hexdigit())
        .map(|b| (b as char).to_digit(16).expect("hex digit") as u8)
        .collect();
    assert!(digits.len().is_multiple_of(2), "whole octets");
    digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect()
}
fn arr<const N: usize>(s: &str) -> [u8; N] {
    hex(s).try_into().expect("length")
}

// ---- RFC 3711 Appendix B ---------------------------------------------------

/// RFC 3711 Appendix B.2: AES-CM keystream for SSRC 0, ROC 0, SEQ 0.
#[test]
fn rfc3711_b2_aes_cm_keystream() {
    let keys = CmKeys::from_session(
        &arr("2B7E151628AED2A6ABF7158809CF4F3C"),
        &arr("F0F1F2F3F4F5F6F7F8F9FAFBFCFD"),
        &[0; 20],
    )
    .expect("keys");
    let iv = keys.iv(0, 0);
    assert_eq!(iv.as_slice(), hex("F0F1F2F3F4F5F6F7F8F9FAFBFCFD0000"));
    // "Keystream segment length: 1044512 octets (65282 AES blocks)".
    let mut stream = vec![0u8; 1_044_512];
    transform::keystream(&keys.cipher, &iv, &mut stream).expect("keystream");
    let block = |n: usize| &stream[16 * n..16 * n + 16];
    assert_eq!(block(0), hex("E03EAD0935C95E80E166B16DD92B4EB4"));
    assert_eq!(block(1), hex("D23513162B02D0F72A43A2FE4A5F97AB"));
    assert_eq!(block(2), hex("41E95B3BB0A2E8DD477901E4FCA894C0"));
    assert_eq!(block(0xFEFF), hex("EC8CDF7398607CB0F2D21675EA9EA1E4"));
    assert_eq!(block(0xFF00), hex("362B7C3C6773516318A077D7FC5073AE"));
    assert_eq!(block(0xFF01), hex("6A2CC3787889374FBEB4C81B17BA6C44"));
}

/// RFC 3711 Appendix B.3: key derivation with a key derivation rate of 0.
#[test]
fn rfc3711_b3_key_derivation() {
    let master = Aes128Enc::new_from_slice(&hex("E1F97A0D3E018BE0D64FA32C06DE4139")).expect("key");
    let salt = hex("0EC675AD498AFEEBB6960B3AABE6");
    assert_eq!(
        transform::prf_input(&salt, 0).expect("x"),
        arr::<16>("0EC675AD498AFEEBB6960B3AABE60000")
    );
    assert_eq!(
        transform::prf_input(&salt, 2).expect("x"),
        arr::<16>("0EC675AD498AFEE9B6960B3AABE60000")
    );
    assert_eq!(
        transform::prf_input(&salt, 1).expect("x"),
        arr::<16>("0EC675AD498AFEEAB6960B3AABE60000")
    );
    let mut cipher_key = [0u8; 16];
    transform::derive(&master, &salt, 0, &mut cipher_key).expect("derive");
    assert_eq!(
        cipher_key.as_slice(),
        hex("C61E7A93744F39EE10734AFE3FF7A087")
    );
    let mut cipher_salt = [0u8; 14];
    transform::derive(&master, &salt, 2, &mut cipher_salt).expect("derive");
    assert_eq!(cipher_salt.as_slice(), hex("30CBBC08863D8C85D49DB34A9AE1"));
    // "a 94-octet session authentication key".
    let mut auth = [0u8; 94];
    transform::derive(&master, &salt, 1, &mut auth).expect("derive");
    assert_eq!(
        auth.as_slice(),
        hex("CEBE321F6FF7716B6FD4AB49AF256A15
             6D38BAA48F0A0ACF3C34E2359E6CDBCE
             E049646C43D9327AD175578EF7227098
             6371C10C9A369AC2F94A8C5FBCDDDC25
             6D6E919A48B610EF17C2041E47403576
             6B68642C59BBFC2F34DB60DBDFB2")
    );
}

// ---- RFC 7714 section 16 (RTP) ----------------------------------------------

const RTP_PLAIN: &str = "8040f17b 8041f8d3 5501a0b2 47616c6c
                         69612065 7374206f 6d6e6973 20646976
                         69736120 696e2070 61727465 73207472
                         6573";
const SALT: &str = "51756964 2070726f 2071756f";
const KEY128: &str = "000102030405060708090a0b0c0d0e0f";
const KEY256: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn gcm128() -> Transform {
    let k = || GcmKeys::<Aes128Gcm>::from_session(&hex(KEY128), &arr(SALT)).expect("keys");
    Transform::Gcm {
        rtp: Box::new(k()),
        rtcp: Box::new(k()),
    }
}
fn gcm256() -> GcmKeys<Aes256Gcm> {
    GcmKeys::<Aes256Gcm>::from_session(&hex(KEY256), &arr(SALT)).expect("keys")
}

#[test]
fn rfc7714_section16_iv() {
    let iv = transform::gcm_rtp_iv(&arr(SALT), 0x5501_a0b2, 0xf17b);
    assert_eq!(iv.as_slice(), hex("51 75 3c 65 80 c2 72 6f 20 71 84 14"));
}

/// RFC 7714 section 16.1.1: SRTP AEAD_AES_128_GCM encryption.
#[test]
fn rfc7714_16_1_1_encrypt() {
    let mut out = Outbound::with_transform(gcm128());
    let plain = hex(RTP_PLAIN);
    let mut buf = plain.clone();
    buf.resize(plain.len() + 16, 0);
    let n = out.protect_rtp(&mut buf, plain.len()).expect("protects");
    assert_eq!(
        &buf[..n],
        hex("8040f17b 8041f8d3 5501a0b2 f24de3a3
             fb34de6c acba861c 9d7e4bca be633bd5
             0d294e6f 42a5f47a 51c7d19b 36de3adf
             8833899d 7f27beb1 6a9152cf 765ee439
             0cce")
    );
}

/// RFC 7714 section 16.1.2: SRTP AEAD_AES_128_GCM decryption.
#[test]
fn rfc7714_16_1_2_decrypt() {
    let mut inbound = Inbound::with_transform(gcm128(), Config::default()).expect("ctx");
    let mut buf = hex("8040f17b 8041f8d3 5501a0b2 f24de3a3
                       fb34de6c acba861c 9d7e4bca be633bd5
                       0d294e6f 42a5f47a 51c7d19b 36de3adf
                       8833899d 7f27beb1 6a9152cf 765ee439
                       0cce");
    let n = inbound.unprotect_rtp(&mut buf).expect("verifies");
    assert_eq!(&buf[..n], hex(RTP_PLAIN));
}

/// RFC 7714 sections 16.1.3 and 16.1.4: GMAC tag over the whole packet
/// with the SRTP IV. (Profile SRTP always encrypts; this checks the IV and
/// AAD handling against the vector.)
#[test]
fn rfc7714_16_1_3_and_16_1_4_tag_only() {
    let keys = GcmKeys::<Aes128Gcm>::from_session(&hex(KEY128), &arr(SALT)).expect("keys");
    let iv = transform::gcm_rtp_iv(&keys.salt, 0x5501_a0b2, 0xf17b);
    let tag = keys
        .cipher
        .encrypt_inout_detached(&iv, &hex(RTP_PLAIN), (&mut [0u8; 0][..]).into())
        .expect("tag");
    assert_eq!(tag.as_slice(), hex("22493f82 d2bce397 e9d79e3b 19aa4216"));
    keys.cipher
        .decrypt_inout_detached(&iv, &hex(RTP_PLAIN), (&mut [0u8; 0][..]).into(), &tag)
        .expect("16.1.4 verifies");
}

/// RFC 7714 sections 16.2.1 to 16.2.4, AEAD_AES_256_GCM, through the same
/// IV, AAD and layout code (AES-256 is not an offered profile).
#[test]
fn rfc7714_16_2_aes256() {
    let keys = gcm256();
    let plain = hex(RTP_PLAIN);
    let sealed = hex("8040f17b 8041f8d3 5501a0b2 32b1de78
                      a822fe12 ef9f78fa 332e33aa b1801238
                      9a58e2f3 b50b2a02 76ffae0f 1ba63799
                      b87b7aa3 db36dfff d6b0f9bb 7878d7a7
                      6c13");
    let mut buf = plain.clone();
    buf.resize(plain.len() + 16, 0);
    let n = transform::gcm_rtp_protect(&keys, &mut buf, plain.len(), 12, 0x5501_a0b2, 0xf17b)
        .expect("16.2.1");
    assert_eq!(&buf[..n], sealed);
    let mut buf = sealed.clone();
    let n = transform::gcm_rtp_unprotect(&keys, &mut buf, 12, 0x5501_a0b2, 0xf17b).expect("16.2.2");
    assert_eq!(&buf[..n], plain);
    let iv = transform::gcm_rtp_iv(&keys.salt, 0x5501_a0b2, 0xf17b);
    let tag = keys
        .cipher
        .encrypt_inout_detached(&iv, &plain, (&mut [0u8; 0][..]).into())
        .expect("16.2.3");
    assert_eq!(tag.as_slice(), hex("a866d591 0f887463 067ceefe c45215d4"));
}

// ---- RFC 7714 section 17 (RTCP) ---------------------------------------------

// The packet used by sections 17.1 to 17.4 (not the one in the section 17
// preamble, which differs in three words).
const RTCP_PLAIN: &str = "81c8000d 4d617273 4e545031 4e545032
                          52545020 0000042a 0000e930 4c756e61
                          deadbeef deadbeef deadbeef deadbeef
                          deadbeef";

#[test]
fn rfc7714_section17_iv() {
    let iv = transform::gcm_rtcp_iv(&arr(SALT), 0x4d61_7273, 0x5d4);
    assert_eq!(iv.as_slice(), hex("51 75 24 05 52 03 72 6f 20 71 70 bb"));
}

/// RFC 7714 section 17.1: SRTCP AEAD_AES_128_GCM encryption and tagging.
#[test]
fn rfc7714_17_1_encrypt() {
    let sealed = hex("81c8000d 4d617273 63e94885 dcdab67c
                      a727d766 2f6b7e99 7ff5c0f7 6c06f32d
                      c676a5f1 730d6fda 4ce09b46 86303ded
                      0bb9275b c84aa458 96cf4d2f c5abf872
                      45d9eade 800005d4");
    let mut out = Outbound::with_transform(gcm128());
    out.rtcp_index = 0x5d4;
    let plain = hex(RTCP_PLAIN);
    let mut buf = plain.clone();
    buf.resize(plain.len() + 20, 0);
    let n = out.protect_rtcp(&mut buf, plain.len()).expect("protects");
    assert_eq!(&buf[..n], sealed);
    assert_eq!(out.rtcp_index, 0x5d5);
    let mut inbound = Inbound::with_transform(gcm128(), Config::default()).expect("ctx");
    let mut buf = sealed.clone();
    let n = inbound.unprotect_rtcp(&mut buf).expect("verifies");
    assert_eq!(&buf[..n], plain);
}

/// RFC 7714 section 17.2: SRTCP AEAD_AES_256_GCM verification and
/// decryption.
#[test]
fn rfc7714_17_2_aes256_decrypt() {
    let sealed = hex("81c8000d 4d617273 d50ae4d1 f5ce5d30
                      4ba297e4 7d470c28 2c3ece5d bffe0a50
                      a2eaa5c1 110555be 8415f658 c61de047
                      6f1b6fad 1d1eb30c 4446839f 57ff6f6c
                      b26ac3be 800005d4");
    let keys = gcm256();
    let mut buf = sealed.clone();
    let n = transform::gcm_rtcp_unprotect(&keys, &mut buf).expect("verifies");
    assert_eq!(&buf[..n], hex(RTCP_PLAIN));
    let plain = hex(RTCP_PLAIN);
    let mut buf = plain.clone();
    buf.resize(plain.len() + 20, 0);
    let n = transform::gcm_rtcp_protect(&keys, &mut buf, plain.len(), 0x5d4, true).expect("seal");
    assert_eq!(&buf[..n], sealed);
}

/// RFC 7714 section 17.3: SRTCP AEAD_AES_128_GCM tagging only (E = 0).
#[test]
fn rfc7714_17_3_tag_only() {
    let tagged = hex("81c8000d 4d617273 4e545031 4e545032
                      52545020 0000042a 0000e930 4c756e61
                      deadbeef deadbeef deadbeef deadbeef
                      deadbeef 841dd968 3dd78ec9 2ae58790
                      125f62b3 000005d4");
    let mut out = Outbound::with_transform(gcm128());
    out.rtcp_index = 0x5d4;
    let plain = hex(RTCP_PLAIN);
    let mut buf = plain.clone();
    buf.resize(plain.len() + 20, 0);
    let n = out
        .protect_rtcp_with(&mut buf, plain.len(), false)
        .expect("tags");
    assert_eq!(&buf[..n], tagged);
    let mut inbound = Inbound::with_transform(gcm128(), Config::default()).expect("ctx");
    let mut buf = tagged.clone();
    let n = inbound.unprotect_rtcp(&mut buf).expect("verifies");
    assert_eq!(&buf[..n], plain);
}

/// RFC 7714 section 17.4: SRTCP AEAD_AES_256_GCM tag verification (E = 0).
#[test]
fn rfc7714_17_4_aes256_tag_verify() {
    let tagged = hex("81c8000d 4d617273 4e545031 4e545032
                      52545020 0000042a 0000e930 4c756e61
                      deadbeef deadbeef deadbeef deadbeef
                      deadbeef 91db4afb feee5a97 8fab4393
                      ed2615fe 000005d4");
    let keys = gcm256();
    let mut buf = tagged.clone();
    let n = transform::gcm_rtcp_unprotect(&keys, &mut buf).expect("verifies");
    assert_eq!(&buf[..n], hex(RTCP_PLAIN));
    let plain = hex(RTCP_PLAIN);
    let mut buf = plain.clone();
    buf.resize(plain.len() + 20, 0);
    let n = transform::gcm_rtcp_protect(&keys, &mut buf, plain.len(), 0x5d4, false).expect("tag");
    assert_eq!(&buf[..n], tagged);
}

// ---- Behaviour ----------------------------------------------------------------

const PROFILES: [Profile; 2] = [Profile::Aes128CmHmacSha1_80, Profile::AeadAes128Gcm];

fn material(profile: Profile) -> Vec<u8> {
    (0..profile.keying_material_len() as u8)
        .map(|n| n.wrapping_mul(37) ^ 0x5a)
        .collect()
}

/// A sender (DTLS client) and our receiver (DTLS server) from one
/// exporter output.
fn pair(profile: Profile, config: Config) -> (Outbound, Inbound) {
    let m = material(profile);
    let client = split_keying_material(profile, Role::Client, &m).expect("client keys");
    let server = split_keying_material(profile, Role::Server, &m).expect("server keys");
    (
        Outbound::new(&client.local).expect("outbound"),
        Inbound::new(&server.remote, config).expect("inbound"),
    )
}

fn rtp(seq: u16, ssrc: u32, payload_len: usize) -> Vec<u8> {
    crate::rtp::RtpBuilder {
        payload_type: 96,
        sequence: seq,
        timestamp: u32::from(seq) * 3000,
        ssrc,
        payload: (0..payload_len).map(|n| (n as u8) ^ (seq as u8)).collect(),
        ..Default::default()
    }
    .build()
}

fn seal(out: &mut Outbound, plain: &[u8]) -> Vec<u8> {
    let mut buf = plain.to_vec();
    buf.resize(plain.len() + 16, 0);
    let n = out.protect_rtp(&mut buf, plain.len()).expect("protects");
    buf.truncate(n);
    buf
}

fn open(inbound: &mut Inbound, sealed: &[u8]) -> Result<Vec<u8>, SrtpError> {
    let mut buf = sealed.to_vec();
    let n = inbound.unprotect_rtp(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

#[test]
fn role_split_assigns_directions() {
    for profile in PROFILES {
        let m = material(profile);
        let (k, s) = (profile.master_key_len(), profile.master_salt_len());
        let server = split_keying_material(profile, Role::Server, &m).expect("keys");
        assert_eq!(server.local.key.as_slice(), &m[k..2 * k], "server writes");
        assert_eq!(server.local.salt(), &m[2 * k + s..]);
        assert_eq!(server.remote.key.as_slice(), &m[..k], "client writes");
        assert_eq!(server.remote.salt(), &m[2 * k..2 * k + s]);
        let client = split_keying_material(profile, Role::Client, &m).expect("keys");
        assert_eq!(client.local.key.as_slice(), server.remote.key.as_slice());
        assert_eq!(client.remote.salt(), server.local.salt());
        assert_eq!(
            split_keying_material(profile, Role::Server, &m[1..]).map(|_| ()),
            Err(SrtpError::Config)
        );
        // The two directions do not interoperate with each other's keys.
        let mut wrong = Inbound::new(&server.local, Config::default()).expect("ctx");
        let mut out = Outbound::new(&client.local).expect("ctx");
        let sealed = seal(&mut out, &rtp(1, 7, 100));
        assert_eq!(open(&mut wrong, &sealed), Err(SrtpError::Authentication));
    }
    assert!(
        !format!(
            "{:?}",
            MasterKey::new(Profile::AeadAes128Gcm, &[9; 16], &[9; 12]).expect("key")
        )
        .contains('9')
    );
}

#[test]
fn round_trip_both_profiles() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        assert_eq!(inbound.profile(), profile);
        for (seq, len) in [(100u16, 0usize), (101, 1), (102, 1200), (103, 17)] {
            let plain = rtp(seq, 0x1111, len);
            let sealed = seal(&mut out, &plain);
            assert_eq!(sealed.len(), plain.len() + profile.rtp_overhead());
            if len >= 16 {
                assert_ne!(sealed[12..plain.len()], plain[12..], "{profile:?} encrypts");
            }
            assert_eq!(open(&mut inbound, &sealed).expect("opens"), plain);
        }
        // With CSRCs, an extension and padding.
        let plain = crate::rtp::RtpBuilder {
            sequence: 104,
            ssrc: 0x1111,
            csrcs: vec![1, 2],
            extension: Some(crate::rtp::RtpBuilder::one_byte_elements(&[(1, b"0")])),
            payload: vec![7; 30],
            padding: 3,
            ..Default::default()
        }
        .build();
        let sealed = seal(&mut out, &plain);
        let opened = open(&mut inbound, &sealed).expect("opens");
        let p = crate::rtp::RtpPacket::parse(&opened).expect("RTP");
        assert_eq!(p.payload(), [7; 30]);
        assert_eq!(p.extension_element(1), Some(&b"0"[..]));
    }
}

#[test]
fn roc_rolls_over_across_sequence_wrap() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        let seqs: Vec<u16> = (65_530..=65_535).chain(0..10).collect();
        let sealed: Vec<_> = seqs
            .iter()
            .map(|&s| seal(&mut out, &rtp(s, 5, 40)))
            .collect();
        // Deliver with the wrap reordered: 0 and 1 before 65535.
        let order = [0, 1, 2, 3, 4, 6, 7, 5, 8, 9, 10, 11, 12, 13, 14, 15];
        for &i in &order {
            assert_eq!(
                open(&mut inbound, &sealed[i]).expect("opens"),
                rtp(seqs[i], 5, 40),
                "{profile:?} seq {}",
                seqs[i]
            );
        }
        let slot = inbound.rtp.find(5).expect("stream");
        assert_eq!(inbound.rtp.window(slot).expect("w").top(), Some(65_536 + 9));
        // A second wrap.
        for s in (10..=65_535).chain(0..3) {
            let sealed = seal(&mut out, &rtp(s, 5, 4));
            open(&mut inbound, &sealed).expect("opens");
        }
        let top = inbound.rtp.window(slot).expect("w").top();
        assert_eq!(top, Some(2 * 65_536 + 2));
    }
}

#[test]
fn replay_window_rules() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        let sealed: Vec<_> = (0..1200u16)
            .map(|s| seal(&mut out, &rtp(s, 9, 20)))
            .collect();
        open(&mut inbound, &sealed[0]).expect("first");
        // Hold back 1..=300, as if lost, and deliver the rest.
        for packet in &sealed[301..1200] {
            open(&mut inbound, packet).expect("in order");
        }
        // Duplicate of a received index.
        assert_eq!(open(&mut inbound, &sealed[1199]), Err(SrtpError::Replayed));
        assert_eq!(open(&mut inbound, &sealed[700]), Err(SrtpError::Replayed));
        // Late but never received, still inside the 1024 window: accepted,
        // once. This is a resend on the original SSRC and index.
        assert_eq!(
            open(&mut inbound, &sealed[200]).expect("late"),
            rtp(200, 9, 20)
        );
        assert_eq!(open(&mut inbound, &sealed[200]), Err(SrtpError::Replayed));
        // Behind the window.
        assert_eq!(open(&mut inbound, &sealed[100]), Err(SrtpError::TooOld));
        assert_eq!(open(&mut inbound, &sealed[0]), Err(SrtpError::TooOld));
    }
}

#[test]
fn small_window_is_configurable() {
    let config = Config {
        replay_window: 64,
        max_streams: 1,
    };
    let (mut out, mut inbound) = pair(Profile::AeadAes128Gcm, config);
    let sealed: Vec<_> = (0..100u16)
        .map(|s| seal(&mut out, &rtp(s, 9, 20)))
        .collect();
    open(&mut inbound, &sealed[99]).expect("ahead");
    assert_eq!(
        open(&mut inbound, &sealed[36]).expect("inside"),
        rtp(36, 9, 20)
    );
    assert_eq!(open(&mut inbound, &sealed[35]), Err(SrtpError::TooOld));
}

#[test]
fn tampering_is_refused_and_changes_nothing() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        let sealed = seal(&mut out, &rtp(10, 3, 64));
        let overhead = profile.rtp_overhead();
        // The last tag octet, a header octet (AAD), a payload octet.
        for at in [sealed.len() - 1, sealed.len() - overhead, 1, 5, 11, 12, 40] {
            let mut bad = sealed.clone();
            bad[at] ^= 0x01;
            let result = open(&mut inbound, &bad);
            assert!(
                matches!(
                    result,
                    Err(SrtpError::Authentication | SrtpError::Malformed)
                ),
                "{profile:?} at {at}: {result:?}"
            );
        }
        // No stream was created and the genuine packet still opens.
        assert_eq!(inbound.rtp.tracked(), 0);
        assert_eq!(open(&mut inbound, &sealed).expect("opens"), rtp(10, 3, 64));
        // A tampered later packet does not move the window.
        let next = seal(&mut out, &rtp(5000, 3, 64));
        let mut bad = next.clone();
        bad[20] ^= 0x80;
        assert_eq!(open(&mut inbound, &bad), Err(SrtpError::Authentication));
        let slot = inbound.rtp.find(3).expect("stream");
        assert_eq!(inbound.rtp.window(slot).expect("w").top(), Some(10));
        open(&mut inbound, &next).expect("genuine");
    }
}

fn rtcp_rr(ssrc: u32) -> Vec<u8> {
    let mut out = Vec::new();
    crate::rtcp::build::CompoundBuilder::new(
        &mut out,
        ssrc,
        &[crate::rtcp::ReportBlock {
            ssrc: 77,
            jitter: 5,
            ..Default::default()
        }],
    )
    .pli(77);
    out
}

fn seal_rtcp(out: &mut Outbound, plain: &[u8], encrypt: bool) -> Vec<u8> {
    let mut buf = plain.to_vec();
    buf.resize(plain.len() + 20, 0);
    let n = out
        .protect_rtcp_with(&mut buf, plain.len(), encrypt)
        .expect("protects");
    buf.truncate(n);
    buf
}

fn open_rtcp(inbound: &mut Inbound, sealed: &[u8]) -> Result<Vec<u8>, SrtpError> {
    let mut buf = sealed.to_vec();
    let n = inbound.unprotect_rtcp(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

#[test]
fn srtcp_e_flag_and_index() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        let plain = rtcp_rr(0xabcd);
        let (word_at, tag) = match profile {
            Profile::Aes128CmHmacSha1_80 => (plain.len(), 10),
            Profile::AeadAes128Gcm => (plain.len() + 16, 16),
        };
        let encrypted = seal_rtcp(&mut out, &plain, true);
        let clear = seal_rtcp(&mut out, &plain, false);
        assert_eq!(encrypted.len(), plain.len() + 4 + tag);
        assert_eq!(
            &encrypted[word_at..word_at + 4],
            [0x80, 0, 0, 0],
            "E=1, index 0"
        );
        assert_eq!(&clear[word_at..word_at + 4], [0, 0, 0, 1], "E=0, index 1");
        assert_eq!(encrypted[..8], plain[..8], "first eight octets in clear");
        assert_ne!(encrypted[8..plain.len()], plain[8..]);
        assert_eq!(clear[..plain.len()], plain[..], "E=0 leaves RTCP readable");
        assert_eq!(open_rtcp(&mut inbound, &encrypted).expect("opens"), plain);
        assert_eq!(open_rtcp(&mut inbound, &clear).expect("opens"), plain);
        assert_eq!(open_rtcp(&mut inbound, &clear), Err(SrtpError::Replayed));
        // Flipping the E flag breaks authentication.
        let mut flipped = seal_rtcp(&mut out, &plain, true);
        flipped[word_at] ^= 0x80;
        assert_eq!(
            open_rtcp(&mut inbound, &flipped),
            Err(SrtpError::Authentication)
        );
        // Changing the index does too.
        let mut moved = seal_rtcp(&mut out, &plain, true);
        moved[word_at + 3] ^= 0x40;
        assert_eq!(
            open_rtcp(&mut inbound, &moved),
            Err(SrtpError::Authentication)
        );
        // Tampered header octet (in the AAD in both forms).
        let mut header = seal_rtcp(&mut out, &plain, true);
        header[1] ^= 1;
        assert_eq!(
            open_rtcp(&mut inbound, &header),
            Err(SrtpError::Authentication)
        );
        // The parser reads what came out.
        let opened = open_rtcp(&mut inbound, &seal_rtcp(&mut out, &plain, true)).expect("opens");
        let c = crate::rtcp::Compound::parse(&opened).expect("RTCP");
        assert_eq!(c.packets().count(), 2);
    }
}

#[test]
fn srtcp_replay_is_per_ssrc() {
    for profile in PROFILES {
        // Two senders under the same key, each with its own index sequence.
        let (mut a, mut inbound) = pair(profile, Config::default());
        let mut b = pair(profile, Config::default()).0;
        let first_a = seal_rtcp(&mut a, &rtcp_rr(1), true);
        let first_b = seal_rtcp(&mut b, &rtcp_rr(2), true);
        open_rtcp(&mut inbound, &first_a).expect("a index 0");
        open_rtcp(&mut inbound, &first_b).expect("b index 0, another SSRC");
        assert_eq!(open_rtcp(&mut inbound, &first_a), Err(SrtpError::Replayed));
        assert_eq!(open_rtcp(&mut inbound, &first_b), Err(SrtpError::Replayed));
    }
}

#[test]
fn srtcp_index_refuses_to_wrap() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        out.rtcp_index = 0x7fff_ffff;
        let last = seal_rtcp(&mut out, &rtcp_rr(1), true);
        open_rtcp(&mut inbound, &last).expect("the last index is usable");
        let plain = rtcp_rr(1);
        let mut buf = plain.clone();
        buf.resize(plain.len() + 20, 0);
        assert_eq!(
            out.protect_rtcp(&mut buf, plain.len()),
            Err(SrtpError::Exhausted)
        );
    }
}

#[test]
fn stream_cap_is_enforced_before_cryptography() {
    let config = Config {
        replay_window: 1024,
        max_streams: 2,
    };
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, config);
        open(&mut inbound, &seal(&mut out, &rtp(1, 10, 8))).expect("one");
        open(&mut inbound, &seal(&mut out, &rtp(1, 11, 8))).expect("two");
        assert_eq!(
            open(&mut inbound, &seal(&mut out, &rtp(1, 12, 8))),
            Err(SrtpError::TooManyStreams)
        );
        open(&mut inbound, &seal(&mut out, &rtp(2, 10, 8))).expect("known stream");
        // A forged packet for a new SSRC does not take a slot.
        let (mut _o, mut fresh) = pair(profile, config);
        let mut junk = rtp(1, 99, 30);
        junk.extend_from_slice(&[0; 16]);
        assert_eq!(open(&mut fresh, &junk), Err(SrtpError::Authentication));
        assert_eq!(fresh.rtp.tracked(), 0);
    }
}

#[test]
fn malformed_and_truncated_inputs() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        let mut b = crate::rtp::RtpBuilder {
            sequence: 1,
            ssrc: 4,
            csrcs: vec![1],
            payload: vec![1; 20],
            ..Default::default()
        };
        b.extension = Some(crate::rtp::RtpBuilder::one_byte_elements(&[(2, &[1, 2])]));
        let sealed = seal(&mut out, &b.build());
        for len in 0..sealed.len() {
            assert!(
                open(&mut inbound, &sealed[..len]).is_err(),
                "{profile:?} {len}"
            );
        }
        let sealed_rtcp = seal_rtcp(&mut out, &rtcp_rr(1), true);
        for len in 0..sealed_rtcp.len() {
            assert!(
                open_rtcp(&mut inbound, &sealed_rtcp[..len]).is_err(),
                "{len}"
            );
        }
        assert_eq!(inbound.rtp.tracked() + inbound.rtcp.tracked(), 0);
        assert_eq!(open(&mut inbound, &[0x40; 40]), Err(SrtpError::Malformed));
        // Protect needs room for the trailer.
        let plain = rtp(2, 4, 10);
        let mut tight = plain.clone();
        assert_eq!(
            out.protect_rtp(&mut tight, plain.len()),
            Err(SrtpError::BufferTooSmall)
        );
        let rr = rtcp_rr(1);
        let mut tight = rr.clone();
        tight.resize(rr.len() + profile.rtcp_overhead() - 1, 0);
        assert_eq!(
            out.protect_rtcp(&mut tight, rr.len()),
            Err(SrtpError::BufferTooSmall)
        );
        let mut short = [0x80u8; 30];
        assert_eq!(out.protect_rtcp(&mut short, 7), Err(SrtpError::Malformed));
    }
}

#[test]
fn every_bit_flip_is_refused() {
    for profile in PROFILES {
        let (mut out, mut inbound) = pair(profile, Config::default());
        let sealed = seal(&mut out, &rtp(1, 4, 24));
        for at in 0..sealed.len() {
            for bit in 0..8 {
                let mut m = sealed.clone();
                m[at] ^= 1 << bit;
                assert!(open(&mut inbound, &m).is_err(), "{profile:?} {at}.{bit}");
            }
        }
        let sealed = seal_rtcp(&mut out, &rtcp_rr(1), true);
        for at in 0..sealed.len() {
            for bit in 0..8 {
                let mut m = sealed.clone();
                m[at] ^= 1 << bit;
                assert!(
                    open_rtcp(&mut inbound, &m).is_err(),
                    "{profile:?} {at}.{bit}"
                );
            }
        }
    }
}

#[test]
fn configuration_and_key_checks() {
    let key = MasterKey::new(Profile::Aes128CmHmacSha1_80, &[1; 16], &[2; 14]).expect("key");
    for (window, streams) in [
        (0, 8),
        (63, 8),
        (100, 8),
        (32_832, 8),
        (1024, 0),
        (1024, 65),
    ] {
        let config = Config {
            replay_window: window,
            max_streams: streams,
        };
        assert_eq!(
            Inbound::new(&key, config).map(|_| ()),
            Err(SrtpError::Config)
        );
    }
    assert!(
        Inbound::new(
            &key,
            Config {
                replay_window: 32_768,
                max_streams: 64
            }
        )
        .is_ok()
    );
    assert!(MasterKey::new(Profile::Aes128CmHmacSha1_80, &[1; 16], &[2; 12]).is_err());
    assert!(MasterKey::new(Profile::AeadAes128Gcm, &[1; 16], &[2; 14]).is_err());
    assert!(MasterKey::new(Profile::AeadAes128Gcm, &[1; 15], &[2; 12]).is_err());
    for profile in PROFILES {
        assert_eq!(Profile::from_dtls_id(profile.dtls_id()), Some(profile));
    }
    assert_eq!(Profile::from_dtls_id(0x0002), None);
    assert_eq!(Profile::Aes128CmHmacSha1_80.keying_material_len(), 60);
    assert_eq!(Profile::AeadAes128Gcm.keying_material_len(), 56);
    assert_eq!(Profile::AeadAes128Gcm.rtcp_overhead(), 20);
    assert_eq!(Profile::Aes128CmHmacSha1_80.rtcp_overhead(), 14);
}

/// Records `[channel, u16 length, datagram]` from `tests/capture_ffmpeg.py`.
fn fixture_records(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some((&channel, tail)) = rest.split_first() {
        let len = usize::from(u16::from_be_bytes([tail[0], tail[1]]));
        out.push((channel, tail[2..2 + len].to_vec()));
        rest = &tail[2 + len..];
    }
    out
}

/// FFmpeg 8.1.2, run as a black box with `-srtp_out_suite
/// AES_CM_128_HMAC_SHA1_80` and a public test key
/// (`tests/fixtures/README.md`): every SRTP and SRTCP packet it sent
/// unprotects with our code, reads as RTP or RTCP, and protects back with
/// our code to exactly FFmpeg's bytes.
#[test]
fn ffmpeg_srtp_interop() {
    let records = fixture_records(include_bytes!("../../tests/fixtures/ffmpeg-srtp-h264.rec"));
    let key: Vec<u8> = (0x10..0x20).collect();
    let salt: Vec<u8> = (0x40..0x4e).collect();
    let master = MasterKey::new(Profile::Aes128CmHmacSha1_80, &key, &salt).expect("key");
    let mut inbound = Inbound::new(&master, Config::default()).expect("inbound");
    let mut outbound = Outbound::new(&master).expect("outbound");
    let (mut rtp_count, mut rtcp_count) = (0, 0);
    let mut first: Option<(u32, u16)> = None;
    for (channel, sealed) in records {
        let mut buf = sealed.clone();
        if channel == 0 {
            let n = inbound
                .unprotect_rtp(&mut buf)
                .expect("FFmpeg SRTP unprotects");
            let plain = buf[..n].to_vec();
            let p = crate::rtp::RtpPacket::parse(&plain).expect("RTP");
            assert_eq!(p.payload_type(), 96);
            let (ssrc, seq) = *first.get_or_insert((p.ssrc(), p.sequence()));
            assert_eq!(p.ssrc(), ssrc);
            assert_eq!(p.sequence(), seq.wrapping_add(rtp_count as u16));
            let nal = p.payload()[0];
            assert_eq!(nal & 0x80, 0, "forbidden bit");
            assert!(matches!(nal & 0x1f, 1..=24 | 28), "NAL type {}", nal & 0x1f);
            let mut again = plain.clone();
            again.resize(plain.len() + 10, 0);
            let m = outbound
                .protect_rtp(&mut again, plain.len())
                .expect("protects");
            assert_eq!(again[..m], sealed[..], "our SRTP matches FFmpeg's");
            rtp_count += 1;
        } else {
            let n = inbound
                .unprotect_rtcp(&mut buf)
                .expect("FFmpeg SRTCP unprotects");
            let plain = buf[..n].to_vec();
            let c = crate::rtcp::Compound::parse(&plain).expect("RTCP");
            let Some(Ok(crate::rtcp::Packet::SenderReport(sr))) = c.packets().next() else {
                panic!("FFmpeg sends sender reports");
            };
            if let Some((ssrc, _)) = first {
                assert_eq!(sr.ssrc(), ssrc);
            }
            let mut again = plain.clone();
            again.resize(plain.len() + 14, 0);
            let m = outbound
                .protect_rtcp(&mut again, plain.len())
                .expect("protects");
            assert_eq!(again[..m], sealed[..], "our SRTCP matches FFmpeg's");
            rtcp_count += 1;
        }
    }
    assert_eq!((rtp_count, rtcp_count), (30, 2));
}
