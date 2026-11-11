//! RFC 8216bis section 6.2.2 (drafts 19 and 22, worded the same):
//!
//! - "Clients MUST be able to download the Partial Segment for at least
//!   three Target Durations after the EXT-X-PART tag is removed from the
//!   Playlist."
//! - "The Availability Duration of a Media Segment is the duration of the
//!   segment plus the duration of the longest-duration Playlist
//!   distributed by the server containing that segment. [...] If the
//!   server removes a Media Segment URI from a Playlist that does not
//!   contain an EXT-X-ENDLIST tag, clients MUST be able to download the
//!   segment until the time at which it first appeared in the Playlist
//!   plus the segment's Availability Duration." sapflow never writes
//!   `EXT-X-ENDLIST`.
//!
//! Measured as a client sees it: the part tags each render lists, the
//! segments each lists and the playlist's `EXTINF` total, and whether
//! `Muxer::part` and `Muxer::segment`, which the server answers chunk
//! requests from, still serve them. Time is a clock that runs with the
//! media pushed, as from a publisher sending in real time, and the media
//! the muxer has published.
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use bytes::Bytes;

use crate::codec::{AacConfig, AccessUnit, OpusConfig};
use crate::muxer::tests::{aac, video};
use crate::{EsIngest, Muxer, MuxerConfig, PartLookup, TrackSet, TsIngest, playlist};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;

/// What a client could fetch, render by render, on two clocks: the input
/// timestamps (a publisher sending in real time), and the media the muxer
/// has published.
#[derive(Default)]
struct Retention {
    listed: BTreeSet<u64>,
    /// Part id, and both clocks (90 kHz) when its tag left the playlist.
    removed: BTreeMap<u64, (i64, i64)>,
    /// Removed parts still served.
    pending: BTreeSet<u64>,
    /// Part id, and how long it was still served after its tag left.
    held: BTreeMap<u64, (i64, i64)>,
    published: Published,
    /// Segment id: both clocks when it first appeared (the published one at
    /// the end of its own `EXTINF`), and its duration plus the longest
    /// playlist that listed it.
    segments: BTreeMap<u64, ((i64, Duration), Duration)>,
    /// Segment id, and how long it was still served past its Availability
    /// Duration, on both clocks (negative: gone early).
    margin: BTreeMap<u64, (i64, i128)>,
    /// The crate's own checker, which the fuzz targets run.
    checker: playlist::RetentionCheck,
    broken: Option<String>,
}

impl Retention {
    fn observe(&mut self, m: &Muxer, input: i64) {
        let clock = (input, self.published.advance(m));
        self.observe_segments(m, input);
        if let Err(e) = self.checker.check(m)
            && self.broken.is_none()
        {
            self.broken = Some(e);
        }
        if !m.has_content() {
            return;
        }
        let pl = playlist::media(m, "r");
        let listed: BTreeSet<u64> = pl
            .lines()
            .filter(|l| l.starts_with("#EXT-X-PART:"))
            .filter_map(|l| l.split("URI=\"r_part").nth(1))
            .filter_map(|r| r.split('.').next()?.parse().ok())
            .collect();
        for id in &listed {
            assert!(
                matches!(m.part(*id), PartLookup::Found(_)),
                "part {id} listed but not served"
            );
        }
        for &id in self.listed.difference(&listed) {
            self.removed.entry(id).or_insert(clock);
            self.pending.insert(id);
        }
        let gone: Vec<u64> = self
            .pending
            .iter()
            .copied()
            .filter(|id| matches!(m.part(*id), PartLookup::Missing))
            .collect();
        for id in gone {
            self.pending.remove(&id);
            let (input, published) = self.removed[&id];
            self.held.insert(id, (clock.0 - input, clock.1 - published));
        }
        self.listed = listed;
    }

    fn observe_segments(&mut self, m: &Muxer, input: i64) {
        let total: Duration = m.entries().iter().map(|e| e.duration()).sum();
        let mut end = self.published.closed_before(m);
        for (i, entry) in m.entries().iter().enumerate() {
            end += entry.duration();
            let crate::muxer::Entry::Segment(s) = entry else {
                continue;
            };
            let id = m.media_sequence() + i as u64;
            let seen = self
                .segments
                .entry(id)
                .or_insert(((input, end), s.duration));
            seen.1 = seen.1.max(s.duration + total);
        }
        let gone: Vec<u64> = self
            .segments
            .keys()
            .copied()
            .filter(|&id| id < m.media_sequence() && !self.margin.contains_key(&id))
            .filter(|&id| match m.segment(id) {
                // A kept segment is served with its init.
                Some(s) => {
                    assert!(m.initialization(s.initialization.id).is_some());
                    false
                }
                None => true,
            })
            .collect();
        let now = self.published.now();
        for id in gone {
            let ((appeared_input, appeared), availability) = self.segments[&id];
            let input_margin =
                input - appeared_input - (availability.as_nanos() * 9 / 100_000) as i64;
            let margin = now.as_nanos() as i128 - (appeared + availability).as_nanos() as i128;
            self.margin.insert(id, (input_margin, margin));
        }
    }

    /// The least time, in seconds of input and of published media, any
    /// segment was still served past its Availability Duration, over the
    /// segments that have gone; negative if one went early.
    fn least_margin(&self) -> Option<(f64, f64)> {
        let input = self.margin.values().map(|m| m.0).min()?;
        let published = self.margin.values().map(|m| m.1).min()?;
        Some((input as f64 / 90_000.0, published as f64 / 1e9))
    }

    /// The shortest time, in seconds of input and of published media, any
    /// part was still served after its tag left, over the parts that have
    /// gone.
    fn shortest(&self) -> Option<(f64, f64)> {
        let secs = |t: i64| t as f64 / 90_000.0;
        let input = self.held.values().map(|h| h.0).min()?;
        let published = self.held.values().map(|h| h.1).min()?;
        Some((secs(input), secs(published)))
    }
}

/// One measured case: its name, the target duration, and the retention.
struct Case {
    name: &'static str,
    target: u64,
    retention: Retention,
}

/// Steady 30 fps video with a keyframe each second and 48 kHz AAC, for 60 s.
fn steady(name: &'static str, segment_count: usize) -> Case {
    let cfg = MuxerConfig {
        segment_count,
        ..MuxerConfig::default()
    };
    let mut m = Muxer::new(cfg, video(), Some(aac()));
    let mut retention = Retention::default();
    let mut next_audio = 0;
    for i in 0..60 * 30 {
        let dts = i * 3000;
        while next_audio <= dts {
            m.push_audio(next_audio, Bytes::from_static(b"aac"))
                .expect("AAC");
            next_audio += 1920;
        }
        m.push_video(dts, dts, i % 30 == 0, Bytes::from(vec![0u8; 100]), NOW)
            .expect("video");
        retention.observe(&m, dts);
    }
    Case {
        name,
        target: m.target_duration(),
        retention,
    }
}

/// Opus packets of `samples` (TOC config 31 for 20 ms, 11 for 60 ms).
fn opus_packet(samples: u32) -> Vec<u8> {
    match samples {
        960 => vec![31 << 3 | 4, 0xff, 0xfe],
        _ => vec![11 << 3 | 4, 1, 2],
    }
}

/// Audio-only Opus at `segment_count` 7, for 60 s.
fn opus_audio_only(name: &'static str, samples: u32) -> Case {
    let cfg = MuxerConfig {
        segment_count: 7,
        ..MuxerConfig::default()
    };
    let mut ingest = EsIngest::new(cfg);
    ingest
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("audio-only");
    let mut retention = Retention::default();
    let step = i64::from(samples) * 90_000 / 48_000;
    let mut pts = 0;
    while pts < 60 * 90_000 {
        ingest
            .opus_at(pts, &opus_packet(samples), NOW)
            .expect("Opus");
        pts += step;
        retention.observe(ingest.muxer().expect("muxer"), pts);
    }
    Case {
        name,
        target: ingest.muxer().expect("muxer").target_duration(),
        retention,
    }
}

/// Video-led Opus at `segment_count` 7: 30 fps with a keyframe each second
/// and 20 ms packets, for 60 s.
fn opus_video_led() -> Case {
    let cfg = MuxerConfig {
        segment_count: 7,
        ..MuxerConfig::default()
    };
    let video = video();
    let mut ingest = EsIngest::new(cfg);
    ingest
        .declare_tracks(TrackSet::AudioVideo, NOW)
        .expect("A/V");
    ingest.opus_config(OpusConfig::STEREO).expect("stereo");
    ingest.parameter_sets(Some(video.sps), Some(video.pps));
    let mut retention = Retention::default();
    let mut audio = 0i64;
    for frame in 0..60 * 30i64 {
        while audio <= frame * 3000 {
            ingest.opus_at(audio, &opus_packet(960), NOW).expect("Opus");
            audio += 1800;
        }
        ingest
            .video(
                frame * 3000,
                frame * 3000,
                AccessUnit {
                    data: vec![1; 100],
                    idr: frame % 30 == 0,
                    ..Default::default()
                },
                NOW,
            )
            .expect("video");
        if let Some(m) = ingest.muxer() {
            retention.observe(m, frame * 3000);
        }
    }
    Case {
        name: "Opus video-led 20 ms, segment_count 7",
        target: ingest.muxer().expect("muxer").target_duration(),
        retention,
    }
}

/// AAC-only at the default window, the publisher reconnecting every
/// `every` frames.
fn aac_reconnects(name: &'static str, every: i64) -> Case {
    let config = AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("AAC");
    let mut m = Muxer::audio_only(MuxerConfig::default(), config).expect("AAC");
    let mut retention = Retention::default();
    let mut pts = 0;
    for i in 0..3000 {
        m.push_audio_at(pts, Bytes::from_static(&[1, 2]), NOW)
            .expect("AAC");
        pts += 1920;
        if i % every == every - 1 {
            m.interrupt(NOW);
        }
        retention.observe(&m, pts);
    }
    Case {
        name,
        target: m.target_duration(),
        retention,
    }
}

/// The media a client has seen published: every entry the window has held,
/// once each, plus the open parts, in 90 kHz ticks. A clock for input whose
/// own timestamps restart.
#[derive(Default)]
struct Published {
    counted: u64,
    closed: Duration,
    /// The `EXTINF` total of every entry that has left the window.
    before: Duration,
    latest: Duration,
}

impl Published {
    fn advance(&mut self, m: &Muxer) -> i64 {
        for (i, entry) in m.entries().iter().enumerate() {
            let msn = m.media_sequence() + i as u64;
            if msn >= self.counted {
                self.closed += entry.duration();
                self.counted = msn + 1;
            }
        }
        let open: Duration = m.open_parts().iter().map(|p| p.span).sum();
        self.latest = self.latest.max(self.closed + open);
        (self.latest.as_nanos() * 9 / 100_000) as i64
    }

    fn now(&self) -> Duration {
        self.latest
    }

    /// Published media before the window's first entry, once `advance` has
    /// counted the window.
    fn closed_before(&mut self, m: &Muxer) -> Duration {
        let listed: Duration = m.entries().iter().map(|e| e.duration()).sum();
        self.before = self.closed - listed;
        self.before
    }
}

/// The epoch regression: 16 reconnects alternating two resolutions, eight
/// entries kept.
fn epochs() -> Case {
    let portrait = include_bytes!("../tests/fixtures/epoch-portrait.ts");
    let landscape = include_bytes!("../tests/fixtures/epoch-landscape.ts");
    let cfg = MuxerConfig {
        segment_count: 8,
        ..MuxerConfig::default()
    };
    let mut ingest = TsIngest::new(cfg);
    let mut retention = Retention::default();
    // The fixture's timestamps restart each epoch: the published clock
    // stands in for the input one.
    let mut clock = Published::default();
    for epoch in 0..16 {
        let bytes = if epoch % 2 == 0 {
            portrait.as_slice()
        } else {
            landscape.as_slice()
        };
        for chunk in bytes.chunks(1316) {
            ingest.push(chunk, NOW).expect("fixture");
            if let Some(m) = ingest.muxer() {
                retention.observe(m, clock.advance(m));
            }
        }
        let mut held = ingest.take_muxer().expect("fixture");
        held.interrupt(NOW);
        retention.observe(&held, clock.advance(&held));
        ingest = TsIngest::resume(cfg, held);
    }
    Case {
        name: "epochs, segment_count 8",
        target: ingest.muxer().expect("fixture").target_duration(),
        retention,
    }
}

fn cases() -> [Case; 9] {
    [
        steady("steady, segment_count 7", 7),
        steady("steady, segment_count 12", 12),
        steady("steady, segment_count 24", 24),
        aac_reconnects("AAC, reconnect every 10 frames", 10),
        aac_reconnects("AAC, reconnect every 20 frames", 20),
        epochs(),
        opus_audio_only("Opus audio-only 20 ms, segment_count 7", 960),
        opus_audio_only("Opus audio-only 60 ms, segment_count 7", 2880),
        opus_video_led(),
    ]
}

#[test]
fn parts_stay_downloadable_three_target_durations_after_their_tags_leave() {
    let mut short = Vec::new();
    for case in &cases() {
        let shortest = case.retention.shortest();
        println!(
            "{}: target {} s, {} parts gone, shortest held after the tag left \
             (input s, published s) {:?}",
            case.name,
            case.target,
            case.retention.held.len(),
            shortest
        );
        assert!(!case.retention.held.is_empty(), "{}: parts went", case.name);
        assert_eq!(case.retention.broken, None, "{}: RetentionCheck", case.name);
        if shortest.is_some_and(|(_, published)| published < (3 * case.target) as f64) {
            short.push(case.name);
        }
    }
    assert!(short.is_empty(), "under three target durations: {short:?}");
}

#[test]
fn segments_stay_downloadable_for_their_availability_duration() {
    let mut early = Vec::new();
    for case in &cases() {
        let margin = case.retention.least_margin();
        println!(
            "{}: {} segments gone, least time served past the Availability \
             Duration (input s, published s) {:?}",
            case.name,
            case.retention.margin.len(),
            margin
        );
        assert!(
            !case.retention.margin.is_empty(),
            "{}: segments went",
            case.name
        );
        assert_eq!(case.retention.broken, None, "{}: RetentionCheck", case.name);
        if margin.is_some_and(|(_, published)| published < 0.0) {
            early.push(case.name);
        }
    }
    assert!(
        early.is_empty(),
        "gone before their Availability Duration: {early:?}"
    );
}

/// What is still served for segments that have left the window.
#[derive(Debug, Default, Clone, Copy)]
struct Kept {
    parts: usize,
    segments: usize,
    /// Bytes of the kept parts and segments together: a segment's bytes
    /// are its parts' bytes, counted once.
    bytes: usize,
}

fn kept_past_the_window(m: &Muxer) -> Kept {
    let first = m
        .entries()
        .iter()
        .find_map(|e| match e {
            crate::muxer::Entry::Segment(s) => s.parts.first().map(|p| p.id),
            crate::muxer::Entry::Gap { .. } => None,
        })
        .unwrap_or(m.next_part_id());
    let mut parts: BTreeMap<u64, usize> = (0..first)
        .filter_map(|id| match m.part(id) {
            PartLookup::Found(p) => Some((id, p.data.len())),
            _ => None,
        })
        .collect();
    let mut kept = Kept {
        parts: parts.len(),
        ..Kept::default()
    };
    let from = m
        .media_sequence()
        .saturating_sub(2 * crate::muxer::MAX_RETAINED_SEGMENTS as u64);
    for id in from..m.media_sequence() {
        if let Some(s) = m.segment(id) {
            kept.segments += 1;
            kept.bytes += s.data.len();
            for p in &s.parts {
                parts.remove(&p.id);
            }
        }
    }
    kept.bytes += parts.values().sum::<usize>();
    kept
}

/// 2 MiB AAC frames, the publisher reconnecting after each: three target
/// durations of their parts, and their segments' Availability Durations,
/// would hold far more than 64 MiB, so the oldest go early and are
/// counted.
#[test]
fn kept_parts_and_segments_stop_at_64_mib_and_are_counted() {
    let config = AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("AAC");
    let mut m = Muxer::audio_only(MuxerConfig::default(), config).expect("AAC");
    let payload = Bytes::from(vec![0u8; 2 << 20]);
    let mut checker = playlist::RetentionCheck::default();
    let (mut most, mut pts) = (Kept::default(), 0);
    for _ in 0..200 {
        m.push_audio_at(pts, payload.clone(), NOW).expect("AAC");
        pts += 1920;
        m.interrupt(NOW);
        checker
            .check(&m)
            .expect("a part or segment goes early only when counted");
        let now = kept_past_the_window(&m);
        most.bytes = most.bytes.max(now.bytes);
        most.segments = most.segments.max(now.segments);
    }
    assert!(m.parts_released_early() > 0, "the byte cap was reached");
    assert!(most.bytes > 0 && most.bytes <= 64 << 20, "{most:?}");
    assert!(most.segments > 0, "{most:?}");
}

/// A 45 s first segment fixes a 46 s target, then one-frame segments:
/// three target durations of them are more parts than the part cap, and
/// the 45 s playlists they appear in keep more segments than the segment
/// cap.
#[test]
fn kept_parts_and_segments_stop_at_their_count_caps() {
    let mut m = Muxer::new(MuxerConfig::default(), video(), None);
    let frame = |m: &mut Muxer, i: i64, sync: bool| {
        m.push_video(i * 3000, i * 3000, sync, Bytes::from_static(&[1]), NOW)
            .expect("video");
    };
    for i in 0..=45 * 30 {
        frame(&mut m, i, i % (45 * 30) == 0);
    }
    assert_eq!(m.target_duration(), 46);
    let mut most = Kept::default();
    for step in 0..5000 {
        // The first frame is sealed by the second; the second is dropped.
        let i = 45 * 30 + 1 + 2 * step;
        frame(&mut m, i, true);
        frame(&mut m, i + 1, true);
        m.interrupt(NOW);
        if step % 250 == 249 {
            let now = kept_past_the_window(&m);
            most.parts = most.parts.max(now.parts);
            most.segments = most.segments.max(now.segments);
        }
    }
    assert!(m.parts_released_early() > 0, "the part cap was reached");
    assert!(
        m.segments_released_early() > 0,
        "the segment cap was reached"
    );
    assert!(
        most.parts > 0 && most.parts <= crate::muxer::MAX_RETAINED_PARTS,
        "{most:?}"
    );
    assert!(
        most.segments > 0 && most.segments <= crate::muxer::MAX_RETAINED_SEGMENTS,
        "{most:?}"
    );
}

/// A fragment budget that holds the window but not three more target
/// durations of parts, and the segments' Availability Durations, past it:
/// what is kept gives way to new media, which is never refused, and each
/// part and segment that went early is counted.
#[test]
fn kept_parts_and_segments_give_way_under_the_fragment_budget() {
    let cfg = MuxerConfig {
        segment_count: 7,
        ..MuxerConfig::default()
    };
    let quota = std::sync::Arc::new(sapflow_core::fragment::FragmentQuota::new(6 << 20));
    let mut m =
        Muxer::with_fragment_quota(cfg, Some(video()), Some(aac()), quota.clone()).expect("muxer");
    let mut checker = playlist::RetentionCheck::default();
    let mut next_audio = 0;
    for i in 0..60 * 30 {
        let dts = i * 3000;
        while next_audio <= dts {
            m.push_audio(next_audio, Bytes::from_static(b"aac"))
                .expect("AAC");
            next_audio += 1920;
        }
        m.push_video(dts, dts, i % 30 == 0, Bytes::from(vec![0u8; 20_000]), NOW)
            .expect("never refused");
        checker
            .check(&m)
            .expect("a part or segment goes early only when counted");
    }
    assert!(m.parts_released_early() > 0, "the budget was reached");
    assert!(
        m.segments_released_early() > 0,
        "kept segments gave way too"
    );
    assert!(quota.used() <= 6 << 20);
}
