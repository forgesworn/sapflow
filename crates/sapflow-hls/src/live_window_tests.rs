//! RFC 8216bis section 6.2.2: a server must not remove a media segment from
//! a live playlist if the playlist left would last less than three target
//! durations. Measured as a client reads it: the printed `EXTINF` values of
//! a render, once its media sequence number shows segments were removed.
use std::time::SystemTime;

use bytes::Bytes;

use crate::codec::AacConfig;
use crate::{Muxer, MuxerConfig, TsIngest, playlist};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;

/// The media sequence number of a render, and the `EXTINF` total it lists,
/// in 10 µs units as printed.
fn listed(pl: &str) -> (u64, u64) {
    let msn = pl
        .lines()
        .find_map(|l| l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|v| v.parse().ok())
        .expect("media sequence");
    let units = pl
        .lines()
        .filter_map(|l| l.strip_prefix("#EXTINF:"))
        .map(|v| {
            let v = v.trim_end_matches(',');
            let (whole, frac) = v.split_once('.').unwrap_or((v, ""));
            format!("{whole}{frac:0<5}")
                .parse::<u64>()
                .expect("decimal")
        })
        .sum();
    (msn, units)
}

/// The shortest window any render listed after a removal, in seconds, and
/// the largest number of entries the muxer held.
#[derive(Debug, Default)]
struct Window {
    shortest: Option<f64>,
    most_entries: usize,
    renders: usize,
    rules: playlist::RuleCheck,
    broken: Option<String>,
}

impl Window {
    fn observe(&mut self, m: &Muxer) {
        self.most_entries = self.most_entries.max(m.entries().len());
        if !m.has_content() {
            return;
        }
        if self.broken.is_none()
            && let Err(e) = self.rules.check(&playlist::media(m, "w"))
        {
            self.broken = Some(e);
        }
        for pl in [
            playlist::media(m, "w"),
            playlist::media_with_mode(m, "w", false),
        ] {
            let (msn, units) = listed(&pl);
            if msn > 0 {
                let secs = units as f64 / 100_000.0;
                self.shortest = Some(self.shortest.map_or(secs, |s: f64| s.min(secs)));
                self.renders += 1;
            }
        }
    }
}

/// The epoch regression: 16 reconnects alternating two resolutions, eight
/// entries kept.
fn epochs() -> (Window, u64) {
    let portrait = include_bytes!("../tests/fixtures/epoch-portrait.ts");
    let landscape = include_bytes!("../tests/fixtures/epoch-landscape.ts");
    let cfg = MuxerConfig {
        segment_count: 8,
        ..MuxerConfig::default()
    };
    let mut ingest = TsIngest::new(cfg);
    let mut window = Window::default();
    for epoch in 0..16 {
        let bytes = if epoch % 2 == 0 {
            portrait.as_slice()
        } else {
            landscape.as_slice()
        };
        for chunk in bytes.chunks(1316) {
            ingest.push(chunk, NOW).expect("fixture");
            if let Some(m) = ingest.muxer() {
                window.observe(m);
            }
        }
        let mut held = ingest.take_muxer().expect("fixture");
        held.interrupt(NOW);
        window.observe(&held);
        ingest = TsIngest::resume(cfg, held);
    }
    let target = ingest.muxer().expect("fixture").target_duration();
    (window, target)
}

/// AAC-only at the default window, the publisher reconnecting every ten
/// frames (213 ms).
fn aac_reconnects() -> (Window, u64) {
    let aac = AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("AAC");
    let mut m = Muxer::audio_only(MuxerConfig::default(), aac).expect("AAC");
    let mut window = Window::default();
    let mut pts = 0;
    for i in 0..2000 {
        m.push_audio_at(pts, Bytes::from_static(&[1, 2]), NOW)
            .expect("AAC");
        pts += 1920;
        if i % 10 == 9 {
            m.interrupt(NOW);
        }
        window.observe(&m);
    }
    (window, m.target_duration())
}

#[test]
fn removal_keeps_three_target_durations_listed() {
    let mut short = Vec::new();
    for (name, (window, target)) in [("epochs", epochs()), ("aac-reconnects", aac_reconnects())] {
        println!(
            "{name}: shortest window after a removal {:?} s over {} renders, most entries {}, target {target} s",
            window.shortest, window.renders, window.most_entries
        );
        assert!(window.renders > 0, "{name}: segments were removed");
        assert_eq!(window.broken, None, "{name}: RuleCheck");
        if window.shortest.is_some_and(|s| s < (3 * target) as f64) {
            short.push(name);
        }
    }
    assert!(short.is_empty(), "under three target durations: {short:?}");
}

/// AAC-only, the publisher reconnecting after every frame: 21 ms segments,
/// which three target durations would need hundreds of. The window stops at
/// its entry cap, and the cap wins over the three-target rule.
#[test]
fn the_entry_cap_wins_over_the_window_for_tiny_segments() {
    let aac = AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("AAC");
    let mut m = Muxer::audio_only(MuxerConfig::default(), aac).expect("AAC");
    let mut window = Window::default();
    let mut pts = 0;
    for _ in 0..1000 {
        m.push_audio_at(pts, Bytes::from_static(&[1, 2]), NOW)
            .expect("AAC");
        pts += 1920;
        m.interrupt(NOW);
        window.observe(&m);
        assert!(m.entries().len() <= crate::muxer::MAX_WINDOW_ENTRIES);
    }
    assert_eq!(window.most_entries, crate::muxer::MAX_WINDOW_ENTRIES);
    assert!(window.shortest.is_some_and(|s| s < 6.0), "{window:?}");
    // RuleCheck allows it: the render lists the cap.
    assert_eq!(window.broken, None);
}

/// Short segments of 2 MiB each (a high bitrate, reconnecting): beyond the
/// first `segment_count` entries the window holds at most 64 MiB more.
#[test]
fn the_byte_cap_bounds_what_the_window_holds_beyond_its_count() {
    let aac = AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("AAC");
    let cfg = MuxerConfig::default();
    let mut m = Muxer::audio_only(cfg, aac).expect("AAC");
    let payload = Bytes::from(vec![0u8; 2 << 20]);
    let mut pts = 0;
    let mut most_extra = 0usize;
    // RuleCheck with the muxer's byte-cap signal accepts every render;
    // without it, the cut-short window is refused.
    let (mut signalled, mut plain) = (
        playlist::RuleCheck::default(),
        playlist::RuleCheck::default(),
    );
    let (mut refused_plain, mut capped) = (false, false);
    // The signal is exact: a render on its own breaks the window rule only
    // when the muxer says the byte cap cut it.
    let mut check = |m: &Muxer| {
        if !m.has_content() {
            return;
        }
        let pl = playlist::media(m, "w");
        signalled
            .check_with_byte_cap(&pl, m.window_byte_capped())
            .expect("with the muxer's signal");
        refused_plain |= plain.check(&pl).is_err();
        capped |= m.window_byte_capped();
        assert_eq!(
            playlist::RuleCheck::default().check(&pl).is_err(),
            m.window_byte_capped(),
            "{} entries, listed {:?}",
            m.entries().len(),
            listed(&pl)
        );
    };
    for _ in 0..120 {
        m.push_audio_at(pts, payload.clone(), NOW).expect("AAC");
        pts += 1920;
        m.interrupt(NOW);
        check(&m);
        let extra: usize = m
            .entries()
            .iter()
            .take(m.entries().len().saturating_sub(cfg.segment_count))
            .map(|e| match e {
                crate::muxer::Entry::Segment(s) => s.data.len(),
                crate::muxer::Entry::Gap { .. } => 0,
            })
            .sum();
        most_extra = most_extra.max(extra);
    }
    assert!(
        m.entries().len() > cfg.segment_count,
        "the window rule held more"
    );
    assert!(m.entries().len() < crate::muxer::MAX_WINDOW_ENTRIES);
    assert!(most_extra <= 64 << 20, "{most_extra}");
    assert!(m.window_byte_capped(), "still cut short");
    // Small segments of ten frames after it: the large ones leave, the
    // window grows back to three target durations and the signal clears.
    let mut small = 0;
    while m.window_byte_capped() && small < 100 {
        for _ in 0..10 {
            m.push_audio_at(pts, Bytes::from_static(&[1, 2]), NOW)
                .expect("AAC");
            pts += 1920;
            check(&m);
        }
        m.interrupt(NOW);
        check(&m);
        small += 1;
    }
    assert!(capped && refused_plain, "the byte cap cut the window short");
    assert!(!m.window_byte_capped(), "recovered after {small} segments");
    assert_eq!(
        playlist::RuleCheck::default().check(&playlist::media(&m, "w")),
        Ok(())
    );
}

/// The review's mixed-epoch case: A/V Opus, then a reconnect as audio-only
/// Opus with 60 ms packets, which close parts early under the inherited
/// 234 ms part target. While the A/V epoch's parts are still listed, the
/// new epoch's short independent parts are legal (RFC 8216bis 4.4.4.9).
#[test]
fn audio_only_parts_after_an_av_epoch_keep_their_exemption() {
    use crate::codec::OpusConfig;
    use crate::{EsIngest, TrackSet};
    let opus = |samples: u32| -> Vec<u8> {
        match samples {
            960 => vec![31 << 3 | 4, 0xff, 0xfe],
            _ => vec![11 << 3 | 4, 1, 2],
        }
    };
    let video = crate::muxer::tests::video();
    let mut av = EsIngest::new(MuxerConfig::default());
    av.declare_tracks(TrackSet::AudioVideo, NOW).expect("A/V");
    av.opus_config(OpusConfig::STEREO).expect("stereo");
    av.parameter_sets(Some(video.sps), Some(video.pps));
    let mut rules = playlist::RuleCheck::default();
    let mut audio = 0i64;
    for frame in 0..150i64 {
        while audio <= frame * 3000 {
            av.opus_at(audio, &opus(960), NOW).expect("Opus");
            audio += 1800;
        }
        av.video(
            frame * 3000,
            frame * 3000,
            crate::codec::AccessUnit {
                data: vec![1],
                idr: frame % 30 == 0,
                ..Default::default()
            },
            NOW,
        )
        .expect("video");
        if let Some(m) = av.muxer().filter(|m| m.has_content()) {
            rules.check(&playlist::media(m, "w")).expect("A/V epoch");
        }
    }
    let mut held = av.take_muxer().expect("muxer");
    held.interrupt(NOW);
    let mut only = EsIngest::resume(MuxerConfig::default(), held);
    only.declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("audio-only");
    let mut pts = 0;
    for _ in 0..100 {
        only.opus_at(pts, &opus(2880), NOW).expect("60 ms Opus");
        pts += 5400;
        let m = only.muxer().expect("muxer");
        if let Err(e) = rules.check(&playlist::media(m, "w")) {
            panic!("{:.3} s after the switch: {e}", pts as f64 / 90_000.0);
        }
    }
}

/// What the window held once the feed was past `warm`: the fewest and most
/// entries and seconds listed, and the first rule either check broke.
#[derive(Debug)]
struct Steady {
    entries: (usize, usize),
    listed_secs: (f64, f64),
    target: u64,
    broken: Option<String>,
}

/// H.264 at 30 fps with a keyframe every `gop` frames, for `frames` frames,
/// with a retention pool: `RuleCheck` on every render and `RetentionCheck`
/// after every push.
fn keyframes_every(cfg: MuxerConfig, gop: i64, frames: i64, warm: i64) -> Steady {
    keyframes_every_at(cfg, 3000, gop, frames, warm)
}

/// As [`keyframes_every`], with frames `tick` 90 kHz ticks apart.
fn keyframes_every_at(cfg: MuxerConfig, tick: i64, gop: i64, frames: i64, warm: i64) -> Steady {
    let pool = std::sync::Arc::new(sapflow_core::fragment::FragmentQuota::new(
        crate::muxer::RETENTION_POOL_BYTES,
    ));
    let mut m = Muxer::new(cfg, crate::muxer::tests::video(), None).with_retention_pool(pool);
    let mut rules = playlist::RuleCheck::default();
    let mut retention = playlist::RetentionCheck::default();
    let mut steady = Steady {
        entries: (usize::MAX, 0),
        listed_secs: (f64::MAX, 0.0),
        target: 0,
        broken: None,
    };
    for i in 0..frames {
        m.push_video(
            i * tick,
            i * tick,
            i % gop == 0,
            Bytes::from_static(&[1; 64]),
            NOW,
        )
        .expect("video");
        if let Err(e) = retention.check(&m) {
            steady
                .broken
                .get_or_insert(format!("RetentionCheck at frame {i}: {e}"));
        }
        if !m.has_content() {
            continue;
        }
        let pl = playlist::media(&m, "w");
        if let Err(e) = rules.check(&pl) {
            steady
                .broken
                .get_or_insert(format!("RuleCheck at frame {i}: {e}"));
        }
        if i >= warm {
            let n = m.entries().len();
            let secs = listed(&pl).1 as f64 / 100_000.0;
            steady.entries = (steady.entries.0.min(n), steady.entries.1.max(n));
            steady.listed_secs = (
                steady.listed_secs.0.min(secs),
                steady.listed_secs.1.max(secs),
            );
        }
    }
    steady.target = m.target_duration();
    steady
}

fn timed(window: Option<u64>) -> MuxerConfig {
    MuxerConfig {
        window: window.map(std::time::Duration::from_secs),
        ..MuxerConfig::default()
    }
}

/// An 8 s keyframe interval: 24 segments would list 192 s. With a 48 s
/// bound the window keeps seven entries, the fewest it may list (56 s, over
/// three target durations), and both checks hold throughout.
#[test]
fn a_time_bound_holds_a_long_keyframe_interval_to_the_fewest_entries() {
    let unbounded = keyframes_every(timed(None), 240, 30 * 600, 30 * 300);
    assert_eq!(unbounded.entries, (24, 24), "{unbounded:?}");
    let bounded = keyframes_every(timed(Some(48)), 240, 30 * 600, 30 * 300);
    assert_eq!(bounded.broken, None);
    assert_eq!(bounded.entries, (7, 7), "{bounded:?}");
    assert!(
        bounded.listed_secs.0 >= (3 * bounded.target) as f64 && bounded.listed_secs.0 >= 48.0,
        "{bounded:?}"
    );
}

/// 2 s segments with a 30 s bound: the window keeps the fewest segments
/// that still last 30 s once the oldest goes, so it lists 30 s and less
/// than one segment more.
#[test]
fn a_time_bound_above_the_entry_floor_keeps_its_length() {
    let s = keyframes_every(timed(Some(30)), 60, 30 * 300, 30 * 120);
    assert_eq!(s.broken, None);
    assert_eq!(s.entries, (15, 15), "{s:?}");
    assert!(s.listed_secs.0 >= 30.0 && s.listed_secs.1 < 32.0, "{s:?}");
}

/// v4v's settings (1 s keyframes, 24 segments) last 24 s, under a 48 s
/// bound: the window is what it was without one.
#[test]
fn a_bound_longer_than_the_count_changes_nothing() {
    let without = keyframes_every(timed(None), 30, 30 * 120, 30 * 60);
    let with = keyframes_every(timed(Some(48)), 30, 30 * 120, 30 * 60);
    assert_eq!(with.broken, None);
    assert_eq!(with.entries, (24, 24), "{with:?}");
    assert_eq!(with.entries, without.entries);
    assert_eq!(with.listed_secs, without.listed_secs);
}

/// The server's default bound is 2 s a segment: keyframes every 2 s, at 30
/// fps and at 29.97 (2.002 s segments), keep the whole count.
#[test]
fn a_bound_of_two_seconds_a_segment_keeps_two_second_keyframes() {
    for tick in [3000, 3003] {
        let without = keyframes_every_at(timed(None), tick, 60, 30 * 300, 30 * 150);
        let with = keyframes_every_at(timed(Some(48)), tick, 60, 30 * 300, 30 * 150);
        assert_eq!(with.broken, None, "tick {tick}");
        assert_eq!(with.entries, (24, 24), "tick {tick}: {with:?}");
        assert_eq!(with.listed_secs, without.listed_secs, "tick {tick}");
    }
}

/// `projected_window_bytes` estimates the full window from its first
/// segments: after three, it is within a segment and a half of what the
/// window holds once full (which counts a partly filled open segment), for
/// 1 s keyframes and for 8 s ones under a 48 s bound; and it is never under
/// what is held.
#[test]
fn the_projection_estimates_the_full_window_from_its_first_segments() {
    for (cfg, gop) in [(timed(None), 30), (timed(Some(48)), 240)] {
        let mut m = Muxer::new(cfg, crate::muxer::tests::video(), None);
        let segments = |m: &Muxer| {
            m.entries()
                .iter()
                .filter(|e| matches!(e, crate::muxer::Entry::Segment(_)))
                .count()
        };
        let mut early = None;
        for i in 0..30 * 400 {
            m.push_video(
                i * 3000,
                i * 3000,
                i % gop == 0,
                Bytes::from_static(&[1; 64]),
                NOW,
            )
            .expect("video");
            if let Some(p) = m.projected_window_bytes() {
                assert!(p >= m.held_window_bytes(), "gop {gop}: under what is held");
            }
            if early.is_none() && segments(&m) == 3 {
                early = m.projected_window_bytes();
            }
        }
        let early = early.expect("three segments");
        let full = m.held_window_bytes();
        let segment = full / (segments(&m) + 1);
        assert!(
            early.abs_diff(full) <= segment * 3 / 2,
            "gop {gop}: projected {early} after three segments, full {full}, segment {segment}"
        );
    }
}
