//! Additive admission-policy regressions using APIs present before the fix.
//! Opaque short samples exercise resource accounting, not media decoding.
use super::*;
use crate::codec::AccessUnit;
use crate::{EsIngest, FlvIngest, IngestError};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;
const LIVE_AAC_LIMIT: usize = 16_384;

fn muxer() -> Muxer {
    let mut muxer = Muxer::new(MuxerConfig::default(), tests::video(), Some(tests::aac()));
    muxer
        .push_video(0, 0, true, Bytes::from_static(&[1]), NOW)
        .expect("IDR");
    muxer
        .push_video(3000, 3000, false, Bytes::from_static(&[2]), NOW)
        .expect("successor");
    muxer
}

fn state(muxer: &Muxer) -> (usize, usize, Option<(i64, Bytes)>, usize, usize) {
    let part = muxer.part.as_ref();
    let pending = muxer.pending_audio.as_ref();
    (
        part.map_or(0, |p| p.audio.len()) + usize::from(pending.is_some()),
        part.map_or(0, |p| p.audio.iter().map(|a| a.data.len()).sum::<usize>())
            + pending.map_or(0, |p| p.data.len()),
        pending.map(|p| (p.dts, p.data.clone())),
        part.map_or(0, |p| p.audio_times.len()),
        muxer.next_segment.as_ref().map_or(0, |s| s.bytes),
    )
}

#[test]
fn checked_aac_accepts_exact_payload_reservation_and_refuses_one_more() {
    let mut muxer = muxer();
    // Existing counter seam avoids allocating a 64 MiB fixture. Two one-byte
    // AAC frames exactly exhaust the remaining allowance, including pending.
    muxer.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES - 2;
    muxer
        .push_audio_at(0, Bytes::from_static(&[1]), NOW)
        .expect("one reserved byte");
    muxer
        .push_audio_at(1920, Bytes::from_static(&[2]), NOW)
        .expect("exact two bytes");
    assert_eq!(state(&muxer).0, 2);
    assert_eq!(state(&muxer).1, 2);
    let before = state(&muxer);
    assert!(matches!(
        muxer.push_audio_at(3840, Bytes::from_static(&[3]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(state(&muxer), before, "refusal preserves all accepted AAC");
}

#[test]
fn checked_aac_live_count_includes_pending_and_equal_pts() {
    let mut muxer = muxer();
    for _ in 0..LIVE_AAC_LIMIT {
        muxer
            .push_audio_at(0, Bytes::from_static(&[1]), NOW)
            .expect("within live count");
    }
    let before = state(&muxer);
    assert_eq!((before.0, before.1), (LIVE_AAC_LIMIT, LIVE_AAC_LIMIT));
    assert!(
        muxer
            .part
            .as_ref()
            .expect("part")
            .audio
            .iter()
            .all(|a| a.duration == 0)
    );
    assert!(matches!(
        muxer.push_audio_at(0, Bytes::from_static(&[1]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(state(&muxer), before);
}

#[test]
fn checked_aac_live_count_also_bounds_advancing_pts() {
    let mut muxer = muxer();
    for i in 0..LIVE_AAC_LIMIT {
        muxer
            .push_audio_at(i as i64 * 1920, Bytes::from_static(&[1]), NOW)
            .expect("within live count");
    }
    let before = state(&muxer);
    assert!(matches!(
        muxer.push_audio_at(LIVE_AAC_LIMIT as i64 * 1920, Bytes::from_static(&[1]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(state(&muxer), before);
}

#[test]
fn refused_aac_does_not_replace_pending_or_mutate_accepted_part() {
    let mut muxer = muxer();
    muxer
        .push_audio_at(0, Bytes::from_static(&[1, 2]), NOW)
        .expect("first AAC");
    muxer
        .push_audio_at(1920, Bytes::from_static(&[3]), NOW)
        .expect("second AAC");
    muxer.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES;
    let before = state(&muxer);
    assert!(matches!(
        muxer.push_audio_at(3840, Bytes::from_static(&[4]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(state(&muxer), before);
}

#[test]
fn video_admission_reserves_staged_and_pending_aac() {
    let mut muxer = muxer();
    muxer.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES - 2;
    muxer
        .push_audio_at(0, Bytes::from_static(&[1]), NOW)
        .expect("pending AAC");
    muxer
        .push_audio_at(1920, Bytes::from_static(&[2]), NOW)
        .expect("staged plus pending AAC");
    let before = state(&muxer);
    let pending_video = muxer.pending_video.as_ref().expect("pending video").dts;
    assert!(matches!(
        muxer.push_video(6000, 6000, false, Bytes::from_static(&[3]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(state(&muxer), before);
    assert_eq!(
        muxer
            .pending_video
            .as_ref()
            .expect("accepted pending video")
            .dts,
        pending_video
    );
}

#[test]
fn segment_rotation_transfers_carried_aac_before_next_video_charge() {
    let mut muxer = Muxer::new(MuxerConfig::default(), tests::video(), Some(tests::aac()));
    tests::feed(&mut muxer, 0..32, 30);
    assert!(muxer.timing.is_some());
    muxer
        .push_audio_at(200_000, Bytes::from_static(&[1; 5]), NOW)
        .expect("future AAC pending");
    muxer
        .push_audio_at(201_920, Bytes::from_static(&[2; 7]), NOW)
        .expect("future AAC staged");
    for frame in 32..=60 {
        muxer
            .push_video(
                frame * 3000,
                frame * 3000,
                frame % 30 == 0,
                Bytes::from_static(&[1]),
                NOW,
            )
            .expect("normal segment rotation");
    }
    let part = muxer.part.as_ref().expect("new open part");
    assert!(part.audio.iter().any(|sample| sample.data.len() == 5));
    let held_aac = part
        .audio
        .iter()
        .map(|sample| sample.data.len())
        .sum::<usize>()
        + muxer
            .pending_audio
            .as_ref()
            .expect("pending AAC")
            .data
            .len();
    assert!(held_aac >= 12);
    muxer.next_segment.as_mut().expect("new segment").bytes = MAX_SEGMENT_BYTES - held_aac;
    let before = state(&muxer);
    assert!(matches!(
        muxer.push_video(183_000, 183_000, false, Bytes::from_static(&[1]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(state(&muxer), before);
}

#[test]
fn refused_audio_keeps_accepted_prefix_available_for_interrupt_and_idr_resume() {
    let mut muxer = Muxer::new(MuxerConfig::default(), tests::video(), Some(tests::aac()));
    tests::feed(&mut muxer, 0..32, 30);
    let prior_parts = muxer.parts.len();
    muxer.next_segment.as_mut().expect("segment").bytes = MAX_SEGMENT_BYTES;
    assert!(matches!(
        muxer.push_audio_at(96_000, Bytes::from_static(&[1]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert!(muxer.parts.len() >= prior_parts);
    let interrupted = muxer.interrupt(NOW);
    assert!(
        interrupted.part && interrupted.segment,
        "existing accepted-prefix sealing"
    );
    muxer
        .push_video(900_000, 900_000, true, Bytes::from_static(&[1]), NOW)
        .expect("fresh IDR");
    muxer
        .push_audio_at(900_000, Bytes::from_static(&[1]), NOW)
        .expect("resumed AAC");
    muxer
        .push_audio_at(901_920, Bytes::from_static(&[2]), NOW)
        .expect("resumed AAC successor");
    muxer
        .push_video(903_000, 903_000, false, Bytes::from_static(&[2]), NOW)
        .expect("real video successor");
    assert!(muxer.open_discontinuity());
    assert!(state(&muxer).0 <= 2);
}

#[test]
fn elementary_updating_api_propagates_aac_admission_refusal() {
    let mut ingest = EsIngest::new(MuxerConfig::default());
    let config = tests::video();
    ingest.parameter_sets(Some(config.sps), Some(config.pps));
    ingest.audio_config(tests::aac());
    for (dts, idr) in [(0, true), (3000, false)] {
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
    ingest
        .muxer_mut()
        .expect("muxer")
        .next_segment
        .as_mut()
        .expect("segment")
        .bytes = MAX_SEGMENT_BYTES;
    assert_eq!(
        ingest.audio_at(0, tests::aac(), &[1], NOW),
        Err(IngestError::SegmentTooLarge)
    );
}

#[test]
fn flv_updating_api_propagates_aac_admission_refusal() {
    let mut ingest = FlvIngest::new(MuxerConfig::default());
    let config = tests::video();
    let mut sequence = vec![
        0x17,
        0,
        0,
        0,
        0,
        1,
        config.sps[1],
        config.sps[2],
        config.sps[3],
        0xff,
        0xe1,
    ];
    sequence.extend_from_slice(&(config.sps.len() as u16).to_be_bytes());
    sequence.extend_from_slice(&config.sps);
    sequence.push(1);
    sequence.extend_from_slice(&(config.pps.len() as u16).to_be_bytes());
    sequence.extend_from_slice(&config.pps);
    let mut aac = vec![0xaf, 0];
    aac.extend_from_slice(&tests::aac().audio_specific_config());
    ingest.audio_at(0, &aac, NOW).expect("AAC configuration");
    ingest.video(0, &sequence, NOW).expect("AVC configuration");
    ingest
        .video(0, &[0x17, 1, 0, 0, 0, 0, 0, 0, 1, 0x65], NOW)
        .expect("IDR");
    ingest
        .video(33, &[0x27, 1, 0, 0, 0, 0, 0, 0, 1, 0x41], NOW)
        .expect("successor");
    ingest
        .muxer_mut()
        .expect("muxer")
        .next_segment
        .as_mut()
        .expect("segment")
        .bytes = MAX_SEGMENT_BYTES;
    assert_eq!(
        ingest.audio_at(0, &[0xaf, 1, 1], NOW),
        Err(IngestError::SegmentTooLarge)
    );
}

#[test]
fn video_led_zero_sample_rate_returns_error_before_retaining_audio() {
    let mut audio = tests::aac();
    audio.sample_rate = 0;
    let mut muxer = Muxer::new(MuxerConfig::default(), tests::video(), Some(audio));
    assert!(matches!(
        muxer.push_audio_at(0, Bytes::from_static(&[1]), NOW),
        Err(MuxError::InvalidAudio)
    ));
    assert!(muxer.pending_audio.is_none());
}

#[test]
fn elementary_audio_timestamp_subtraction_returns_error_on_overflow() {
    let mut ingest = EsIngest::new(MuxerConfig::default());
    let config = tests::video();
    ingest.parameter_sets(Some(config.sps), Some(config.pps));
    ingest.audio_config(tests::aac());
    ingest
        .video(
            1,
            1,
            AccessUnit {
                data: vec![1],
                idr: true,
                ..AccessUnit::default()
            },
            NOW,
        )
        .expect("established positive origin");
    assert_eq!(
        ingest.audio_at(i64::MIN, tests::aac(), &[1], NOW),
        Err(IngestError::InvalidTimestamp)
    );
    assert!(ingest.muxer().expect("muxer").pending_audio.is_none());
}
