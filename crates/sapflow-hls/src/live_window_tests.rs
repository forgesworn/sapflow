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
    for _ in 0..120 {
        m.push_audio_at(pts, payload.clone(), NOW).expect("AAC");
        pts += 1920;
        m.interrupt(NOW);
        if m.has_content() {
            let pl = playlist::media(&m, "w");
            signalled
                .check_with_byte_cap(&pl, m.window_byte_capped())
                .expect("with the muxer's signal");
            refused_plain |= plain.check(&pl).is_err();
            capped |= m.window_byte_capped();
        }
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
    assert!(capped && refused_plain, "the byte cap cut the window short");
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
