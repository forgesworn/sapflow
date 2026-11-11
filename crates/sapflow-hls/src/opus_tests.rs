//! Opus beside AAC: init and `dOps`, TOC durations through parts and
//! segments, audio-only and video-led streams, the stereo rule, timestamp
//! anomalies, reconnects and the same admission bounds as AAC.
//! Packets are minimal valid Opus framings (RFC 6716 section 3), not
//! decodable media; `opus_real_tests` feeds real encoded packets.
use std::io::Cursor;

use mp4_atom::{Codec, Decode, Moof};

use super::*;
use crate::codec::{AccessUnit, OPUS_SAMPLE_RATE};
use crate::{EsIngest, IngestError, playlist};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;
const LIVE_LIMIT: usize = 16_384;

/// One packet of `samples` at 48 kHz: one CELT fullband frame for 2.5 to
/// 20 ms, one SILK wideband frame for 40 and 60 ms, four or six 20 ms CELT
/// frames (code 3) for 80 and 120 ms. Each frame is the two-byte silent CELT frame.
pub(crate) fn packet(samples: u32, stereo: bool) -> Vec<u8> {
    let s = if stereo { 0x04 } else { 0 };
    let celt = |config: u8| vec![config << 3 | s, 0xff, 0xfe];
    match samples {
        120 => celt(28),
        240 => celt(29),
        480 => celt(30),
        960 => celt(31),
        1920 => vec![10 << 3 | s, 1, 2],
        2880 => vec![11 << 3 | s, 1, 2],
        3840 | 5760 => {
            let frames = (samples / 960) as u8;
            let mut p = vec![31 << 3 | s | 3, frames];
            for _ in 0..frames {
                p.extend([0xff, 0xfe]);
            }
            p
        }
        _ => panic!("no test packet of {samples} samples"),
    }
}

fn ticks(samples: u32) -> i64 {
    i64::from(samples) * 90_000 / i64::from(OPUS_SAMPLE_RATE)
}

fn opus(channels: u8) -> AudioConfig {
    AudioConfig::Opus(OpusConfig { channels })
}

fn audio_only(cfg: MuxerConfig, channels: u8) -> Muxer {
    Muxer::with_audio_config(cfg, None, Some(opus(channels)), None).expect("Opus-only muxer")
}

fn moov(init: &Bytes) -> mp4_atom::Moov {
    let mut input = Cursor::new(init.as_ref());
    mp4_atom::Ftyp::decode(&mut input).expect("ftyp");
    mp4_atom::Moov::decode(&mut input).expect("moov")
}

fn audio_trak(init: &Bytes) -> mp4_atom::Trak {
    moov(init)
        .trak
        .into_iter()
        .find(|t| t.tkhd.track_id == fmp4::AUDIO_TRACK)
        .expect("audio trak")
}

/// The audio run of a part: its base decode time and sample durations.
fn audio_run(part: &Part) -> Option<(u64, Vec<u32>)> {
    let moof = Moof::decode(&mut Cursor::new(&part.data[..])).expect("moof");
    let traf = moof
        .traf
        .iter()
        .find(|t| t.tfhd.track_id == fmp4::AUDIO_TRACK)?;
    Some((
        traf.tfdt.as_ref().expect("tfdt").base_media_decode_time,
        traf.trun
            .iter()
            .flat_map(|r| &r.entries)
            .map(|e| e.duration.expect("duration"))
            .collect(),
    ))
}

fn all_parts(m: &Muxer) -> Vec<Arc<Part>> {
    (0..m.next_part_id())
        .filter_map(|id| match m.part(id) {
            PartLookup::Found(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// Every audio sample duration in the window, checking that each part's
/// audio starts where the last one ended.
fn audio_durations(m: &Muxer) -> Vec<u32> {
    let mut out = Vec::new();
    let mut end = None;
    for part in all_parts(m) {
        if let Some((base, durations)) = audio_run(&part) {
            if let Some(end) = end {
                assert!(base >= end, "audio in part {} goes back", part.id);
            }
            end = Some(base + durations.iter().map(|&d| u64::from(d)).sum::<u64>());
            out.extend(durations);
        }
    }
    out
}

fn segments(m: &Muxer) -> Vec<Arc<Segment>> {
    m.entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Segment(s) => Some(s.clone()),
            Entry::Gap { .. } => None,
        })
        .collect()
}

/// The part rules on every segment, as RuleCheck holds the playlist to: an
/// independent part is exempt from the 85% floor (RFC 8216bis 4.4.4.9).
fn assert_part_rules(m: &Muxer) {
    for segment in segments(m) {
        assert!(segment.duration.as_secs_f64().round() <= m.target_duration() as f64);
        for (i, part) in segment.parts.iter().enumerate() {
            assert!(part.duration <= m.part_target(), "part {}", part.id);
            if i + 1 < segment.parts.len() && !part.independent {
                assert!(
                    part.duration.as_secs_f64() >= m.part_target().as_secs_f64() * 0.85,
                    "part {} is short but not last",
                    part.id
                );
            }
        }
    }
}

fn check(rules: &mut playlist::RuleCheck, m: &Muxer) {
    if m.has_content() {
        let pl = playlist::media(m, "p");
        if let Err(e) = rules.check(&pl) {
            panic!("{e}\n{pl}");
        }
    }
}

#[test]
fn opus_init_has_an_opus_sample_entry_and_dops_set_by_rule() {
    for channels in [1u8, 2] {
        for video in [None, Some(tests::video())] {
            let m = Muxer::with_audio_config(
                MuxerConfig::default(),
                video.clone(),
                Some(opus(channels)),
                None,
            )
            .expect("Opus muxer");
            assert_eq!(m.audio_config(), Some(&opus(channels)));
            assert_eq!(m.audio(), None, "the AAC accessor stays AAC");
            assert!(m.tracks().has_audio());
            let trak = audio_trak(m.init());
            assert_eq!(trak.mdia.mdhd.timescale, 48_000);
            assert_eq!(trak.mdia.hdlr.handler, mp4_atom::FourCC::new(b"soun"));
            let [Codec::Opus(entry)] = trak.mdia.minf.stbl.stsd.codecs.as_slice() else {
                panic!("one Opus sample entry");
            };
            assert_eq!(entry.audio.channel_count, u16::from(channels));
            assert_eq!(entry.audio.sample_size, 16);
            assert_eq!(entry.audio.sample_rate.integer(), 48_000);
            assert_eq!(
                entry.dops,
                mp4_atom::Dops {
                    output_channel_count: channels,
                    pre_skip: 0,
                    input_sample_rate: 48_000,
                    output_gain: 0,
                }
            );
            // The box itself, big-endian (encapsulation 4.3.2): size 19,
            // version 0, channels, pre-skip 0, 48000, gain 0, family 0.
            let at = m
                .init()
                .windows(4)
                .position(|w| w == b"dOps")
                .expect("dOps box");
            assert_eq!(
                &m.init()[at - 4..at + 15],
                &[
                    0, 0, 0, 19, b'd', b'O', b'p', b's', 0, channels, 0, 0, 0, 0, 0xbb, 0x80, 0, 0,
                    0
                ]
            );
        }
    }
    for bad in [0u8, 3] {
        assert!(matches!(
            Muxer::with_audio_config(MuxerConfig::default(), None, Some(opus(bad)), None),
            Err(MuxError::InvalidAudio)
        ));
        assert!(matches!(
            Muxer::with_audio_config(
                MuxerConfig::default(),
                Some(tests::video()),
                Some(opus(bad)),
                None
            ),
            Err(MuxError::InvalidAudio)
        ));
        assert_eq!(
            fmp4::init_segment_for_audio_tracks(None, Some(&opus(bad)), Duration::ZERO),
            None
        );
    }
    assert!(matches!(
        Muxer::with_audio_config(MuxerConfig::default(), None, None, None),
        Err(MuxError::InvalidTracks)
    ));
    // The recording header is the streaming one plus its duration.
    let file = fmp4::init_segment_for_audio_tracks(
        Some(&tests::video()),
        Some(&opus(2)),
        Duration::from_secs(9),
    )
    .expect("recording init");
    assert_eq!(moov(&file).mvhd.duration, 9000);
    assert!(matches!(
        audio_trak(&file).mdia.minf.stbl.stsd.codecs.as_slice(),
        [Codec::Opus(_)]
    ));
}

#[test]
fn audio_only_opus_cuts_independent_parts_on_toc_durations() {
    let cfg = MuxerConfig {
        segment_count: 1000,
        ..MuxerConfig::default()
    };
    let mut ingest = EsIngest::new(cfg);
    ingest
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("declaration");
    let mut rules = playlist::RuleCheck::default();
    let n = 500; // ten seconds of 20 ms packets
    for i in 0..n {
        ingest
            .opus_at(i * 1800, &packet(960, true), NOW)
            .expect("Opus packet");
        check(&mut rules, ingest.muxer().expect("muxer"));
    }
    let m = ingest.muxer_mut().expect("muxer");
    m.interrupt(NOW);
    assert_eq!(m.tracks(), TrackSet::AudioOnly);
    assert_eq!(m.audio_config(), Some(&opus(2)), "first TOC is stereo");
    // Twenty-millisecond packets give the same part target as five-frame
    // multiples at 30 fps: ten packets, 200 ms, under a 234 ms target.
    assert_eq!(m.part_target(), Duration::from_millis(234));
    assert_eq!(m.target_duration(), 2);
    assert_part_rules(m);
    let durations = audio_durations(m);
    assert_eq!(durations.len(), n as usize);
    assert!(durations.iter().all(|&d| d == 960));
    for segment in segments(m) {
        assert!(segment.parts.iter().all(|p| p.independent));
    }
    assert!(segments(m).len() >= 9);
    assert_eq!(
        playlist::multivariant(m)
            .lines()
            .find(|l| l.contains("CODECS"))
            .map(|l| l.contains("CODECS=\"Opus\"")),
        Some(true)
    );
}

/// Watches a live stream push by push, as a client would: RuleCheck on
/// every render, every part as it is produced (so nothing is lost to the
/// window), and RFC 8216bis 6.2.2's part retention: a part stays
/// downloadable for three target durations after its tag leaves the
/// playlist.
struct Watch {
    rules: playlist::RuleCheck,
    parts: Vec<Arc<Part>>,
    next: u64,
    listed: std::collections::BTreeSet<u64>,
    removed: std::collections::BTreeMap<u64, i64>,
    gone: std::collections::BTreeMap<u64, i64>,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            rules: playlist::RuleCheck::with_live_window(),
            parts: Vec::new(),
            next: 0,
            listed: Default::default(),
            removed: Default::default(),
            gone: Default::default(),
        }
    }
}

impl Watch {
    fn observe(&mut self, m: &Muxer, now: i64) {
        while self.next < m.next_part_id() {
            if let PartLookup::Found(p) = m.part(self.next) {
                self.parts.push(p);
            }
            self.next += 1;
        }
        if !m.has_content() {
            return;
        }
        let pl = playlist::media(m, "p");
        if let Err(e) = self.rules.check(&pl) {
            panic!("{e}\n{pl}");
        }
        let listed: std::collections::BTreeSet<u64> = pl
            .lines()
            .filter_map(|l| l.split("URI=\"p_part").nth(1))
            .filter_map(|r| r.split('.').next()?.parse().ok())
            .collect();
        for id in self.listed.difference(&listed) {
            self.removed.entry(*id).or_insert(now);
        }
        for id in self.removed.keys() {
            if !self.gone.contains_key(id) && matches!(m.part(*id), PartLookup::Missing) {
                self.gone.insert(*id, now);
            }
        }
        self.listed = listed;
    }

    fn check_retention(&self, m: &Muxer) {
        let floor = 3 * m.target_duration() as i64 * 90_000;
        for (id, gone) in &self.gone {
            let held = gone - self.removed[id];
            assert!(
                held >= floor,
                "part {id} gone {held} ticks after its tag left"
            );
        }
    }

    fn audio(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for part in &self.parts {
            out.extend(audio_run(part).map(|(_, d)| d).unwrap_or_default());
        }
        out
    }
}

#[test]
fn audio_only_opus_keeps_the_part_rules_when_packet_durations_change() {
    for first in [960u32, 2880, 120, 5760] {
        let mut m = audio_only(MuxerConfig::default(), 1);
        let mut watch = Watch::default();
        let mut pushed = Vec::new();
        let mut pts = 0i64;
        let plan = [
            (first, 30),
            (960, 50),
            (2880, 20),
            (5760, 12),
            (480, 40),
            (120, 100),
            (1920, 30),
            (240, 50),
            (5760, 3),
            (3840, 10),
            (960, 60),
        ];
        for (samples, count) in plan {
            for _ in 0..count {
                let up = m
                    .push_audio_at(pts, Bytes::from(packet(samples, false)), NOW)
                    .expect("valid packet");
                assert!(!up.media_loss);
                pushed.push(samples);
                pts += ticks(samples);
                watch.observe(&m, pts);
            }
        }
        m.interrupt(NOW);
        watch.observe(&m, pts);
        watch.check_retention(&m);
        assert_part_rules(&m);
        assert_eq!(watch.audio(), pushed, "first packet {first}");
        assert_eq!(m.breaks(), 1, "only the final interrupt");
        assert_eq!(
            m.part_target(),
            PartTiming::new(Duration::from_millis(200), &[ticks(first) as u32]).target_duration(),
            "the part target follows the first packet"
        );
        // Every segment but the last reaches segment_min: a packet that does
        // not fit closes only its part (independent, so exempt from 85%).
        let all = segments(&m);
        assert!(
            all[..all.len() - 1]
                .iter()
                .all(|s| s.duration >= Duration::from_secs(1))
        );
        if first == 960 {
            let short = watch
                .parts
                .iter()
                .any(|p| p.duration.as_secs_f64() < m.part_target().as_secs_f64() * 0.85);
            assert!(short, "early part close exercised");
        }
    }
}

/// The review's probe P1: a 20 ms first packet fixes a 234 ms part target,
/// then 60, 80 or 120 ms packets for a minute, default configuration.
/// Closing the segment with each early part collapsed the window to
/// single-part segments (2.88 s at 120 ms, against RFC 8216bis 6.2.2's
/// three target durations).
#[test]
fn longer_packets_after_a_20ms_first_packet_keep_the_live_window() {
    for samples in [2880u32, 3840, 5760] {
        let mut m = audio_only(MuxerConfig::default(), 2);
        let mut watch = Watch::default();
        let mut pts = 0i64;
        let mut pushes = 0;
        m.push_audio_at(pts, Bytes::from(packet(960, true)), NOW)
            .expect("first packet");
        pts += ticks(960);
        pushes += 1;
        while pts < 60 * 90_000 {
            m.push_audio_at(pts, Bytes::from(packet(samples, true)), NOW)
                .expect("valid packet");
            pts += ticks(samples);
            pushes += 1;
            watch.observe(&m, pts);
        }
        watch.check_retention(&m);
        assert_eq!(m.part_target(), Duration::from_millis(234));
        let window: Duration = m.entries().iter().map(Entry::duration).sum();
        let all = segments(&m);
        let mean = all.iter().map(|s| s.duration).sum::<Duration>() / all.len() as u32;
        println!(
            "P1 {} ms: segment {:.3} s, window {:.3} s, {} parts for {pushes} pushes",
            samples / 48,
            mean.as_secs_f64(),
            window.as_secs_f64(),
            watch.parts.len()
        );
        assert!(
            window >= Duration::from_secs(3 * m.target_duration()),
            "{} ms packets: window {window:?}",
            samples / 48
        );
        assert!(mean >= Duration::from_secs(1), "{mean:?}");
    }
}

/// The review's minor 6: a part_min of 100 ms gave a 116 ms part target,
/// and then a valid 120 ms packet was refused. The Opus part target is
/// never under 120 ms, so every valid packet fits a part.
#[test]
fn a_short_part_min_never_makes_a_valid_packet_fatal() {
    for (part_min, first) in [(100, 960u32), (0, 120), (50, 480), (150, 2880)] {
        let mut m = audio_only(
            MuxerConfig {
                part_min: Duration::from_millis(part_min),
                ..MuxerConfig::default()
            },
            2,
        );
        let mut watch = Watch::default();
        let mut pts = 0;
        for samples in [first, 5760, 120, 5760, 5760, 960, 3840, 5760] {
            for _ in 0..20 {
                m.push_audio_at(pts, Bytes::from(packet(samples, true)), NOW)
                    .expect("every valid packet is taken");
                pts += ticks(samples);
                watch.observe(&m, pts);
            }
        }
        assert!(m.part_target() >= Duration::from_millis(120), "{part_min}");
    }
}

/// A part target inherited from an earlier epoch cannot change, so one under
/// 120 ms refuses the audio-only Opus epoch up front, before any change.
#[test]
fn an_inherited_part_target_under_120ms_refuses_audio_only_opus() {
    let cfg = MuxerConfig {
        part_min: Duration::ZERO,
        ..MuxerConfig::default()
    };
    let mut m = Muxer::new(cfg, tests::video(), None);
    for frame in 0..=60i64 {
        m.push_video(
            frame * 1500,
            frame * 1500,
            frame % 30 == 0,
            Bytes::from_static(&[1]),
            NOW,
        )
        .expect("60 fps video");
    }
    assert!(m.part_target() < Duration::from_millis(120));
    let epoch = m.current_initialization_id();
    assert!(matches!(
        m.reconfigure_with_audio(None, Some(opus(2)), NOW),
        Err(MuxError::InvalidAudio)
    ));
    assert_eq!(m.current_initialization_id(), epoch);
    assert!(m.video().is_some());
    // Opus beside the video is unaffected: video cuts its parts.
    assert!(
        m.reconfigure_with_audio(Some(tests::video()), Some(opus(2)), NOW)
            .is_ok()
    );
}

/// What a viewer or recorder could observe of a muxer.
fn observable(m: &Muxer) -> (u64, u64, usize, usize, u64, String) {
    (
        m.next_part_id(),
        m.media_sequence(),
        m.entries().len(),
        m.open_parts().len(),
        m.breaks(),
        if m.has_content() {
            playlist::media(m, "p")
        } else {
            String::new()
        },
    )
}

/// The review's minor 7: a packet that would close the open part early was
/// refused only after the part (and then the segment) had been rotated.
#[test]
fn a_refused_opus_packet_changes_nothing_even_where_it_would_close_a_part() {
    let mut m = audio_only(MuxerConfig::default(), 2);
    let mut pts = 0;
    for _ in 0..60 {
        m.push_audio_at(pts, Bytes::from(packet(960, true)), NOW)
            .expect("20 ms");
        pts += ticks(960);
    }
    for _ in 0..3 {
        m.push_audio_at(pts, Bytes::from(packet(2880, true)), NOW)
            .expect("60 ms");
        pts += ticks(2880);
    }
    // The open part holds 180 ms of a 234 ms target: a 120 ms packet would
    // close it. Refused for size, it must change nothing.
    assert!(m.part.as_ref().is_some_and(|p| !p.audio.is_empty()));
    m.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES;
    let before = (observable(&m), state(&m), m.audio_end);
    let up = m.push_audio_at(pts, Bytes::from(packet(5760, true)), NOW);
    assert!(matches!(up, Err(MuxError::SegmentTooLarge)), "{up:?}");
    assert_eq!(before, (observable(&m), state(&m), m.audio_end));
    // A discontinuous timestamp refused for a malformed packet changes
    // nothing either.
    let before = (observable(&m), state(&m), m.audio_end);
    assert!(
        m.push_audio_at(pts + 90_000, Bytes::from_static(&[0xfb, 0]), NOW)
            .is_err()
    );
    assert_eq!(before, (observable(&m), state(&m), m.audio_end));
}

/// The W3b review: copying a borrowed packet before the checks passed every
/// test. A refused packet, or one the muxer would only drop, is never
/// copied; an admitted one is copied once.
#[test]
fn a_refused_or_dropped_borrowed_opus_packet_is_never_copied() {
    let copies = std::cell::Cell::new(0);
    let copy = |p: &[u8]| {
        copies.set(copies.get() + 1);
        Bytes::copy_from_slice(p)
    };
    let good = packet(960, true);
    let malformed: &[u8] = &[0xfb, 0];
    // Audio-only.
    let mut m = audio_only(MuxerConfig::default(), 2);
    m.push_opus_slice_at_with(0, &good, NOW, copy)
        .expect("admitted");
    assert_eq!(copies.get(), 1);
    for (pts, p) in [(1800, malformed), (1800, &[][..]), (-1, &good[..])] {
        assert!(m.push_opus_slice_at_with(pts, p, NOW, copy).is_err());
    }
    m.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES;
    assert!(matches!(
        m.push_opus_slice_at_with(1800, &good, NOW, copy),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(copies.get(), 1, "audio-only refusals copied nothing");
    // Opus into an AAC muxer.
    let mut aac = Muxer::audio_only(MuxerConfig::default(), tests::aac()).expect("AAC");
    assert!(matches!(
        aac.push_opus_slice_at_with(0, &good, NOW, copy),
        Err(MuxError::InvalidAudio)
    ));
    // Video-led, through both entry points.
    let mut m = av_muxer();
    m.push_opus_slice_with(0, &good, copy).expect("admitted");
    m.push_opus_slice_at_with(1800, &good, NOW, copy)
        .expect("admitted");
    assert_eq!(copies.get(), 3);
    assert!(m.push_opus_slice_with(3600, malformed, copy).is_err());
    // Dropped, not refused: before the stream's start.
    m.push_opus_slice_with(-1800, &good, copy)
        .expect("dropped quietly");
    m.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES;
    assert!(matches!(
        m.push_opus_slice_with(3600, &good, copy),
        Err(MuxError::SegmentTooLarge)
    ));
    // A video-only muxer drops Opus without copying it.
    let mut video_only = Muxer::new(MuxerConfig::default(), tests::video(), None);
    video_only
        .push_video(0, 0, true, Bytes::from_static(&[1]), NOW)
        .expect("IDR");
    video_only
        .push_opus_slice_with(0, &good, copy)
        .expect("dropped quietly");
    assert_eq!(
        copies.get(),
        3,
        "video-led refusals and drops copied nothing"
    );
}

#[test]
fn malformed_opus_is_refused_before_any_state_changes() {
    let bad: [&[u8]; 4] = [&[], &[0xf9, 1, 2, 3], &[0xfb, 0], &[0xfb, 49, 0]];
    // Audio-only.
    let mut m = audio_only(MuxerConfig::default(), 2);
    m.push_audio_at(0, Bytes::from(packet(960, true)), NOW)
        .expect("valid");
    for p in bad {
        let before = (m.part.as_ref().map(|p| p.audio.len()), m.audio_end);
        assert!(
            m.push_audio_at(1800, Bytes::copy_from_slice(p), NOW)
                .is_err()
        );
        assert_eq!(
            before,
            (m.part.as_ref().map(|p| p.audio.len()), m.audio_end)
        );
    }
    // Video-led.
    let mut m = Muxer::with_audio_config(
        MuxerConfig::default(),
        Some(tests::video()),
        Some(opus(2)),
        None,
    )
    .expect("A/V");
    m.push_video(0, 0, true, Bytes::from_static(&[1]), NOW)
        .expect("IDR");
    m.push_video(3000, 3000, false, Bytes::from_static(&[1]), NOW)
        .expect("successor");
    m.push_audio(0, Bytes::from(packet(960, true)))
        .expect("valid");
    for p in &bad[1..] {
        let pending = m.pending_audio.as_ref().map(|p| p.dts);
        assert!(matches!(
            m.push_audio(1800, Bytes::copy_from_slice(p)),
            Err(MuxError::InvalidAudio)
        ));
        assert_eq!(pending, m.pending_audio.as_ref().map(|p| p.dts));
    }
    // Through ingest, every path and codec mix.
    let mut ingest = EsIngest::new(MuxerConfig::default());
    for p in bad {
        assert_eq!(ingest.opus_at(0, p, NOW), Err(IngestError::InvalidAudio));
        assert_eq!(ingest.opus(0, p), Err(IngestError::InvalidAudio));
    }
    assert_eq!(
        ingest.known_audio_config(),
        None,
        "refusal recorded nothing"
    );
    ingest.opus_at(0, &packet(960, false), NOW).expect("Opus");
    assert_eq!(
        ingest.audio_at(0, tests::aac(), &[1], NOW),
        Err(IngestError::InvalidAudio)
    );
    assert_eq!(
        ingest.audio(0, tests::aac(), &[1]),
        Err(IngestError::InvalidAudio)
    );
    let mut aac = EsIngest::new(MuxerConfig::default());
    aac.audio_config(tests::aac());
    assert_eq!(
        aac.opus_at(0, &packet(960, false), NOW),
        Err(IngestError::InvalidAudio)
    );
    assert_eq!(aac.known_audio(), Some(tests::aac()));
    let mut video_only = EsIngest::new(MuxerConfig::default());
    video_only
        .declare_tracks(TrackSet::VideoOnly, NOW)
        .expect("declaration");
    assert_eq!(
        video_only.opus_at(0, &packet(960, false), NOW),
        Err(IngestError::TrackConflict)
    );
}

#[test]
fn an_unsupported_channel_count_is_refused_when_configured() {
    for channels in [0u8, 3, 8] {
        let mut ingest = EsIngest::new(MuxerConfig::default());
        assert_eq!(
            ingest.opus_config(OpusConfig { channels }),
            Err(IngestError::InvalidAudio)
        );
        assert_eq!(ingest.known_audio_config(), None);
        // Nothing was recorded, so the stream still starts from its first
        // packet: a keyframe after it builds a muxer.
        let video = tests::video();
        ingest.parameter_sets(Some(video.sps), Some(video.pps));
        ingest.opus(0, &packet(960, true)).expect("Opus");
        ingest
            .video(
                0,
                0,
                AccessUnit {
                    data: vec![1],
                    idr: true,
                    ..AccessUnit::default()
                },
                NOW,
            )
            .expect("IDR");
        assert_eq!(ingest.muxer().and_then(Muxer::audio_config), Some(&opus(2)));
    }
}

#[test]
fn the_stereo_rule_and_a_changing_toc_stereo_flag() {
    for (negotiated, first, expected) in [
        (None, false, 1u8),
        (None, true, 2),
        (Some(OpusConfig::STEREO), false, 2),
        // Mono is the absence of negotiated stereo: the first TOC decides.
        (Some(OpusConfig::MONO), true, 2),
        (Some(OpusConfig::MONO), false, 1),
    ] {
        for declared in [TrackSet::AudioOnly, TrackSet::AudioVideo] {
            let mut ingest = EsIngest::new(MuxerConfig::default());
            ingest.declare_tracks(declared, NOW).expect("declaration");
            if let Some(config) = negotiated {
                ingest.opus_config(config).expect("supported");
            }
            let video = tests::video();
            ingest.parameter_sets(Some(video.sps), Some(video.pps));
            let mut restarted = false;
            for i in 0..200i64 {
                // The stereo flag flips every 25 packets.
                let stereo = first ^ ((i / 25) % 2 == 1);
                let up = ingest
                    .opus_at(i * 1800, &packet(960, stereo), NOW)
                    .expect("Opus");
                restarted |= up.restarted;
                if declared == TrackSet::AudioVideo && i % 2 == 0 {
                    let up = ingest
                        .video(
                            i * 1800,
                            i * 1800,
                            AccessUnit {
                                data: vec![1],
                                idr: i % 60 == 0,
                                ..AccessUnit::default()
                            },
                            NOW,
                        )
                        .expect("video");
                    restarted |= up.restarted;
                }
            }
            assert!(!restarted, "a TOC change is not a format change");
            assert_eq!(
                ingest.known_audio_config(),
                Some(opus(expected)),
                "{negotiated:?} {first} {declared:?}"
            );
            let m = ingest.muxer().expect("muxer");
            assert_eq!(m.audio_config(), Some(&opus(expected)));
            assert_eq!(m.current_initialization_id(), 0);
            assert_eq!(
                audio_trak(m.init())
                    .mdia
                    .minf
                    .stbl
                    .stsd
                    .codecs
                    .first()
                    .map(|c| match c {
                        Codec::Opus(o) => o.dops.output_channel_count,
                        _ => 0,
                    }),
                Some(expected)
            );
        }
    }
}

#[test]
fn video_led_opus_keeps_timestamps_and_caps_a_gap_at_the_packets_own_duration() {
    for samples in [960u32, 2880] {
        let mut ingest = EsIngest::new(MuxerConfig::default());
        let video = tests::video();
        ingest.parameter_sets(Some(video.sps), Some(video.pps));
        let step = ticks(samples);
        let mut audio = 0i64;
        let mut pushed = 0usize;
        let gap_at = 90_000 * 2;
        for frame in 0..180i64 {
            let dts = frame * 3000;
            while audio <= dts {
                // Two seconds in, the sender stops for a second.
                if !(gap_at..gap_at + 90_000).contains(&audio) {
                    ingest
                        .opus(audio, &packet(samples, true))
                        .expect("video-led Opus");
                    pushed += 1;
                }
                audio += step;
            }
            ingest
                .video(
                    dts,
                    dts,
                    AccessUnit {
                        data: vec![1],
                        idr: frame % 30 == 0,
                        ..AccessUnit::default()
                    },
                    NOW,
                )
                .expect("video");
        }
        let m = ingest.muxer_mut().expect("muxer");
        let durations = audio_durations(m);
        // Not yet in a closed part: the packet before the first keyframe,
        // the pending packet and the open part's packets.
        let slack = 2 + 234 * 48 / samples as usize;
        assert!(durations.len() < pushed && pushed - durations.len() <= slack);
        let regular = durations.iter().filter(|&&d| d == samples).count();
        assert_eq!(
            regular,
            durations.len(),
            "a gap keeps the packet's own duration"
        );
        assert!(
            playlist::multivariant(m).contains("CODECS=\"avc1.4d400d,Opus\""),
            "{}",
            playlist::multivariant(m)
        );
        // Parts are cut on video and audio is split between them by time.
        for part in all_parts(m) {
            let moof = Moof::decode(&mut Cursor::new(&part.data[..])).expect("moof");
            assert!(
                moof.traf
                    .iter()
                    .any(|t| t.tfhd.track_id == fmp4::VIDEO_TRACK)
            );
        }
        assert_part_rules(m);
    }
    // Jitter is kept, as for AAC: a packet 1 ms late lasts 1 ms longer.
    let mut m = Muxer::with_audio_config(
        MuxerConfig::default(),
        Some(tests::video()),
        Some(opus(1)),
        None,
    )
    .expect("A/V");
    m.push_video(0, 0, true, Bytes::from_static(&[1]), NOW)
        .expect("IDR");
    m.push_video(3000, 3000, false, Bytes::from_static(&[1]), NOW)
        .expect("successor");
    for pts in [0, 1800, 1800 * 2 + 90, 1800 * 3] {
        m.push_audio(pts, Bytes::from(packet(960, false)))
            .expect("Opus");
    }
    let durations: Vec<u32> = m
        .part
        .as_ref()
        .expect("part")
        .audio
        .iter()
        .map(|s| s.duration)
        .collect();
    assert_eq!(durations, [960, 1008, 912]);
}

/// Audio-only Opus with timestamps a timeline slews against the sample
/// count: `ppm` fast (negative: slow), `samples` per packet, 120 s. Returns
/// the times of the discontinuities, in seconds.
fn slewed_audio_only(ppm: f64, samples: u32) -> Vec<f64> {
    let mut m = audio_only(MuxerConfig::default(), 2);
    let step = ticks(samples) as f64 * (1.0 + ppm * 1e-6);
    let mut losses = Vec::new();
    let mut i = 0i64;
    loop {
        let pts = (i as f64 * step) as i64;
        if pts > 120 * 90_000 {
            break;
        }
        let up = m
            .push_audio_at(pts, Bytes::from(packet(samples, true)), NOW)
            .expect("Opus");
        if up.media_loss {
            losses.push(pts as f64 / 90_000.0);
        }
        i += 1;
    }
    losses
}

/// The One timeline section slews audio at up to 500 ppm. Timestamps that
/// drift that way against the run's sample count are not breaks.
#[test]
fn audio_only_opus_carries_timeline_slew_without_discontinuities() {
    let mut broken = Vec::new();
    for ppm in [100.0, 500.0, -100.0, -500.0] {
        for samples in [960u32, 120, 2880, 5760] {
            let losses = slewed_audio_only(ppm, samples);
            println!(
                "slew {ppm} ppm, {} ms packets: {} discontinuities in 120 s, first at {:?} s",
                samples as f64 / 48.0,
                losses.len(),
                losses.first()
            );
            if !losses.is_empty() {
                broken.push((ppm, samples, losses.len()));
            }
        }
    }
    assert!(broken.is_empty(), "{broken:?}");
}

/// Pushes a stream `ppm` fast, 400 packets, through `edit`,
/// which sees each packet's index and timestamp and may move it, replace it
/// with a malformed packet, or drop it (`None`). Returns the packets after
/// which media loss was reported.
fn breaks_under_slew(
    ppm: f64,
    samples: u32,
    edit: impl Fn(usize, i64) -> Option<(i64, Vec<u8>)>,
) -> Vec<usize> {
    let mut m = audio_only(MuxerConfig::default(), 2);
    let step = ticks(samples) as f64 * (1.0 + ppm * 1e-6);
    let mut losses = Vec::new();
    for i in 0..400usize {
        let Some((pts, p)) = edit(i, (i as f64 * step) as i64) else {
            continue;
        };
        match m.push_audio_at(pts, Bytes::from(p), NOW) {
            Ok(up) if up.media_loss => losses.push(i),
            Ok(_) => {}
            Err(_) => assert_eq!(i, 200, "only the edited packet may be refused"),
        }
    }
    losses
}

/// A real break is still reported while the timeline slews (500 ppm): a
/// missing, repeated or non-increasing packet. The bound itself, without
/// slew: a step more than 2 ms (180 ticks) from the previous timestamp plus
/// that packet's duration. Tightest at 2.5 ms packets, only 225 ticks long.
#[test]
fn audio_only_opus_reports_real_breaks_while_slewing() {
    for samples in [120u32, 960] {
        let good = || packet(samples, true);
        let d = ticks(samples);
        let at = |pts: i64| Some((pts, good()));
        // Unedited: no break.
        assert_eq!(breaks_under_slew(500.0, samples, |_, pts| at(pts)), []);
        // Packet 200 missing: packet 201 is a packet late.
        assert_eq!(
            breaks_under_slew(500.0, samples, |i, pts| (i != 200).then(|| (pts, good()))),
            [201],
            "{samples}"
        );
        // Packet 200 repeating 199's timestamp, or stepping back past it:
        // a break into it and another out of it.
        for back in [d, 2 * d, d + 1] {
            assert_eq!(
                breaks_under_slew(500.0, samples, |i, pts| at(if i == 200 {
                    pts - back
                } else {
                    pts
                })),
                [200, 201],
                "{samples} {back}"
            );
        }
        // Without slew, every packet from 200 on moved by up to 180 ticks
        // either way is carried; by 181, packet 200 is a break and the rest
        // carry on. (Slew adds up to a tick to a step.)
        for off in [180, -180] {
            assert_eq!(
                breaks_under_slew(0.0, samples, |i, pts| at(if i >= 200 {
                    pts + off
                } else {
                    pts
                })),
                []
            );
        }
        for off in [181, -181] {
            assert_eq!(
                breaks_under_slew(0.0, samples, |i, pts| at(if i >= 200 {
                    pts + off
                } else {
                    pts
                })),
                [200],
                "{samples} {off}"
            );
        }
        // A refused (malformed) packet is missing media: the next is late.
        assert_eq!(
            breaks_under_slew(500.0, samples, |i, pts| Some((
                pts,
                if i == 200 { vec![0xfb, 0] } else { good() }
            ))),
            [201]
        );
    }
}

/// Video-led Opus at `ppm`, `samples` per packet, 30 s beside 30 fps video:
/// every audio sample duration in the closed parts.
fn video_led_durations(ppm: f64, samples: u32) -> Vec<u32> {
    let mut m = Muxer::with_audio_config(
        MuxerConfig::default(),
        Some(tests::video()),
        Some(opus(2)),
        None,
    )
    .expect("A/V");
    let mut watch = Watch::default();
    let step = ticks(samples) as f64 * (1.0 + ppm * 1e-6);
    let mut frame = 0i64;
    let mut i = 0i64;
    while frame < 900 {
        let audio = (i as f64 * step) as i64;
        while frame * 3000 <= audio && frame < 900 {
            m.push_video(
                frame * 3000,
                frame * 3000,
                frame % 30 == 0,
                Bytes::from_static(&[1]),
                NOW,
            )
            .expect("video");
            frame += 1;
        }
        m.push_audio(audio, Bytes::from(packet(samples, true)))
            .expect("Opus");
        watch.observe(&m, audio);
        i += 1;
    }
    watch.audio()
}

/// Video-led Opus sample durations come from the timestamps, as AAC's do,
/// because the shared timeline's slew is what keeps audio with video. With
/// exact timestamps they are exactly each packet's TOC duration; slewed at
/// up to 500 ppm either way, they stay within that slew plus one sample of
/// rounding of it.
#[test]
fn video_led_opus_durations_follow_the_timestamps_within_the_slew() {
    for samples in [120u32, 960, 2880, 5760] {
        let exact = video_led_durations(0.0, samples);
        assert!(exact.len() > 100);
        assert!(exact.iter().all(|&d| d == samples), "{samples}");
        let bound = (f64::from(samples) * 500e-6).ceil() as i64 + 1;
        for ppm in [500.0, -500.0] {
            let slewed = video_led_durations(ppm, samples);
            let worst = slewed
                .iter()
                .map(|&d| (i64::from(d) - i64::from(samples)).abs())
                .max()
                .expect("samples");
            println!(
                "video-led {} ms at {ppm} ppm: largest difference from the TOC duration {worst} samples (bound {bound})",
                f64::from(samples) / 48.0
            );
            assert!(worst <= bound, "{samples} {ppm}: {worst}");
        }
    }
}

#[test]
fn audio_only_opus_reports_a_missing_repeated_or_backward_timestamp_as_loss() {
    let mut m = audio_only(MuxerConfig::default(), 2);
    for i in 0..100 {
        m.push_audio_at(i * 1800, Bytes::from(packet(960, true)), NOW)
            .expect("Opus");
    }
    let missing = m
        .push_audio_at(101 * 1800, Bytes::from(packet(960, true)), NOW)
        .expect("Opus");
    assert!(missing.media_loss);
    let clean = m
        .push_audio_at(102 * 1800, Bytes::from(packet(960, true)), NOW)
        .expect("Opus");
    assert!(!clean.media_loss);
    let repeated = m
        .push_audio_at(102 * 1800, Bytes::from(packet(960, true)), NOW)
        .expect("Opus");
    assert!(repeated.media_loss);
    let backward = m
        .push_audio_at(50 * 1800, Bytes::from(packet(960, true)), NOW)
        .expect("Opus");
    assert!(backward.media_loss);
    // The TOC duration, not 1024 samples, sets the next expected time:
    // a 60 ms packet followed exactly is clean.
    let mut m = audio_only(MuxerConfig::default(), 1);
    let mut pts = 0;
    for samples in [2880u32, 960, 5760, 120, 2880] {
        let up = m
            .push_audio_at(pts, Bytes::from(packet(samples, false)), NOW)
            .expect("Opus");
        assert!(!up.media_loss, "{samples}");
        pts += ticks(samples);
    }
    assert!(!m.interrupt(NOW).media_loss);
    assert!(segments(&m).iter().all(|s| !s.discontinuity));
}

#[test]
fn a_reconnect_with_another_channel_count_starts_a_codec_epoch() {
    let cfg = MuxerConfig {
        segment_count: 100,
        ..MuxerConfig::default()
    };
    let mut first = EsIngest::new(cfg);
    first
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("declaration");
    for i in 0..150 {
        first
            .opus_at(i * 1800, &packet(960, false), NOW)
            .expect("Opus");
    }
    let mut muxer = first.take_muxer().expect("muxer");
    muxer.interrupt(NOW);
    let targets = (muxer.part_target(), muxer.target_duration());
    let mut second = EsIngest::resume(cfg, muxer);
    second
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("declaration");
    let mut restarted = false;
    for i in 0..150 {
        restarted |= second
            .opus_at(5_000_000 + i * 1800, &packet(960, true), NOW)
            .expect("Opus")
            .restarted;
    }
    assert!(restarted);
    let m = second.muxer_mut().expect("muxer");
    m.interrupt(NOW);
    assert_eq!(targets, (m.part_target(), m.target_duration()));
    assert_eq!(m.current_initialization_id(), 1);
    assert_eq!(m.audio_config(), Some(&opus(2)));
    assert_eq!(audio_durations(m).len(), 300);
    assert_part_rules(m);
    // The same channel count continues the epoch.
    let mut muxer = second.take_muxer().expect("muxer");
    muxer.interrupt(NOW);
    let mut third = EsIngest::resume(cfg, muxer);
    third
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("declaration");
    let up = third
        .opus_at(9_000_000, &packet(960, true), NOW)
        .expect("Opus");
    assert!(!up.restarted);
}

fn av_muxer() -> Muxer {
    let mut m = Muxer::with_audio_config(
        MuxerConfig::default(),
        Some(tests::video()),
        Some(opus(2)),
        None,
    )
    .expect("A/V");
    m.push_video(0, 0, true, Bytes::from_static(&[1]), NOW)
        .expect("IDR");
    m.push_video(3000, 3000, false, Bytes::from_static(&[2]), NOW)
        .expect("successor");
    m
}

fn state(m: &Muxer) -> (usize, usize, Option<i64>, usize) {
    let part = m.part.as_ref();
    let pending = m.pending_audio.as_ref();
    (
        part.map_or(0, |p| p.audio.len()) + usize::from(pending.is_some()),
        part.map_or(0, |p| p.audio.iter().map(|a| a.data.len()).sum::<usize>())
            + pending.map_or(0, |p| p.data.len()),
        pending.map(|p| p.dts),
        m.next_segment.as_ref().map_or(0, |s| s.bytes),
    )
}

#[test]
fn opus_payload_reservation_matches_aac() {
    // A TOC byte alone is a valid one-byte packet, as `[1]` is for AAC.
    let one = || Bytes::from_static(&[0xf8]);
    let mut m = av_muxer();
    m.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES - 2;
    m.push_audio_at(0, one(), NOW).expect("one reserved byte");
    m.push_audio_at(1800, one(), NOW).expect("exact two bytes");
    assert_eq!((state(&m).0, state(&m).1), (2, 2));
    let before = state(&m);
    assert!(matches!(
        m.push_audio_at(3600, one(), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(state(&m), before, "refusal preserves accepted Opus");
}

#[test]
fn opus_live_count_matches_aac_for_equal_and_advancing_timestamps() {
    for advancing in [false, true] {
        let mut m = av_muxer();
        for i in 0..LIVE_LIMIT {
            let pts = if advancing { i as i64 * 1800 } else { 0 };
            m.push_audio_at(pts, Bytes::from_static(&[0xf8]), NOW)
                .expect("within the live count");
        }
        let before = state(&m);
        assert_eq!(before.0, LIVE_LIMIT);
        let pts = if advancing {
            LIVE_LIMIT as i64 * 1800
        } else {
            0
        };
        assert!(matches!(
            m.push_audio_at(pts, Bytes::from_static(&[0xf8]), NOW),
            Err(MuxError::SegmentTooLarge)
        ));
        assert_eq!(state(&m), before);
    }
}

#[test]
fn audio_only_opus_segment_bytes_are_bounded_like_aac() {
    let mut m = audio_only(MuxerConfig::default(), 1);
    m.push_audio_at(0, Bytes::from(packet(960, false)), NOW)
        .expect("first");
    m.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES - 3;
    m.push_audio_at(1800, Bytes::from(packet(960, false)), NOW)
        .expect("exactly fits");
    let before = (m.part.as_ref().map(|p| p.audio.len()), m.audio_end);
    assert!(matches!(
        m.push_audio_at(3600, Bytes::from(packet(960, false)), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(
        before,
        (m.part.as_ref().map(|p| p.audio.len()), m.audio_end)
    );
}

#[test]
fn elementary_opus_propagates_admission_refusal_and_timestamp_overflow() {
    let mut ingest = EsIngest::new(MuxerConfig::default());
    let video = tests::video();
    ingest.parameter_sets(Some(video.sps), Some(video.pps));
    ingest.opus_config(OpusConfig::STEREO).expect("stereo");
    for (dts, idr) in [(1, true), (3001, false)] {
        ingest
            .video(
                dts,
                dts,
                AccessUnit {
                    data: vec![1],
                    idr,
                    ..AccessUnit::default()
                },
                NOW,
            )
            .expect("video");
    }
    assert_eq!(
        ingest.opus_at(i64::MIN, &packet(960, true), NOW),
        Err(IngestError::InvalidTimestamp)
    );
    ingest
        .muxer_mut()
        .expect("muxer")
        .next_segment
        .as_mut()
        .expect("segment")
        .bytes = MAX_SEGMENT_BYTES;
    assert_eq!(
        ingest.opus_at(1, &packet(960, true), NOW),
        Err(IngestError::SegmentTooLarge)
    );
    assert!(ingest.muxer().expect("muxer").pending_audio.is_none());
}

#[test]
fn opus_with_a_fragment_quota_charges_the_init_and_parts() {
    let quota = Arc::new(FragmentQuota::new(1 << 20));
    let mut ingest = EsIngest::with_fragment_quota(MuxerConfig::default(), quota.clone());
    ingest
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("declaration");
    ingest.opus_at(0, &packet(960, true), NOW).expect("first");
    let charged = quota.used();
    assert!(charged > 0, "the Opus init is charged");
    for i in 1..100 {
        ingest
            .opus_at(i * 1800, &packet(960, true), NOW)
            .expect("Opus");
    }
    assert!(quota.used() > charged, "parts are charged");
}

#[test]
fn a_video_only_muxer_with_nothing_servable_is_replaced_when_opus_arrives() {
    let mut ingest = EsIngest::new(MuxerConfig::default());
    ingest.set_expect_audio(false);
    let video = tests::video();
    ingest.parameter_sets(Some(video.sps), Some(video.pps));
    ingest
        .video(
            0,
            0,
            AccessUnit {
                data: vec![1],
                idr: true,
                ..AccessUnit::default()
            },
            NOW,
        )
        .expect("IDR");
    assert_eq!(
        ingest.muxer().expect("video muxer").tracks(),
        TrackSet::VideoOnly
    );
    ingest.opus(0, &packet(960, false)).expect("Opus");
    assert!(
        ingest.muxer().is_none(),
        "provisional video-only muxer dropped"
    );
    // As the FLV front end does after an AAC sequence header.
    ingest.set_expect_audio(true);
    ingest
        .video(
            3000,
            3000,
            AccessUnit {
                data: vec![1],
                idr: true,
                ..AccessUnit::default()
            },
            NOW,
        )
        .expect("IDR");
    assert_eq!(
        ingest.muxer().expect("A/V muxer").audio_config(),
        Some(&opus(1))
    );
}
