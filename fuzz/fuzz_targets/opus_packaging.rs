//! Opus packaging in sapflow-hls. On every input, its TOC and framing check
//! (`sapflow_hls::codec::opus_packet`) must agree with the receiver's
//! (`sapflow_rtp::opus::parse_packet`) on validity, duration, frame count
//! and stereo flag: packaging must never refuse a packet the receiver
//! passed on. With the `OPT1` prefix, records of packet shapes, timestamp
//! steps, gaps, steps back, malformed packets and interrupts go into an
//! audio-only or video-led Opus muxer; rendered playlists must pass
//! `playlist::RuleCheck` after every push, and a refused malformed packet
//! must leave the muxer able to carry on.
#![no_main]
use std::time::UNIX_EPOCH;

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use sapflow_hls::codec::{AudioConfig, H264Config, OpusConfig, opus_packet};
use sapflow_hls::muxer::MuxError;
use sapflow_hls::{Muxer, MuxerConfig, playlist};

const SPS: &[u8] = &[
    0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03, 0x00, 0x08,
    0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
];
const PPS: &[u8] = &[0x68, 0xef, 0xbc, 0x80];

fn parity(data: &[u8]) {
    let ours = opus_packet(data);
    let receiver = sapflow_rtp::opus::parse_packet(data);
    match (ours, receiver) {
        (Some(p), Ok(r)) => {
            assert_eq!(p.samples(), r.duration(), "duration");
            assert_eq!(p.frames, r.frames, "frame count");
            assert_eq!(p.stereo, r.toc.stereo(), "stereo flag");
        }
        (None, Err(_)) => {}
        (ours, receiver) => panic!("packaging {ours:?}, receiver {receiver:?}"),
    }
}

/// A packet with TOC byte `toc`, framed for its code: two-byte frames, and
/// `frames` of them for code 3 (zero or too many are malformed).
fn packet(toc: u8, frames: u8) -> Vec<u8> {
    match toc & 3 {
        0 => vec![toc, 0xff, 0xfe],
        1 => vec![toc, 0xff, 0xfe, 0xff, 0xfe],
        2 => vec![toc, 2, 0xff, 0xfe, 0xff, 0xfe],
        _ => {
            let count = frames % 50;
            let mut p = vec![toc, count];
            for _ in 0..count {
                p.extend([0xff, 0xfe]);
            }
            p
        }
    }
}

/// Head: bit 0 video-led, bit 1 stereo. Records of four bytes: TOC, code 3
/// frame count, a step in ticks, flags: bit 0 the step is jitter added to
/// the packet's duration, bit 1 the timestamp goes back by the step, bit 2
/// a gap of 1000 steps first, bit 3 interrupt after, bit 4 a truncated
/// (malformed) packet instead, bit 5 a keyframe at the next video frame.
fn timing(data: &[u8]) {
    let Some((&head, records)) = data.split_first() else {
        return;
    };
    let video_led = head & 1 != 0;
    let channels = if head & 2 != 0 {
        OpusConfig::STEREO
    } else {
        OpusConfig::MONO
    };
    let Some(video) = H264Config::new(SPS, PPS) else {
        return;
    };
    let mut m = Muxer::with_audio_config(
        MuxerConfig::default(),
        video_led.then_some(video),
        Some(AudioConfig::Opus(channels)),
        None,
    )
    .expect("supported Opus configuration");
    let mut rules = playlist::RuleCheck::default();
    let (mut pts, mut dts, mut frame) = (0i64, -3000i64, 0u32);
    let mut keyframe = true;
    for rec in records.chunks_exact(4).take(4096) {
        let (toc, frames, step, flags) = (rec[0], rec[1], i64::from(rec[2]), rec[3]);
        let mut p = packet(toc, frames);
        if flags & 16 != 0 {
            p.truncate(usize::from(frames) % p.len());
        }
        let duration = opus_packet(&p).map_or(0, |i| i64::from(i.samples()) * 90_000 / 48_000);
        if flags & 4 != 0 {
            pts = pts.saturating_add(step * 1000);
        }
        if video_led {
            keyframe |= flags & 32 != 0;
            while dts + 3000 <= pts {
                dts += 3000;
                let sync = keyframe || frame.is_multiple_of(60);
                keyframe = false;
                frame += 1;
                let pushed = m.push_video(dts, dts, sync, Bytes::from_static(&[0; 8]), UNIX_EPOCH);
                if pushed.is_err() {
                    return;
                }
            }
        }
        let result = m.push_audio_at(pts, Bytes::from(p), UNIX_EPOCH);
        pts = if flags & 2 != 0 {
            pts.saturating_sub(step)
        } else {
            pts.saturating_add(duration + if flags & 1 != 0 { step } else { 0 })
        };
        if flags & 8 != 0 {
            m.interrupt(UNIX_EPOCH);
        }
        if m.has_content() {
            let rendered = playlist::media(&m, "p");
            if let Err(error) = rules.check(&rendered) {
                panic!("{error}\n{rendered}");
            }
        }
        match result {
            Ok(_) | Err(MuxError::InvalidAudio) => {}
            Err(_) => return,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    parity(data);
    if let Some(records) = data.strip_prefix(b"OPT1") {
        timing(records);
    }
});
