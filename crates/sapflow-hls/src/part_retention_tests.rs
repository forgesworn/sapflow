//! RFC 8216bis section 6.2.2 (drafts 19 and 22): "Clients MUST be able to
//! download the Partial Segment for at least three Target Durations after
//! the EXT-X-PART tag is removed from the Playlist."
//!
//! Measured as a client sees it: the part tags each render lists, and
//! whether `Muxer::part`, which the server answers part requests from,
//! still serves each part whose tag has gone. Time is a clock that runs
//! with the media pushed, as from a publisher sending in real time.
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

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
    /// The crate's own checker, which the fuzz targets run.
    checker: playlist::RetentionCheck,
    broken: Option<String>,
}

impl Retention {
    fn observe(&mut self, m: &Muxer, input: i64) {
        let clock = (input, self.published.advance(m));
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
    closed: std::time::Duration,
    latest: i64,
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
        let open: std::time::Duration = m.open_parts().iter().map(|p| p.span).sum();
        let ticks = ((self.closed + open).as_nanos() * 9 / 100_000) as i64;
        self.latest = self.latest.max(ticks);
        self.latest
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

#[test]
fn parts_stay_downloadable_three_target_durations_after_their_tags_leave() {
    let cases = [
        steady("steady, segment_count 7", 7),
        steady("steady, segment_count 12", 12),
        steady("steady, segment_count 24", 24),
        aac_reconnects("AAC, reconnect every 10 frames", 10),
        aac_reconnects("AAC, reconnect every 20 frames", 20),
        epochs(),
        opus_audio_only("Opus audio-only 20 ms, segment_count 7", 960),
        opus_audio_only("Opus audio-only 60 ms, segment_count 7", 2880),
        opus_video_led(),
    ];
    let mut short = Vec::new();
    for case in &cases {
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

/// Bytes and count of the parts still served for segments that have left
/// the window.
fn kept_past_the_window(m: &Muxer) -> (usize, usize) {
    let first = m
        .entries()
        .iter()
        .find_map(|e| match e {
            crate::muxer::Entry::Segment(s) => s.parts.first().map(|p| p.id),
            crate::muxer::Entry::Gap { .. } => None,
        })
        .unwrap_or(m.next_part_id());
    (0..first)
        .filter_map(|id| match m.part(id) {
            PartLookup::Found(p) => Some(p.data.len()),
            _ => None,
        })
        .fold((0, 0), |(bytes, n), len| (bytes + len, n + 1))
}

/// 2 MiB AAC frames, the publisher reconnecting after each: three target
/// durations of them would be far more than 64 MiB, so the oldest go early
/// and are counted.
#[test]
fn parts_kept_past_the_window_stop_at_64_mib_and_are_counted() {
    let config = AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("AAC");
    let mut m = Muxer::audio_only(MuxerConfig::default(), config).expect("AAC");
    let payload = Bytes::from(vec![0u8; 2 << 20]);
    let mut checker = playlist::RetentionCheck::default();
    let (mut most, mut pts) = (0, 0);
    for _ in 0..200 {
        m.push_audio_at(pts, payload.clone(), NOW).expect("AAC");
        pts += 1920;
        m.interrupt(NOW);
        checker
            .check(&m)
            .expect("a part goes early only when counted");
        most = most.max(kept_past_the_window(&m).0);
    }
    assert!(m.parts_released_early() > 0, "the byte cap was reached");
    assert!(most > 0 && most <= 64 << 20, "{most}");
}

/// A 45 s first segment fixes a 46 s target, then one-frame segments:
/// three target durations of them are more parts than the count cap.
#[test]
fn parts_kept_past_the_window_stop_at_the_count_cap() {
    let mut m = Muxer::new(MuxerConfig::default(), video(), None);
    let frame = |m: &mut Muxer, i: i64, sync: bool| {
        m.push_video(i * 3000, i * 3000, sync, Bytes::from_static(&[1]), NOW)
            .expect("video");
    };
    for i in 0..=45 * 30 {
        frame(&mut m, i, i % (45 * 30) == 0);
    }
    assert_eq!(m.target_duration(), 46);
    let mut most = 0;
    let mut i = 45 * 30 + 1;
    while i < 45 * 30 + 2 * 5000 {
        // The first frame is sealed by the second; the second is dropped.
        frame(&mut m, i, true);
        frame(&mut m, i + 1, true);
        m.interrupt(NOW);
        i += 2;
        if i % 100 == 0 {
            most = most.max(kept_past_the_window(&m).1);
        }
    }
    most = most.max(kept_past_the_window(&m).1);
    assert!(m.parts_released_early() > 0, "the count cap was reached");
    assert!(
        most > 0 && most <= crate::muxer::MAX_RETAINED_PARTS,
        "{most}"
    );
}

/// A fragment budget that holds the window but not three more target
/// durations of parts past it: the kept parts give way to new media, which
/// is never refused, and each part that went early is counted.
#[test]
fn parts_kept_past_the_window_give_way_under_the_fragment_budget() {
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
            .expect("a part goes early only when counted");
    }
    assert!(m.parts_released_early() > 0, "the budget was reached");
    assert!(quota.used() <= 6 << 20);
}
