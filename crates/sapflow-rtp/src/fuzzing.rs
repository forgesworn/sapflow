//! Entry points shared by the fuzz targets (`fuzz/fuzz_targets/rtp_*.rs`,
//! `rtcp_*`, `srtp_*`, `srtcp_*`) and the seed regression test below, so
//! the committed seeds run through exactly the code the fuzzer drives even
//! where cargo-fuzz is not installed.
//!
//! The SRTP targets use fixed test keys ([`KEY_MATERIAL`]). Their input is
//! a mode byte and then records of `[length high, length low, bytes]`, all
//! fed to one context so that mutation reorders and repeats packets and
//! reaches index estimation and the replay window. Mode bits:
//!
//! - bit 0: profile, 0 for `SRTP_AES128_CM_HMAC_SHA1_80`, 1 for
//!   `SRTP_AEAD_AES_128_GCM`;
//! - bit 1: a 64-packet replay window instead of 1024;
//! - bit 2: one stream slot instead of eight;
//! - bit 3: records are plaintext, protected with the fixed keys and then
//!   unprotected, which must give them back unchanged.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]

use crate::rtcp::{Compound, Packet, build};
use crate::rtp::{RtpPacket, header_len};
use crate::srtp::{Config, Inbound, Outbound, Profile, Role, SrtpError, split_keying_material};

/// Exporter output the fuzz targets and seeds share. Not a secret.
pub const KEY_MATERIAL: [u8; 60] = {
    let mut m = [0u8; 60];
    let mut n = 0;
    while n < 60 {
        m[n] = (n as u8).wrapping_mul(29) ^ 0xa5;
        n += 1;
    }
    m
};

/// Most records read from one input.
pub const MAX_RECORDS: usize = 256;

/// The DTLS client's sending context and our receiving one, from
/// [`KEY_MATERIAL`].
pub fn contexts(profile: Profile, config: Config) -> (Outbound, Inbound) {
    let material = &KEY_MATERIAL[..profile.keying_material_len()];
    let client = split_keying_material(profile, Role::Client, material).expect("client keys");
    let server = split_keying_material(profile, Role::Server, material).expect("server keys");
    (
        Outbound::new(&client.local).expect("outbound"),
        Inbound::new(&server.remote, config).expect("inbound"),
    )
}

/// Splits `[len_hi, len_lo, bytes]` records, stopping at a truncated one.
pub fn records(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let (len, tail) = rest.split_first_chunk::<2>()?;
        let len = usize::from(u16::from_be_bytes(*len));
        let record = tail.get(..len)?;
        rest = &tail[len..];
        Some(record)
    })
    .take(MAX_RECORDS)
}

/// Appends one record to `out`.
pub fn push_record(out: &mut Vec<u8>, record: &[u8]) {
    let len = u16::try_from(record.len()).expect("record fits");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(record);
}

fn mode(data: &[u8]) -> Option<(Profile, Config, bool, &[u8])> {
    let (&mode, rest) = data.split_first()?;
    let profile = if mode & 1 == 0 {
        Profile::Aes128CmHmacSha1_80
    } else {
        Profile::AeadAes128Gcm
    };
    let config = Config {
        replay_window: if mode & 2 != 0 { 64 } else { 1024 },
        max_streams: if mode & 4 != 0 { 1 } else { 8 },
    };
    Some((profile, config, mode & 8 != 0, rest))
}

/// One RTP datagram: parse, and check that the parts add up. Returns
/// whether it parsed.
pub fn rtp_packet(data: &[u8]) -> bool {
    let Ok(p) = RtpPacket::parse(data) else {
        return false;
    };
    assert_eq!(header_len(data), Ok(p.header_len()));
    assert_eq!(
        p.header_len() + p.payload().len() + p.padding_len(),
        data.len()
    );
    assert_eq!(p.csrcs().count(), p.csrc_count());
    let _ = (
        p.marker(),
        p.payload_type(),
        p.sequence(),
        p.timestamp(),
        p.ssrc(),
    );
    if let Some(ext) = p.extension() {
        let _ = ext.form();
        for e in ext.elements() {
            assert!(e.data.len() <= ext.data().len());
            assert!(p.extension_element(e.id).is_some());
        }
        for id in 0..=255u8 {
            assert_eq!(
                ext.get(id),
                ext.elements().find(|e| e.id == id).map(|e| e.data)
            );
        }
    }
    true
}

/// One RTCP datagram: parse every packet, and rebuild what sapflow builds
/// (receiver reports, NACK, PLI, FIR) to check it reads back the same.
/// Returns the number of packets read.
pub fn rtcp_compound(data: &[u8]) -> usize {
    let Ok(compound) = Compound::parse(data) else {
        return 0;
    };
    let _ = compound.starts_with_report();
    let mut count = 0;
    for packet in compound.packets() {
        let Ok(packet) = packet else {
            break;
        };
        count += 1;
        let mut out = Vec::new();
        match packet {
            Packet::ReceiverReport(rr) => {
                let blocks: Vec<_> = rr.reports().collect();
                build::receiver_report(&mut out, rr.ssrc(), &blocks);
                let again: Vec<_> = Compound::parse(&out)
                    .expect("built RR parses")
                    .packets()
                    .flat_map(|p| match p.expect("built RR reads") {
                        Packet::ReceiverReport(r) => r.reports().collect::<Vec<_>>(),
                        other => panic!("not an RR: {other:?}"),
                    })
                    .collect();
                assert_eq!(again, blocks);
            }
            Packet::SenderReport(sr) => {
                let _ = (sr.ssrc(), sr.ntp_time(), sr.rtp_time());
                let _ = (sr.packet_count(), sr.octet_count(), sr.reports().count());
            }
            Packet::Nack(nack) => {
                let mut lost: Vec<u16> = nack.lost().collect();
                lost.sort_unstable();
                lost.dedup();
                build::nack(&mut out, nack.sender_ssrc(), nack.media_ssrc(), &lost)
                    .expect("sorted losses fit");
                let Some(Ok(Packet::Nack(again))) =
                    Compound::parse(&out).expect("built NACK").packets().next()
                else {
                    panic!("built NACK reads back");
                };
                let mut back: Vec<u16> = again.lost().collect();
                back.sort_unstable();
                back.dedup();
                assert_eq!(back, lost);
            }
            Packet::Pli(pli) => {
                build::pli(&mut out, pli.sender_ssrc, pli.media_ssrc);
                let first = Compound::parse(&out).expect("PLI").packets().next();
                assert_eq!(first, Some(Ok(Packet::Pli(pli))));
            }
            Packet::Fir(fir) => {
                let entries: Vec<_> = fir.entries().collect();
                if build::fir(&mut out, fir.sender_ssrc(), &entries).is_ok() {
                    let Some(Ok(Packet::Fir(again))) =
                        Compound::parse(&out).expect("built FIR").packets().next()
                    else {
                        panic!("built FIR reads back");
                    };
                    assert_eq!(again.entries().collect::<Vec<_>>(), entries);
                }
            }
            Packet::Other(other) => {
                let _ = other.body.len();
            }
        }
    }
    count
}

/// Records of SRTP into one [`Inbound`] (see the module notes). Returns
/// the number of packets that authenticated.
pub fn srtp_unprotect(data: &[u8]) -> usize {
    let Some((profile, config, round_trip, rest)) = mode(data) else {
        return 0;
    };
    let (mut out, mut inbound) = contexts(profile, config);
    let mut opened = 0;
    let mut buf = Vec::with_capacity(2048);
    for record in records(rest) {
        buf.clear();
        buf.extend_from_slice(record);
        if round_trip {
            buf.resize(record.len() + profile.rtp_overhead(), 0);
            let Ok(sealed) = out.protect_rtp(&mut buf, record.len()) else {
                continue;
            };
            match inbound.unprotect_rtp(&mut buf[..sealed]) {
                Ok(n) => {
                    assert_eq!(&buf[..n], record, "unprotect undoes protect");
                    opened += 1;
                }
                Err(SrtpError::Replayed | SrtpError::TooOld | SrtpError::TooManyStreams) => {}
                Err(e) => panic!("our own packet was refused: {e:?}"),
            }
        } else if let Ok(n) = inbound.unprotect_rtp(&mut buf) {
            assert!(n + profile.rtp_overhead() == record.len());
            let _ = rtp_packet(&buf[..n]);
            opened += 1;
        }
    }
    opened
}

/// Records of SRTCP into one [`Inbound`] (see the module notes). Returns
/// the number of packets that authenticated.
pub fn srtcp_unprotect(data: &[u8]) -> usize {
    let Some((profile, config, round_trip, rest)) = mode(data) else {
        return 0;
    };
    let (mut out, mut inbound) = contexts(profile, config);
    let mut opened = 0;
    let mut buf = Vec::with_capacity(2048);
    for record in records(rest) {
        buf.clear();
        buf.extend_from_slice(record);
        if round_trip {
            buf.resize(record.len() + profile.rtcp_overhead(), 0);
            let Ok(sealed) = out.protect_rtcp(&mut buf, record.len()) else {
                continue;
            };
            match inbound.unprotect_rtcp(&mut buf[..sealed]) {
                Ok(n) => {
                    assert_eq!(&buf[..n], record, "unprotect undoes protect");
                    opened += 1;
                }
                Err(SrtpError::TooManyStreams) => {}
                Err(e) => panic!("our own packet was refused: {e:?}"),
            }
        } else if let Ok(n) = inbound.unprotect_rtcp(&mut buf) {
            assert!(n + profile.rtcp_overhead() == record.len());
            let _ = rtcp_compound(&buf[..n]);
            opened += 1;
        }
    }
    opened
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn seeds(target: &str) -> Vec<(PathBuf, Vec<u8>)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/seeds")
            .join(target);
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|entry| entry.expect("entry").path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        assert!(!files.is_empty(), "seeds for {target}");
        files
            .into_iter()
            .map(|p| {
                let bytes = std::fs::read(&p).expect("seed");
                (p, bytes)
            })
            .collect()
    }

    #[test]
    fn rtp_seeds_parse() {
        for (path, bytes) in seeds("rtp_packet") {
            assert!(rtp_packet(&bytes), "{} parses", path.display());
        }
    }

    #[test]
    fn rtcp_seeds_parse() {
        for (path, bytes) in seeds("rtcp_compound") {
            assert!(rtcp_compound(&bytes) > 0, "{} parses", path.display());
        }
    }

    /// Every SRTP seed gets at least one packet past authentication, so the
    /// fuzzer starts beyond the tag check.
    #[test]
    fn srtp_seeds_authenticate() {
        for (path, bytes) in seeds("srtp_unprotect") {
            assert!(
                srtp_unprotect(&bytes) > 0,
                "{} authenticates",
                path.display()
            );
        }
    }

    #[test]
    fn srtcp_seeds_authenticate() {
        for (path, bytes) in seeds("srtcp_unprotect") {
            assert!(
                srtcp_unprotect(&bytes) > 0,
                "{} authenticates",
                path.display()
            );
        }
    }

    #[test]
    fn records_stop_at_truncation() {
        let mut data = Vec::new();
        push_record(&mut data, b"abc");
        push_record(&mut data, b"");
        data.extend_from_slice(&[0, 9, 1]);
        assert_eq!(records(&data).collect::<Vec<_>>(), [&b"abc"[..], b""]);
        assert_eq!(srtp_unprotect(&[]), 0);
        assert_eq!(srtcp_unprotect(&[9, 0, 1]), 0);
    }

    /// Reads a capture from `tests/capture_ffmpeg.py`: `(channel, datagram)`.
    fn capture(dir: &Path, name: &str) -> Vec<(u8, Vec<u8>)> {
        let bytes = std::fs::read(dir.join(name)).expect("capture");
        let mut out = Vec::new();
        let mut rest = &bytes[..];
        while let Some((&channel, tail)) = rest.split_first() {
            let len = usize::from(u16::from_be_bytes([tail[0], tail[1]]));
            out.push((channel, tail[2..2 + len].to_vec()));
            rest = &tail[2 + len..];
        }
        out
    }

    fn sealed_rtp(out: &mut Outbound, plain: &[u8]) -> Vec<u8> {
        let mut buf = plain.to_vec();
        buf.resize(plain.len() + 16, 0);
        let n = out.protect_rtp(&mut buf, plain.len()).expect("protect");
        buf.truncate(n);
        buf
    }

    fn sealed_rtcp(out: &mut Outbound, plain: &[u8], encrypt: bool) -> Vec<u8> {
        let mut buf = plain.to_vec();
        buf.resize(plain.len() + 20, 0);
        let n = out
            .protect_rtcp_with(&mut buf, plain.len(), encrypt)
            .expect("protect");
        buf.truncate(n);
        buf
    }

    fn with_seq(packet: &[u8], seq: u16) -> Vec<u8> {
        let mut p = packet.to_vec();
        p[2..4].copy_from_slice(&seq.to_be_bytes());
        p
    }

    /// Writes the committed seeds. Run on demand after a capture:
    /// `SAPFLOW_RTP_CAPTURE=<dir> cargo test -p sapflow-rtp --lib --
    /// --ignored write_seeds`.
    #[test]
    #[ignore = "writes fuzz/seeds from a fresh FFmpeg capture"]
    fn write_seeds() {
        use crate::rtcp::ReportBlock;
        use crate::rtp::RtpBuilder;
        let dir = PathBuf::from(std::env::var("SAPFLOW_RTP_CAPTURE").expect("capture dir"));
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
        let write = |target: &str, name: &str, bytes: &[u8]| {
            let d = root.join(target);
            std::fs::create_dir_all(&d).expect("dir");
            assert!(bytes.len() <= 65_536, "{name} within CI's -max_len");
            std::fs::write(d.join(name), bytes).expect("write");
        };
        let rtp_of = |name: &str| -> Vec<Vec<u8>> {
            capture(&dir, name)
                .into_iter()
                .filter(|(c, _)| *c == 0)
                .map(|(_, d)| d)
                .collect()
        };
        let rtcp_of = |name: &str| -> Vec<Vec<u8>> {
            capture(&dir, name)
                .into_iter()
                .filter(|(c, _)| *c == 1)
                .map(|(_, d)| d)
                .collect()
        };
        let h264 = rtp_of("h264.rec");
        let opus = rtp_of("opus.rec");
        let mut sr = rtcp_of("h264.rec");
        sr.extend(rtcp_of("opus.rec"));

        // rtp_packet: FFmpeg's packets, then shapes FFmpeg does not send.
        write("rtp_packet", "ffmpeg-h264-first", &h264[0]);
        write("rtp_packet", "ffmpeg-h264-middle", &h264[1]);
        let marked = h264.iter().find(|p| p[1] & 0x80 != 0).expect("a marker");
        write("rtp_packet", "ffmpeg-h264-marker", marked);
        write("rtp_packet", "ffmpeg-opus", &opus[0]);
        let mut mid = RtpBuilder {
            payload_type: 111,
            sequence: 9,
            timestamp: 960,
            ssrc: 0x0102_0304,
            payload: vec![0xfc; 40],
            ..RtpBuilder::default()
        };
        mid.extension = Some(RtpBuilder::one_byte_elements(&[
            (1, b"0"),
            (3, &[0, 0x40, 0x00]),
            (5, &[0, 7]),
        ]));
        write("rtp_packet", "built-one-byte-extensions", &mid.build());
        mid.extension = Some(RtpBuilder::two_byte_elements(
            0,
            &[(1, b""), (17, b"video"), (200, &[1; 20])],
        ));
        write("rtp_packet", "built-two-byte-extensions", &mid.build());
        mid.extension = None;
        mid.csrcs = vec![11, 12, 13];
        mid.padding = 4;
        mid.marker = true;
        write("rtp_packet", "built-csrcs-padding", &mid.build());

        // rtcp_compound: FFmpeg's sender reports, then what sapflow sends.
        write("rtcp_compound", "ffmpeg-h264-sr", &sr[0]);
        write("rtcp_compound", "ffmpeg-opus-sr", &sr[3]);
        let blocks = [
            ReportBlock {
                ssrc: 0x1eca_5f01,
                fraction_lost: 12,
                cumulative_lost: 40,
                highest_sequence: 0x0001_0384,
                jitter: 30,
                last_sr: 0x32d3_cb43,
                delay_since_last_sr: 0x2000,
            },
            ReportBlock {
                ssrc: 0xa19f_c242,
                cumulative_lost: -2,
                ..ReportBlock::default()
            },
        ];
        let mut compound = Vec::new();
        build::CompoundBuilder::new(&mut compound, 1, &blocks)
            .cname(b"sapflow")
            .expect("cname")
            .nack(0xa19f_c242, &[65_530, 65_531, 65_535, 2, 40])
            .expect("nack")
            .pli(0xa19f_c242)
            .fir(&[(0xa19f_c242, 3)])
            .expect("fir");
        write("rtcp_compound", "built-rr-sdes-nack-pli-fir", &compound);
        let mut pli = Vec::new();
        build::pli(&mut pli, 1, 0xa19f_c242);
        write("rtcp_compound", "built-reduced-size-pli", &pli);
        let mut sr_blocks = sr[3].clone();
        sr_blocks[0] = 0x81;
        sr_blocks[3] += 6;
        let mut block = Vec::new();
        build::receiver_report(&mut block, 0, &blocks[..1]);
        sr_blocks.extend_from_slice(&block[8..]);
        sr_blocks.extend_from_slice(&[0x81, 203, 0, 1]);
        sr_blocks.extend_from_slice(&sr[3][4..8]);
        write("rtcp_compound", "sr-with-block-and-bye", &sr_blocks);

        // srtp_unprotect: ciphertext under the fixed keys, then plaintext
        // round trips.
        let seed = |target: &str, name: &str, mode: u8, records: &[Vec<u8>]| {
            let mut bytes = vec![mode];
            for r in records {
                push_record(&mut bytes, r);
            }
            write(target, name, &bytes);
        };
        let cm = Config::default();
        let small = Config {
            replay_window: 64,
            max_streams: 1,
        };
        let (mut out, _) = contexts(Profile::Aes128CmHmacSha1_80, cm);
        let sealed: Vec<_> = h264[..20].iter().map(|p| sealed_rtp(&mut out, p)).collect();
        seed("srtp_unprotect", "cm-ffmpeg-h264", 0, &sealed);
        let (mut out, _) = contexts(Profile::AeadAes128Gcm, cm);
        let sealed: Vec<_> = opus[..30].iter().map(|p| sealed_rtp(&mut out, p)).collect();
        seed("srtp_unprotect", "gcm-ffmpeg-opus", 1, &sealed);
        // Across the sequence wrap, delivered reordered with a duplicate.
        let (mut out, _) = contexts(Profile::Aes128CmHmacSha1_80, cm);
        let wrapped: Vec<_> = opus[..12]
            .iter()
            .enumerate()
            .map(|(n, p)| sealed_rtp(&mut out, &with_seq(p, 65_530u16.wrapping_add(n as u16))))
            .collect();
        let order = [0, 1, 2, 3, 4, 6, 7, 5, 8, 7, 9, 10, 11];
        let shuffled: Vec<_> = order.iter().map(|&i| wrapped[i].clone()).collect();
        seed("srtp_unprotect", "cm-wrap-reordered", 0, &shuffled);
        // A small window: late packets inside and outside it.
        let (mut out, _) = contexts(Profile::AeadAes128Gcm, small);
        let spread: Vec<_> = (0..100u16)
            .map(|n| sealed_rtp(&mut out, &with_seq(&opus[0][..40], 1000 + n)))
            .collect();
        let order: Vec<usize> = (0..10).chain([99, 50, 30, 98, 51]).collect();
        let late: Vec<_> = order.iter().map(|&i| spread[i].clone()).collect();
        seed("srtp_unprotect", "gcm-small-window-late", 1 | 2 | 4, &late);
        let mut plain: Vec<Vec<u8>> = h264[..6].to_vec();
        plain.extend(opus[..6].iter().cloned());
        plain.push(mid.build());
        seed("srtp_unprotect", "roundtrip-cm", 8, &plain);
        seed(
            "srtp_unprotect",
            "roundtrip-gcm-one-stream",
            1 | 4 | 8,
            &plain,
        );

        // srtcp_unprotect.
        let rtcp_plain = vec![sr[0].clone(), sr[3].clone(), compound.clone(), pli.clone()];
        let (mut out, _) = contexts(Profile::Aes128CmHmacSha1_80, cm);
        let sealed: Vec<_> = rtcp_plain
            .iter()
            .enumerate()
            .map(|(n, p)| sealed_rtcp(&mut out, p, n != 1))
            .collect();
        seed("srtcp_unprotect", "cm-sr-compound-e0", 0, &sealed);
        let (mut out, _) = contexts(Profile::AeadAes128Gcm, cm);
        let sealed: Vec<_> = rtcp_plain
            .iter()
            .enumerate()
            .map(|(n, p)| sealed_rtcp(&mut out, p, n != 2))
            .collect();
        seed("srtcp_unprotect", "gcm-sr-compound-e0", 1, &sealed);
        seed("srtcp_unprotect", "roundtrip-cm", 8, &rtcp_plain);
        seed("srtcp_unprotect", "roundtrip-gcm", 1 | 8, &rtcp_plain);
    }
}
