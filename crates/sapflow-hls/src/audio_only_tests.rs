//! Independent MP4/playlist checks for explicitly declared AAC-only input.
use std::io::Cursor;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use mp4_atom::Decode;

use crate::codec::AacConfig;
use crate::fmp4::{AUDIO_TRACK, VIDEO_TRACK};
use crate::muxer::{Entry, LEADING_GAPS, Part};
use crate::{EsIngest, FlvIngest, IngestError, Muxer, MuxerConfig, TrackSet, TsIngest, playlist};

fn audio(rate: u32) -> AacConfig {
    AacConfig::from_audio_specific_config(if rate == 44_100 {
        &[0x12, 0x10]
    } else {
        &[0x11, 0x88]
    })
    .expect("AAC")
}
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}
fn stamp(frame: i64, rate: u32) -> i64 {
    frame * 1024 * 90_000 / i64::from(rate)
}
fn push(muxer: &mut Muxer, start: i64, count: i64, rate: u32) {
    for frame in start..start + count {
        muxer
            .push_audio_at(stamp(frame, rate), Bytes::from_static(&[1, 2, 3]), now())
            .expect("audio frame");
    }
}
fn audio_run(part: &Part) -> (u64, Vec<u32>) {
    let moof = mp4_atom::Moof::decode(&mut Cursor::new(&part.data[..])).expect("MP4 moof");
    assert_eq!(moof.traf.len(), 1);
    let track = &moof.traf[0];
    assert_eq!(track.tfhd.track_id, AUDIO_TRACK);
    assert_ne!(track.tfhd.track_id, VIDEO_TRACK);
    (
        track.tfdt.as_ref().expect("tfdt").base_media_decode_time,
        track.trun[0]
            .entries
            .iter()
            .map(|sample| sample.duration.expect("duration"))
            .collect(),
    )
}
fn assert_audio_window(muxer: &Muxer) -> usize {
    assert_eq!(muxer.tracks(), TrackSet::AudioOnly);
    assert!(muxer.video().is_none());
    let mut frames = 0;
    let mut previous = None;
    for entry in muxer.entries() {
        let Entry::Segment(segment) = entry else {
            continue;
        };
        assert!(segment.duration.as_secs_f64().round() <= muxer.target_duration() as f64);
        for (index, part) in segment.parts.iter().enumerate() {
            assert!(part.independent);
            assert!(part.duration <= muxer.part_target());
            if index + 1 < segment.parts.len() {
                assert!(part.duration.as_secs_f64() >= muxer.part_target().as_secs_f64() * 0.85);
            }
            let (base, durations) = audio_run(part);
            if let Some(end) = previous {
                assert_eq!(base, end);
            }
            assert!(durations.iter().all(|duration| *duration == 1024));
            frames += durations.len();
            previous = Some(base + durations.iter().map(|n| u64::from(*n)).sum::<u64>());
        }
    }
    frames
}

#[test]
fn declaration_or_aac_header_alone_never_announces_media() {
    let mut legacy = FlvIngest::new(MuxerConfig::default());
    legacy
        .audio_at(0, &[0xaf, 0, 0x11, 0x88], now())
        .expect("valid canonical AAC input");
    for frame in 0..200 {
        legacy
            .audio_at((frame * 1024 * 1000 / 48000) as u32, &[0xaf, 1, 1], now())
            .expect("valid canonical AAC input");
    }
    assert!(
        legacy.muxer().is_none(),
        "early AAC cannot infer absent video"
    );
    let mut explicit = FlvIngest::new(MuxerConfig::default());
    assert!(
        !explicit
            .declare_tracks(TrackSet::AudioOnly, now())
            .expect("valid immutable topology declaration")
            .any()
    );
    assert!(
        !explicit
            .audio_at(0, &[0xaf, 0, 0x11, 0x88], now())
            .expect("valid canonical AAC input")
            .any()
    );
    assert!(explicit.muxer().is_none());
    let mut changed = false;
    for frame in 0..200 {
        changed |= explicit
            .audio_at((frame * 1024 * 1000 / 48000) as u32, &[0xaf, 1, 1], now())
            .expect("valid canonical AAC input")
            .any();
    }
    assert!(
        changed
            && explicit
                .muxer()
                .expect("accepted AAC media creates a muxer")
                .has_content()
    );
}

#[test]
fn audio_only_has_no_video_boxes_and_exact_frame_durations_at_both_rates() {
    for rate in [48_000, 44_100] {
        let mut muxer = Muxer::audio_only(
            MuxerConfig {
                segment_count: 1000,
                ..MuxerConfig::default()
            },
            audio(rate),
        )
        .expect("supported AAC-only muxer configuration");
        push(&mut muxer, 0, 10_000, rate);
        let before = muxer.next_part_id();
        muxer.interrupt(now()); // Known AAC duration closes the standalone tail.
        assert!(muxer.next_part_id() >= before);
        assert_eq!(assert_audio_window(&muxer), 10_000);
        // Parse through arbitrary top-level ftyp size instead of trusting the offset.
        let bytes = muxer.init();
        let ftyp_len = u32::from_be_bytes(
            bytes[..4]
                .try_into()
                .expect("MP4 size header contains four bytes"),
        ) as usize;
        let moov = mp4_atom::Moov::decode(&mut Cursor::new(&bytes[ftyp_len..]))
            .expect("audio-only MP4 initialization decodes");
        assert_eq!(moov.trak.len(), 1);
        assert_eq!(moov.trak[0].tkhd.track_id, AUDIO_TRACK);
        let last = muxer.entries().back().expect("completed segment exists");
        let Entry::Segment(last) = last else {
            panic!("last segment")
        };
        let (base, frames) = audio_run(last.parts.last().expect("completed part exists"));
        assert_eq!(base + frames.len() as u64 * 1024, 10_240_000);
        let master = playlist::multivariant(&muxer);
        assert!(master.contains("CODECS=\"mp4a.40.2\""));
        assert!(
            !master.contains("avc1")
                && !master.contains("FRAME-RATE")
                && !master.contains("RESOLUTION")
        );
        playlist::RuleCheck::default()
            .check(&playlist::media(&muxer, "abc"))
            .expect("generated playlist obeys fixed HLS timing");
    }
}

#[test]
fn first_single_aac_frame_can_flush_without_a_next_packet_or_idr() {
    let mut muxer = Muxer::audio_only(MuxerConfig::default(), audio(48_000))
        .expect("supported AAC-only muxer configuration");
    push(&mut muxer, 0, 1, 48_000);
    assert!(!muxer.has_content());
    assert!(muxer.interrupt(now()).segment);
    assert_eq!(assert_audio_window(&muxer), 1);
    assert_eq!(
        muxer
            .segment(LEADING_GAPS)
            .expect("retained segment remains available")
            .parts[0]
            .duration,
        Duration::from_nanos(21_333_333)
    );
    assert!(!muxer.interrupt(now()).any());
}

#[test]
fn topology_and_codec_are_frozen_per_connection() {
    let mut ingest = EsIngest::new(MuxerConfig::default());
    ingest
        .declare_tracks(TrackSet::AudioOnly, now())
        .expect("valid immutable topology declaration");
    ingest
        .audio_at(0, audio(48_000), &[1], now())
        .expect("valid canonical AAC input");
    assert_eq!(
        ingest
            .declare_tracks(TrackSet::AudioVideo, now())
            .expect_err("contradictory topology must be rejected"),
        IngestError::TrackConflict
    );
    assert_eq!(
        ingest
            .audio_at(1920, audio(44_100), &[1], now())
            .expect_err("changed AAC configuration must be rejected"),
        IngestError::InvalidAudio
    );
    let mut video = EsIngest::new(MuxerConfig::default());
    video
        .declare_tracks(TrackSet::VideoOnly, now())
        .expect("valid immutable topology declaration");
    assert_eq!(
        video
            .audio_at(0, audio(48_000), &[1], now())
            .expect_err("audio on a video-only declaration must be rejected"),
        IngestError::TrackConflict
    );
}

#[test]
fn reconnect_keeps_ids_targets_maps_and_rebases_an_aac_clock_reset() {
    let cfg = MuxerConfig {
        segment_count: 100,
        ..MuxerConfig::default()
    };
    let mut first = EsIngest::new(cfg);
    first
        .declare_tracks(TrackSet::AudioOnly, now())
        .expect("valid immutable topology declaration");
    for frame in 0..150 {
        first
            .audio_at(stamp(frame, 48_000), audio(48_000), &[1], now())
            .expect("valid canonical AAC input");
    }
    let mut muxer = first
        .take_muxer()
        .expect("accepted connection retains its muxer");
    muxer.interrupt(now());
    let old = muxer
        .segment(LEADING_GAPS)
        .expect("retained segment remains available");
    let bytes = old.data.clone();
    let targets = (muxer.part_target(), muxer.target_duration());
    let floor = muxer.next_part_id();
    let mut second = EsIngest::resume(cfg, muxer);
    second
        .declare_tracks(TrackSet::AudioOnly, now())
        .expect("valid immutable topology declaration");
    assert!(
        second
            .muxer()
            .expect("accepted AAC media creates a muxer")
            .open_parts()
            .is_empty()
    );
    for frame in 0..150 {
        second
            .audio_at(
                stamp(frame, 48_000),
                audio(48_000),
                &[2],
                now() + Duration::from_secs(5),
            )
            .expect("valid canonical AAC input");
    }
    let muxer = second.muxer_mut().expect("current AAC muxer exists");
    muxer.interrupt(now());
    assert_eq!(targets, (muxer.part_target(), muxer.target_duration()));
    assert_eq!(
        muxer
            .segment(LEADING_GAPS)
            .expect("retained segment remains available")
            .data,
        bytes
    );
    assert_eq!(muxer.current_initialization_id(), 0);
    assert!(muxer.next_part_id() > floor);
    assert_eq!(assert_audio_window(muxer), 300);
    assert!(
        muxer
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Segment(s) if s.discontinuity))
    );
    assert!(
        old.initialization.data
            == *muxer
                .initialization(0)
                .expect("retained initialization remains available")
    );
}

#[test]
fn reconfiguration_retains_old_init_and_never_advertises_an_empty_epoch() {
    let cfg = MuxerConfig {
        segment_count: 100,
        ..MuxerConfig::default()
    };
    let mut muxer =
        Muxer::audio_only(cfg, audio(48_000)).expect("supported AAC-only muxer configuration");
    push(&mut muxer, 0, 100, 48_000);
    muxer.interrupt(now());
    let old = muxer
        .segment(LEADING_GAPS)
        .expect("retained segment remains available");
    let old_init = old.initialization.clone();
    muxer
        .reconfigure_tracks(
            Some(crate::muxer::tests::video()),
            Some(audio(48_000)),
            now(),
        )
        .expect("valid retained codec-epoch transition");
    assert_eq!(muxer.tracks(), TrackSet::AudioVideo);
    let empty_epoch = muxer.current_initialization_id();
    assert!(
        !playlist::media(&muxer, "abc")
            .contains(&format!("#EXT-X-MAP:URI=\"abc_init{empty_epoch}.mp4\""))
    );
    muxer
        .reconfigure_tracks(None, Some(audio(44_100)), now())
        .expect("valid retained codec-epoch transition");
    push(&mut muxer, 0, 100, 44_100);
    muxer.interrupt(now());
    assert_eq!(muxer.tracks(), TrackSet::AudioOnly);
    assert_eq!(
        old_init.data,
        *muxer
            .initialization(0)
            .expect("retained initialization remains available")
    );
    assert!(playlist::media(&muxer, "abc").contains("abc_init2.mp4"));
    assert!(muxer.next_part_id() > old.parts.last().expect("completed part exists").id);
}

#[test]
fn actual_pmt_audio_only_fixtures_make_current_completed_independent_parts() {
    for (fixture, rate, channels) in [
        (
            &include_bytes!("../tests/fixtures/audio-48k-mono.ts")[..],
            48_000,
            1,
        ),
        (
            &include_bytes!("../tests/fixtures/audio-44k1-stereo.ts")[..],
            44_100,
            2,
        ),
    ] {
        let mut ingest = TsIngest::new(MuxerConfig::default());
        let mut changed = false;
        for bytes in fixture.chunks(1316) {
            changed |= ingest
                .push(bytes, now())
                .expect("generated PMT and AAC input is valid")
                .any();
        }
        let muxer = ingest.muxer_mut().expect("PMT+AAC creates muxer");
        muxer.interrupt(now());
        assert!(changed && muxer.has_content());
        assert_eq!(
            muxer
                .audio()
                .expect("declared AAC configuration exists")
                .sample_rate,
            rate
        );
        assert_eq!(
            muxer
                .audio()
                .expect("declared AAC configuration exists")
                .channels,
            channels
        );
        assert!(assert_audio_window(muxer) > 200);
        playlist::RuleCheck::default()
            .check(&playlist::media(muxer, "abc"))
            .expect("generated playlist obeys fixed HLS timing");
    }
}

#[test]
fn real_av_audio_av_epochs_preserve_retained_parts_and_fixed_targets() {
    let cfg = MuxerConfig {
        segment_count: 100,
        ..MuxerConfig::default()
    };
    let mut muxer = Muxer::new(cfg, crate::muxer::tests::video(), Some(audio(48_000)));
    crate::muxer::tests::feed(&mut muxer, 0..91, 30);
    muxer.interrupt(now());
    let old_segment = muxer
        .segment(LEADING_GAPS)
        .expect("retained segment remains available");
    let old_bytes = old_segment.data.clone();
    let targets = (muxer.part_target(), muxer.target_duration());
    muxer
        .reconfigure_tracks(None, Some(audio(44_100)), now())
        .expect("valid retained codec-epoch transition");
    push(&mut muxer, 0, 150, 44_100);
    muxer.interrupt(now());
    let audio_epoch = muxer.current_initialization_id();
    assert!(muxer.entries().iter().any(|entry| matches!(entry, Entry::Segment(s) if s.initialization.id == audio_epoch && s.parts.iter().all(|part| part.independent))));
    muxer
        .reconfigure_tracks(
            Some(crate::muxer::tests::video()),
            Some(audio(48_000)),
            now(),
        )
        .expect("valid retained codec-epoch transition");
    crate::muxer::tests::feed(&mut muxer, 0..91, 30);
    muxer.interrupt(now());
    assert_eq!(targets, (muxer.part_target(), muxer.target_duration()));
    assert_eq!(
        muxer
            .segment(LEADING_GAPS)
            .expect("retained segment remains available")
            .data,
        old_bytes
    );
    assert!(muxer.initialization(audio_epoch).is_some());
    assert_eq!(muxer.tracks(), TrackSet::AudioVideo);
    let playlist = playlist::media(&muxer, "abc");
    assert!(playlist.contains("abc_init1.mp4") && playlist.contains("abc_init2.mp4"));
    playlist::RuleCheck::default()
        .check(&playlist)
        .expect("generated playlist obeys fixed HLS timing");
}

#[test]
fn unsupported_audio_and_empty_tracks_are_rejected_without_mutating_epoch() {
    let mut config = audio(48_000);
    config.sample_rate = 0;
    assert!(Muxer::audio_only(MuxerConfig::default(), config).is_err());
    let mut config = audio(48_000);
    config.object_type = 5;
    assert!(Muxer::audio_only(MuxerConfig::default(), config).is_err());
    let mut muxer = Muxer::audio_only(MuxerConfig::default(), audio(48_000))
        .expect("supported AAC-only muxer configuration");
    assert!(muxer.reconfigure_tracks(None, None, now()).is_err());
    assert_eq!(muxer.current_initialization_id(), 0);
    assert_eq!(muxer.tracks(), TrackSet::AudioOnly);
}

#[test]
fn explicit_audio_only_rejects_unsupported_raw_asc_before_state_mutation() {
    for asc in [
        &[0x11, 0x8c][..],
        &[0x11, 0x8a],
        &[0x11, 0x89],
        &[0x11, 0x88, 0],
    ] {
        let mut ingest = FlvIngest::new(MuxerConfig::default());
        ingest
            .declare_tracks(TrackSet::AudioOnly, now())
            .expect("valid immutable topology declaration");
        let mut header = vec![0xaf, 0];
        header.extend_from_slice(asc);
        assert_eq!(
            ingest.audio_at(0, &header, now()),
            Err(IngestError::InvalidAudio)
        );
        assert!(ingest.muxer().is_none());
        // A rejected header cannot poison the first actual accepted config.
        assert!(
            !ingest
                .audio_at(0, &[0xaf, 0, 0x12, 0x10], now())
                .expect("valid canonical AAC input")
                .any()
        );
        ingest
            .audio_at(0, &[0xaf, 1, 1], now())
            .expect("valid canonical AAC input");
        assert_eq!(
            ingest
                .muxer()
                .expect("accepted AAC media creates a muxer")
                .audio()
                .expect("declared AAC configuration exists")
                .sample_rate,
            44_100
        );
    }
}

#[test]
fn a_late_audio_only_declaration_cannot_hide_prior_unsupported_asc_flags() {
    for legacy in [false, true] {
        let mut ingest = FlvIngest::new(MuxerConfig::default());
        if legacy {
            ingest.audio(0, &[0xaf, 0, 0x11, 0x8c]);
        } else {
            ingest
                .audio_at(0, &[0xaf, 0, 0x11, 0x8c], now())
                .expect("legacy AV header policy remains unchanged");
        }
        assert_eq!(
            ingest.declare_tracks(TrackSet::AudioOnly, now()),
            Err(IngestError::InvalidAudio)
        );
        assert!(ingest.muxer().is_none());
        // Legacy AV declaration policy is deliberately unchanged.
        ingest
            .declare_tracks(TrackSet::AudioVideo, now())
            .expect("valid immutable topology declaration");
    }
}

#[test]
fn explicit_audio_only_freezes_first_asc_even_before_media_but_reconnect_can_change_it() {
    let cfg = MuxerConfig::default();
    let mut ingest = FlvIngest::new(cfg);
    ingest
        .declare_tracks(TrackSet::AudioOnly, now())
        .expect("valid immutable topology declaration");
    ingest
        .audio_at(0, &[0xaf, 0, 0x11, 0x88], now())
        .expect("valid canonical AAC input");
    assert!(
        !ingest
            .audio_at(0, &[0xaf, 0, 0x11, 0x88], now())
            .expect("valid canonical AAC input")
            .any()
    );
    assert_eq!(
        ingest.audio_at(0, &[0xaf, 0, 0x12, 0x10], now()),
        Err(IngestError::InvalidAudio)
    );
    assert!(ingest.muxer().is_none());
    for frame in 0..100 {
        ingest
            .audio_at((frame * 1024 * 1000 / 48000) as u32, &[0xaf, 1, 1], now())
            .expect("valid canonical AAC input");
    }
    let mut retained = ingest
        .take_muxer()
        .expect("accepted connection retains its muxer");
    retained.interrupt(now());
    assert_eq!(
        retained
            .audio()
            .expect("declared AAC configuration exists")
            .sample_rate,
        48_000
    );
    let mut replacement = FlvIngest::resume(cfg, retained);
    replacement
        .declare_tracks(TrackSet::AudioOnly, now())
        .expect("valid immutable topology declaration");
    replacement
        .audio_at(0, &[0xaf, 0, 0x12, 0x10], now())
        .expect("valid canonical AAC input");
    assert!(
        replacement
            .audio_at(0, &[0xaf, 1, 2], now())
            .expect("valid canonical AAC input")
            .restarted
    );
    assert_eq!(
        replacement
            .muxer()
            .expect("accepted AAC media creates a muxer")
            .audio()
            .expect("declared AAC configuration exists")
            .sample_rate,
        44_100
    );
    assert_eq!(
        replacement
            .muxer()
            .expect("accepted AAC media creates a muxer")
            .current_initialization_id(),
        1
    );
    // The typed elementary seam enforces the same first-config rule.
    let mut elementary = EsIngest::new(cfg);
    elementary
        .declare_tracks(TrackSet::AudioOnly, now())
        .expect("valid immutable topology declaration");
    elementary
        .audio_at(0, audio(48_000), &[], now())
        .expect("valid canonical AAC input");
    assert_eq!(
        elementary.audio_at(0, audio(44_100), &[], now()),
        Err(IngestError::InvalidAudio)
    );
}

#[test]
fn missing_or_repeated_aac_timestamps_report_loss_but_explicit_replacement_does_not() {
    let mut muxer = Muxer::audio_only(MuxerConfig::default(), audio(48_000))
        .expect("supported AAC-only muxer configuration");
    push(&mut muxer, 0, 100, 48_000);
    let missing = muxer
        .push_audio_at(stamp(101, 48_000), Bytes::from_static(&[1]), now())
        .expect("valid canonical AAC input");
    assert!(missing.media_loss);
    let clean = muxer
        .push_audio_at(stamp(102, 48_000), Bytes::from_static(&[1]), now())
        .expect("valid canonical AAC input");
    assert!(!clean.media_loss);
    let repeated = muxer
        .push_audio_at(stamp(102, 48_000), Bytes::from_static(&[1]), now())
        .expect("valid canonical AAC input");
    assert!(repeated.media_loss);
    let mut merged = crate::Update::default();
    merged.merge(missing);
    merged.merge(clean);
    assert!(merged.media_loss && merged.any());
    assert!(!muxer.interrupt(now()).media_loss);
    assert!(
        !muxer
            .push_audio_at(0, Bytes::from_static(&[1]), now())
            .expect("valid canonical AAC input")
            .media_loss
    );
    assert!(
        !muxer
            .reconfigure_tracks(None, Some(audio(44_100)), now())
            .expect("valid retained codec-epoch transition")
            .media_loss
    );
}

#[test]
fn audio_pdt_is_media_anchored_under_jitter_and_reanchors_real_breaks_without_overlap() {
    let cfg = MuxerConfig {
        segment_count: 100,
        ..MuxerConfig::default()
    };
    let mut muxer =
        Muxer::audio_only(cfg, audio(44_100)).expect("supported AAC-only muxer configuration");
    let wall = now();
    for frame in 0..300 {
        let arrival =
            wall + Duration::from_millis((frame * 1024 * 1000 / 44100 + frame % 53) as u64);
        muxer
            .push_audio_at(stamp(frame, 44_100), Bytes::from_static(&[1]), arrival)
            .expect("valid canonical AAC input");
    }
    let segments: Vec<_> = muxer
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Segment(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(segments.len() >= 5);
    for pair in segments.windows(2) {
        // Independent integer-nanosecond conversions can leave one ns,
        // never arrival jitter or accumulated rounded-frame drift.
        let end = pair[0].start + pair[0].duration;
        assert!(pair[1].start >= end);
        assert!(
            pair[1]
                .start
                .duration_since(end)
                .expect("next PDT does not overlap the preceding segment")
                <= Duration::from_nanos(1)
        );
    }
    muxer.interrupt(wall + Duration::from_secs(7));
    let prior_end = match muxer.entries().back().expect("completed segment exists") {
        Entry::Segment(s) => s.start + s.duration,
        _ => unreachable!(),
    };
    let resumed_floor = muxer.next_part_id();
    for frame in 0..50 {
        muxer
            .push_audio_at(
                stamp(frame, 44_100),
                Bytes::from_static(&[2]),
                wall + Duration::from_secs(20),
            )
            .expect("valid canonical AAC input");
    }
    muxer.interrupt(wall + Duration::from_secs(22));
    let new = muxer
        .entries()
        .iter()
        .find_map(|entry| match entry {
            Entry::Segment(s) if s.parts.first().is_some_and(|part| part.id >= resumed_floor) => {
                Some(s.clone())
            }
            _ => None,
        })
        .expect("resumed part floor identifies its first segment");
    assert!(
        new.discontinuity && new.start == wall + Duration::from_secs(20) && new.start >= prior_end
    );
    let end = match muxer.entries().back().expect("completed segment exists") {
        Entry::Segment(s) => s.start + s.duration,
        _ => unreachable!(),
    };
    let rollback_floor = muxer.next_part_id();
    for frame in 0..50 {
        muxer
            .push_audio_at(stamp(frame, 44_100), Bytes::from_static(&[3]), wall)
            .expect("valid canonical AAC input");
    }
    muxer.interrupt(wall);
    let rollback = muxer
        .entries()
        .iter()
        .find_map(|entry| match entry {
            Entry::Segment(s)
                if s.parts
                    .first()
                    .is_some_and(|part| part.id >= rollback_floor) =>
            {
                Some(s)
            }
            _ => None,
        })
        .expect("resumed part floor identifies its first segment");
    assert!(rollback.discontinuity && rollback.start >= end);
}
