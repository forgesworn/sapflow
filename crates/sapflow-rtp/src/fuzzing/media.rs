//! Entry points for the media fuzz targets (`h264_depacketise`,
//! `opus_depacketise`, `reorder_nack`) and their seed regression test.
//! Inputs use the `[u16 length, bytes]` records of [`super::records`].
//!
//! - `h264_depacketise`: a mode byte (bit 0: small limits), then records
//!   of `[flags, RTP payload]`; flag bit 0 is the marker, bit 1 starts a
//!   new timestamp, bit 2 signals a loss before the packet.
//! - `opus_depacketise`: the whole input is first parsed as one Opus
//!   packet (the TOC and framing). Then a mode byte (bit 0: a 100 ms fill
//!   limit) and records of `[flags, step high, step low, Opus packet]`;
//!   the timestamp advances by the step (times 120 unless flag bit 1),
//!   flag bit 2 adds 2^31, and bit 0 signals a loss before the packet.
//! - `reorder_nack`: a mode byte and records of operations on a
//!   [`Receiver`] with a video stream (RTX negotiated) and an audio stream:
//!   video, audio and RTX packets with explicit sequence numbers, clock
//!   steps forward and back, and packets for no stream. See
//!   [`reorder_nack`].
//!
//! Each checks what it produces, not only that nothing panics: access
//! units re-parse within their limits, the Opus timeline has no holes
//! except at a reported discontinuity, buffers stay within their bounds,
//! NACKs read back through the RTCP parser and name only packets still
//! missing, and the next timer is always later than the one just run.

use super::records;
use crate::h264::{self, AccessUnit, H264Limits, H264Output};
use crate::opus::{self, FrameKind, OpusLimits, OpusOutput, parse_packet};
use crate::receive::{
    Codec, Event, Feedback, Receiver, ReceiverConfig, RtxConfig, StreamConfig, StreamId,
};
use crate::rtcp::{Compound, Packet};
use crate::rtp::{RtpBuilder, RtpPacket};
use std::time::{Duration, Instant};

/// Checks an access unit against the limits it was built under.
pub fn check_access_unit(au: &AccessUnit, limits: &H264Limits) {
    let mut units = 0usize;
    let mut total = 0usize;
    let mut idr = false;
    for unit in au.nal_units() {
        let header = unit[0];
        assert_eq!(header & 0x80, 0, "forbidden bit");
        let kind = header & 0x1f;
        assert!((1..=23).contains(&kind), "a NAL unit type, not {kind}");
        assert!(
            !(7..=12).contains(&kind),
            "parameter sets and delimiters are out"
        );
        assert!(unit.len() <= limits.max_nal_bytes);
        idr |= kind == 5;
        units += 1;
        total += 4 + unit.len();
    }
    assert_eq!(total, au.data.len(), "length prefixes add up");
    assert_eq!(idr, au.idr);
    for s in &au.sps {
        assert_eq!(s[0] & 0x9f, 7);
    }
    for p in &au.pps {
        assert_eq!(p[0] & 0x9f, 8);
    }
    let sets: usize = au.sps.iter().chain(&au.pps).map(|s| 4 + s.len()).sum();
    assert!(units + au.sps.len() + au.pps.len() <= limits.max_nal_units);
    assert!(total + sets <= limits.max_frame_bytes);
}

fn small_h264() -> H264Limits {
    H264Limits {
        max_frame_bytes: 4096,
        max_nal_bytes: 2048,
        max_nal_units: 8,
    }
}

/// H.264 depacketising. Returns the number of access units.
pub fn h264_depacketise(data: &[u8]) -> usize {
    let Some((&mode, rest)) = data.split_first() else {
        return 0;
    };
    let limits = if mode & 1 != 0 {
        small_h264()
    } else {
        H264Limits::default()
    };
    let mut d = h264::Depacketiser::new(limits);
    let mut ts = 0u32;
    let mut frames = 0;
    let mut last: Option<u32> = None;
    let mut check = |d: &mut h264::Depacketiser, frames: &mut usize| {
        while let Some(o) = d.poll() {
            let t = match &o {
                H264Output::Frame(au) => {
                    check_access_unit(au, &limits);
                    *frames += 1;
                    au.timestamp
                }
                H264Output::Incomplete { timestamp, .. } => *timestamp,
            };
            // In timestamp order.
            assert!(last.is_none_or(|l| t >= l), "{t} after {last:?}");
            last = Some(t);
        }
    };
    for record in records(rest) {
        let Some((&flags, payload)) = record.split_first() else {
            continue;
        };
        if flags & 2 != 0 {
            ts += 3000;
        }
        if flags & 4 != 0 {
            d.lost();
        }
        let _ = d.push(ts, flags & 1 != 0, payload);
        check(&mut d, &mut frames);
    }
    d.reset();
    check(&mut d, &mut frames);
    assert!(!d.in_progress());
    frames
}

/// Checks one Opus packet's parse. Returns whether it is valid.
pub fn check_opus_packet(packet: &[u8]) -> bool {
    let Ok(info) = parse_packet(packet) else {
        return false;
    };
    assert!((1..=opus::MAX_FRAMES).contains(&info.frames));
    assert!(info.duration() <= opus::MAX_PACKET_SAMPLES);
    assert_eq!(info.toc.0, packet[0]);
    assert!(info.frame_bytes + info.padding < packet.len());
    assert!(info.frame_bytes <= usize::from(info.frames) * opus::MAX_FRAME_BYTES);
    if info.toc.code() == 0 {
        assert_eq!(info.frame_bytes, packet.len() - 1);
    }
    true
}

/// The Opus TOC parser, then Opus depacketising. Returns the number of
/// frames out, plus one if the whole input is a valid Opus packet.
pub fn opus_depacketise(data: &[u8]) -> usize {
    let valid = usize::from(check_opus_packet(data));
    let Some((&mode, rest)) = data.split_first() else {
        return valid;
    };
    let limits = OpusLimits {
        max_fill: if mode & 1 != 0 { 4800 } else { 96_000 },
    };
    let mut d = opus::Depacketiser::new(limits);
    let mut ts = 0u32;
    let mut frames = 0usize;
    // Where the timeline should continue, and whether loss came first.
    let mut end: Option<u32> = None;
    for record in records(rest) {
        let Some((&[flags, hi, lo], payload)) = record.split_first_chunk::<3>() else {
            continue;
        };
        let step = u32::from(u16::from_be_bytes([hi, lo]));
        ts = ts.wrapping_add(if flags & 2 != 0 { step } else { step * 120 });
        if flags & 4 != 0 {
            ts = ts.wrapping_add(1 << 31);
        }
        let lost = flags & 1 != 0;
        if lost {
            d.lost();
        }
        let valid_packet = check_opus_packet(payload);
        assert_eq!(d.push(ts, payload).is_ok(), valid_packet);
        let mut fills = 0u32;
        let mut seen_media = false;
        while let Some(o) = d.poll() {
            match o {
                OpusOutput::Frame(f) => {
                    let info = parse_packet(&f.data).expect("frames parse");
                    assert_eq!(info.duration(), f.duration);
                    assert_eq!(info.toc.stereo(), f.stereo);
                    if let Some(e) = end {
                        let off = f.timestamp.wrapping_sub(e);
                        // Only a gap under 2.5 ms is left unfilled.
                        assert!(off < opus::FILL_GRANULE, "hole of {off}");
                        if f.kind != FrameKind::Media {
                            assert_eq!(off, 0);
                        }
                    }
                    end = Some(f.timestamp.wrapping_add(f.duration));
                    match f.kind {
                        FrameKind::Media => {
                            assert!(!seen_media);
                            seen_media = true;
                            assert_eq!((f.data.as_slice(), f.timestamp), (payload, ts));
                        }
                        FrameKind::Dtx | FrameKind::Loss => {
                            assert!(!seen_media, "fill comes first");
                            fills += f.duration;
                            assert!(fills <= limits.max_fill);
                        }
                    }
                    frames += 1;
                }
                OpusOutput::Discontinuity { timestamp, .. } => {
                    assert_eq!(timestamp, ts);
                    end = None;
                }
            }
        }
        assert_eq!(seen_media, valid_packet);
    }
    frames + valid
}

/// One stream's bounds, for the receiver target.
fn receiver_config(mode: u8) -> ReceiverConfig {
    let mut cfg = ReceiverConfig::default();
    if mode & 1 != 0 {
        cfg.max_packets = 16;
        cfg.max_bytes = 4000;
    }
    if mode & 2 != 0 {
        cfg.jitter = Duration::from_millis(50);
        cfg.nack_burst = 64;
        cfg.nack_bitrate = 4000;
    }
    if mode & 8 != 0 {
        cfg.h264 = small_h264();
    }
    cfg
}

const VIDEO_SSRC: u32 = 0x1111_1111;
const RTX_SSRC: u32 = 0x2222_2222;
const AUDIO_SSRC: u32 = 0x3333_3333;

/// The receiver as the server drives it. A mode byte (bit 0: a 16-packet,
/// 4000-byte buffer; bit 1: a 50 ms jitter wait and a small NACK
/// allowance; bit 2: FIR instead of PLI and SSRCs bound from the first
/// packets; bit 3: small H.264 limits), then records whose first byte
/// picks an operation (modulo 6):
///
/// - 0 video and 1 audio: `[op, seq high, seq low, flags, payload]`. Flag
///   bit 0 is the marker; bit 1 advances the timestamp (3000 for video,
///   960 for audio).
/// - 2 RTX: `[op, OSN high, OSN low, flags, payload]`, at the current
///   video timestamp. An empty payload after the flags sends padding only.
/// - 3: the clock moves on `[op, ms high, ms low]` milliseconds, then
///   timers run.
/// - 4: the clock reads `[op, ms]` milliseconds earlier (a backwards
///   clock).
/// - 5: a packet for no stream.
///
/// Returns the number of events and feedback packets produced.
pub fn reorder_nack(data: &[u8]) -> usize {
    let Some((&mode, rest)) = data.split_first() else {
        return 0;
    };
    let cfg = receiver_config(mode);
    let start = Instant::now();
    let mut r = Receiver::new(cfg, start);
    let bind = mode & 4 == 0;
    let mut video = StreamConfig::new(Codec::H264, 96);
    video.ssrc = bind.then_some(VIDEO_SSRC);
    video.rtx = Some(RtxConfig {
        payload_type: 97,
        ssrc: bind.then_some(RTX_SSRC),
    });
    video.nack = true;
    video.pli = bind;
    video.fir = !bind;
    let mut audio = StreamConfig::new(Codec::Opus, 111);
    audio.ssrc = bind.then_some(AUDIO_SSRC);
    audio.nack = true;
    let v = r.add_stream(video).expect("video");
    let a = r.add_stream(audio).expect("audio");
    let mut now = 0u64;
    let mut clock = 0u64;
    let (mut vts, mut ats, mut rtx_seq) = (0u32, 0u32, 0u16);
    let mut produced = 0usize;
    // The last Lost run per stream, to check that losses only move on.
    let mut last_lost: [Option<u16>; 2] = [None, None];
    for record in records(rest) {
        let Some((&op, args)) = record.split_first() else {
            continue;
        };
        let at = |ms: u64| start + Duration::from_millis(ms);
        match op % 6 {
            kind @ (0 | 1 | 2 | 5) => {
                let Some((&[hi, lo, flags], payload)) = args.split_first_chunk::<3>() else {
                    continue;
                };
                let seq = u16::from_be_bytes([hi, lo]);
                let advance = flags & 2 != 0;
                let packet = match kind {
                    0 => {
                        if advance {
                            vts = vts.wrapping_add(3000);
                        }
                        (96, VIDEO_SSRC, seq, vts, payload.to_vec())
                    }
                    1 => {
                        if advance {
                            ats = ats.wrapping_add(960);
                        }
                        (111, AUDIO_SSRC, seq, ats, payload.to_vec())
                    }
                    2 => {
                        rtx_seq = rtx_seq.wrapping_add(1);
                        let body = if payload.is_empty() {
                            Vec::new()
                        } else {
                            [&seq.to_be_bytes()[..], payload].concat()
                        };
                        (97, RTX_SSRC, rtx_seq, vts, body)
                    }
                    _ => (100, 0x4444_4444, seq, 0, payload.to_vec()),
                };
                let built = RtpBuilder {
                    marker: flags & 1 != 0,
                    payload_type: packet.0,
                    sequence: packet.2,
                    timestamp: packet.3,
                    ssrc: packet.1,
                    payload: packet.4,
                    ..RtpBuilder::default()
                }
                .build();
                let parsed = RtpPacket::parse(&built).expect("built packets parse");
                let result = r.handle_rtp(at(clock), &parsed);
                if kind == 5 {
                    assert!(result.is_err(), "no stream takes it");
                }
            }
            3 => {
                let Some(&[hi, lo]) = args.first_chunk::<2>() else {
                    continue;
                };
                clock += u64::from(u16::from_be_bytes([hi, lo]));
                now = now.max(clock);
                r.handle_timeout(at(clock));
                // Run timers as the server would, up to the clock.
                for _ in 0..64 {
                    let Some(next) = r.poll_timeout() else {
                        break;
                    };
                    if next > at(now) {
                        break;
                    }
                    r.handle_timeout(next);
                    if let Some(after) = r.poll_timeout() {
                        assert!(after > next, "the next timer is later");
                    }
                    produced += drain(&mut r, [v, a], &cfg.h264, &mut last_lost);
                }
            }
            _ => {
                let back = u64::from(args.first().copied().unwrap_or(0));
                clock = clock.saturating_sub(back);
                r.handle_timeout(at(clock));
            }
        }
        now = now.max(clock);
        produced += drain(&mut r, [v, a], &cfg.h264, &mut last_lost);
        for s in [v, a] {
            let (packets, bytes) = r.buffered(s).expect("stream");
            assert!(packets <= cfg.max_packets && bytes <= cfg.max_bytes);
        }
    }
    produced
}

fn drain(
    r: &mut Receiver,
    streams: [StreamId; 2],
    limits: &H264Limits,
    last_lost: &mut [Option<u16>; 2],
) -> usize {
    let mut n = 0;
    while let Some(f) = r.poll_feedback() {
        n += 1;
        let mut wire = Vec::new();
        f.write(&mut wire, 1).expect("feedback builds");
        let compound = Compound::parse(&wire).expect("feedback parses");
        let parsed = compound.packets().next().expect("a packet").expect("valid");
        let ssrc = f.media_ssrc();
        let stream = streams
            .into_iter()
            .find(|&s| r.ssrc(s) == Some(ssrc))
            .expect("feedback about a stream");
        match (&f, parsed) {
            (Feedback::Nack { lost, .. }, Packet::Nack(n)) => {
                assert_eq!(n.lost().collect::<Vec<_>>(), *lost);
                for &seq in lost {
                    assert!(
                        r.is_missing(stream, seq),
                        "NACK for {seq}, which is not missing"
                    );
                }
            }
            (Feedback::Pli { .. }, Packet::Pli(_)) | (Feedback::Fir { .. }, Packet::Fir(_)) => {
                assert_eq!(stream, streams[0], "keyframes for video only");
            }
            other => panic!("feedback {other:?}"),
        }
    }
    while let Some(e) = r.poll_event() {
        n += 1;
        match e {
            Event::AccessUnit { au, .. } => check_access_unit(&au, limits),
            Event::Audio { frame, .. } => {
                let info = parse_packet(&frame.data).expect("audio parses");
                assert_eq!(info.duration(), frame.duration);
            }
            Event::Lost {
                stream,
                first,
                count,
            } => {
                assert!(count > 0);
                let slot = &mut last_lost[stream.index()];
                if let Some(prev_end) = *slot {
                    // Losses move forward, in 16-bit terms within half
                    // the space.
                    let step = first.wrapping_sub(prev_end);
                    assert!(step < 0x8000, "loss at {first} after {prev_end}");
                }
                *slot = Some(first.wrapping_add(count as u16));
            }
            Event::Discontinuity { stream, .. } => last_lost[stream.index()] = None,
            Event::IncompleteFrame { .. } | Event::FrameSkipped { .. } | Event::Refused { .. } => {}
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::super::push_record;
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
                assert!(
                    bytes.len() <= 65_536,
                    "{} within CI's -max_len",
                    p.display()
                );
                (p, bytes)
            })
            .collect()
    }

    /// Every seed gets past the length checks to output: a frame, a NACK
    /// or a valid packet.
    #[test]
    fn h264_seeds_make_frames() {
        for (path, bytes) in seeds("h264_depacketise") {
            assert!(h264_depacketise(&bytes) > 0, "{}", path.display());
        }
    }

    #[test]
    fn opus_seeds_make_frames() {
        for (path, bytes) in seeds("opus_depacketise") {
            assert!(opus_depacketise(&bytes) > 0, "{}", path.display());
        }
    }

    #[test]
    fn reorder_seeds_make_output() {
        for (path, bytes) in seeds("reorder_nack") {
            assert!(reorder_nack(&bytes) > 0, "{}", path.display());
        }
    }

    #[test]
    fn empty_and_junk_inputs() {
        assert_eq!(h264_depacketise(&[]), 0);
        assert_eq!(opus_depacketise(&[]), 0);
        assert_eq!(reorder_nack(&[]), 0);
        for junk in [
            &[0xffu8; 40][..],
            &[0, 0, 3, 0x7c, 0x85, 0x01][..],
            &[1, 0, 1][..],
        ] {
            h264_depacketise(junk);
            opus_depacketise(junk);
            reorder_nack(junk);
        }
    }

    /// Reads a fixture: RTP datagrams in `[channel, u16 length, bytes]`.
    fn fixture(name: &str) -> Vec<Vec<u8>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let bytes = std::fs::read(path).expect("fixture");
        let mut out = Vec::new();
        let mut rest = &bytes[..];
        while let Some((_, tail)) = rest.split_first() {
            let len = usize::from(u16::from_be_bytes([tail[0], tail[1]]));
            out.push(tail[2..2 + len].to_vec());
            rest = &tail[2 + len..];
        }
        out
    }

    /// Writes the committed media seeds from the committed fixtures:
    /// `cargo test -p sapflow-rtp --lib -- --ignored write_media_seeds`.
    #[test]
    #[ignore = "writes fuzz/seeds from tests/fixtures"]
    fn write_media_seeds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
        let write = |target: &str, name: &str, bytes: &[u8]| {
            let d = root.join(target);
            std::fs::create_dir_all(&d).expect("dir");
            assert!(bytes.len() <= 65_536, "{name} within CI's -max_len");
            std::fs::write(d.join(name), bytes).expect("write");
        };
        let video = fixture("ffmpeg-h264-fu.rec");
        let audio = fixture("ffmpeg-opus-stereo.rec");
        fn parsed(d: &[u8]) -> RtpPacket<'_> {
            RtpPacket::parse(d).expect("rtp")
        }

        // h264_depacketise: the capture whole, then with losses, then
        // built aggregation and refusals, under small limits.
        let h264_records = |mode: u8, count: usize, lose: &dyn Fn(usize) -> bool| {
            let mut out = vec![mode];
            let mut last_ts = None;
            let mut lost = false;
            for (n, d) in video.iter().take(count).enumerate() {
                let p = parsed(d);
                if lose(n) {
                    lost = true;
                    continue;
                }
                let mut flags = u8::from(p.marker());
                if last_ts != Some(p.timestamp()) {
                    flags |= 2;
                }
                if std::mem::take(&mut lost) {
                    flags |= 4;
                }
                last_ts = Some(p.timestamp());
                push_record(&mut out, &[&[flags][..], p.payload()].concat());
            }
            out
        };
        write(
            "h264_depacketise",
            "ffmpeg-h264-fu",
            &h264_records(0, usize::MAX, &|_| false),
        );
        write(
            "h264_depacketise",
            "ffmpeg-h264-fu-lossy",
            &h264_records(0, usize::MAX, &|n| n % 13 == 4 || n == 90),
        );
        write(
            "h264_depacketise",
            "ffmpeg-h264-fu-small-limits",
            &h264_records(1, 40, &|_| false),
        );
        let mut built = vec![0u8];
        let sps = [0x67, 0x42, 0xc0, 0x1e, 0xda];
        let pps = [0x68, 0xce, 0x3c, 0x80];
        let stap = |units: &[&[u8]]| {
            let mut p = vec![0x78];
            for u in units {
                p.extend_from_slice(&(u.len() as u16).to_be_bytes());
                p.extend_from_slice(u);
            }
            p
        };
        let slice = [0x41, 0x9a, 0x11, 0x22, 0x33];
        let idr = [0x65, 0x88, 0x84, 0x21];
        for (flags, payload) in [
            (2u8, stap(&[&[0x09, 0xf0], &sps, &pps, &idr])),
            (3, slice.to_vec()),
            (2, vec![0x7c, 0x85, 1, 2, 3]),
            (0, vec![0x7c, 0x05, 4, 5, 6]),
            (1, vec![0x7c, 0x45, 7, 8]),
            (3, vec![0x79, 0, 0, 0, 0]),
            (3, vec![0x7a, 0, 0, 0, 0]),
            (3, vec![0x7d, 0x85, 0]),
            (6, slice.to_vec()),
            (1, vec![0x1e]),
            (3, stap(&[&sps, &[0x78, 0, 1]])),
            (3, vec![0xc1, 0x9a]),
            (6, stap(&[&pps, &slice])),
            (1, slice.to_vec()),
            (2, slice.to_vec()),
            (3, idr.to_vec()),
        ] {
            push_record(&mut built, &[&[flags][..], &payload].concat());
        }
        write("h264_depacketise", "built-stap-fu-refusals", &built);

        // opus_depacketise: the capture with DTX, loss and a jump, then
        // packets of every code. Steps are in 2.5 ms units.
        let mut out = vec![0u8];
        let mut prev: Option<u32> = None;
        for (n, d) in audio.iter().enumerate() {
            let p = parsed(d);
            if (20..25).contains(&n) || n == 50 {
                continue;
            }
            let step = prev.map_or(0, |t| p.timestamp().wrapping_sub(t) / 120) as u16;
            prev = Some(p.timestamp());
            let flags = u8::from(n == 51);
            let mut rec = vec![flags];
            rec.extend_from_slice(&step.to_be_bytes());
            rec.extend_from_slice(p.payload());
            push_record(&mut out, &rec);
        }
        let mut jump = vec![4u8, 0, 8];
        jump.extend_from_slice(parsed(&audio[0]).payload());
        push_record(&mut out, &jump);
        write("opus_depacketise", "ffmpeg-opus-dtx-loss", &out);
        let mut codes = vec![1u8];
        for (flags, step, packet) in [
            (0u8, 0u16, vec![0x18, 1, 2]),
            (0, 24, vec![0xf9, 1, 2, 3, 4]),
            (1, 40, vec![0x0a, 2, 9, 9, 8]),
            (2, 7, vec![0xfb, 0x83, 1, 2, 7, 8, 8, 9, 9, 9]),
            (0, 30, vec![0xfb, 0x42, 2, 5, 5, 0, 0]),
            (0, 8, vec![0xfb, 0xc2, 1, 1, 4, 5, 6, 0]),
            (0, 8, vec![0xe3, 48]),
            (0, 48, vec![0xf9, 1, 2, 3]),
            (0, 20, vec![0x0a, 252, 1]),
            (0, 20, vec![0x78]),
        ] {
            let mut rec = vec![flags];
            rec.extend_from_slice(&step.to_be_bytes());
            rec.extend_from_slice(&packet);
            push_record(&mut codes, &rec);
        }
        write("opus_depacketise", "built-codes", &codes);
        // A raw packet, for the TOC parser on its own.
        let mut pad = vec![0xfb, 0xc3, 255, 3, 2, 1, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        pad.resize(pad.len() + 257, 0);
        write("opus_depacketise", "packet-code3-vbr-padded", &pad);

        // reorder_nack: the capture interleaved, with drops, reordering,
        // a duplicate, RTX answers, clock steps and a stranger.
        let media = |op: u8, d: &Vec<u8>, advance: bool| {
            let p = parsed(d);
            let mut rec = vec![op];
            rec.extend_from_slice(&p.sequence().to_be_bytes());
            rec.push(u8::from(p.marker()) | if advance { 2 } else { 0 });
            rec.extend_from_slice(p.payload());
            rec
        };
        let step = |ms: u16| [&[3u8][..], &ms.to_be_bytes()].concat();
        let mut ops = vec![0u8];
        let mut last_ts = None;
        let mut withheld = Vec::new();
        let mut a = audio.iter();
        for (n, d) in video.iter().take(100).enumerate() {
            let p = parsed(d);
            let advance = last_ts.is_some_and(|t| t != p.timestamp());
            if advance {
                push_record(&mut ops, &step(22));
                if let Some(ad) = a.next() {
                    push_record(&mut ops, &media(1, ad, true));
                }
                push_record(&mut ops, &step(22));
                if let Some(ad) = a.next() {
                    push_record(&mut ops, &media(1, ad, true));
                }
                push_record(&mut ops, &step(22));
                // Answer what was withheld over RTX.
                for (osn, payload, marker) in withheld.drain(..) {
                    let mut rec = vec![2u8];
                    rec.extend_from_slice(&u16::to_be_bytes(osn));
                    rec.push(marker);
                    rec.extend_from_slice(payload);
                    push_record(&mut ops, &rec);
                }
            }
            last_ts = Some(p.timestamp());
            if n % 9 == 4 {
                withheld.push((p.sequence(), p.payload(), u8::from(p.marker())));
                continue;
            }
            let rec = media(0, d, advance);
            push_record(&mut ops, &rec);
            if n % 31 == 7 {
                push_record(&mut ops, &rec);
            }
        }
        push_record(&mut ops, &step(300));
        push_record(&mut ops, &[4, 50]);
        push_record(&mut ops, &[5, 0, 1, 0, 0x41]);
        push_record(&mut ops, &[2, 0, 0, 0]);
        push_record(&mut ops, &step(2500));
        write("reorder_nack", "ffmpeg-interleaved-rtx", &ops);
        // Small buffers and short waits, SSRCs bound from the packets.
        ops[0] = 1 | 2 | 4;
        write("reorder_nack", "ffmpeg-interleaved-small-bound", &ops);
    }
}
