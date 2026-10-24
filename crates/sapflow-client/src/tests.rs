use std::sync::Arc;

use crate::media::*;
use crate::queue::*;
use crate::session::*;

fn video_config() -> Arc<CodecConfig> {
    Arc::new(CodecConfig::H264(
        H264Config::new(
            &[
                0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0,
                0, 3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
            ],
            &[0x68, 0xef, 0xbc, 0x80],
        )
        .expect("fixture SPS/PPS"),
    ))
}
fn audio_config() -> Arc<CodecConfig> {
    Arc::new(CodecConfig::Aac(
        AacConfig::new(&[0x11, 0x90]).expect("AAC-LC"),
    ))
}
fn timing(n: i64) -> SampleTiming {
    SampleTiming::new(n, n, 1, 30).expect("video timing")
}
fn video_with(config: Arc<CodecConfig>, frame: i64, key: bool) -> SharedSample {
    Arc::new(
        EncodedSample::new(
            config,
            timing(frame),
            key,
            Arc::from([0, 0, 0, 2, if key { 0x65 } else { 0x41 }, 0x80]),
        )
        .expect("framed fixture"),
    )
}
fn video(frame: i64, key: bool) -> SharedSample {
    video_with(video_config(), frame, key)
}
fn audio(n: i64) -> SharedSample {
    Arc::new(
        EncodedSample::new(
            audio_config(),
            SampleTiming::new(n, n, 1024, 48000).expect("audio time"),
            false,
            Arc::from([0x21, 0x10]),
        )
        .expect("audio unit"),
    )
}

fn mono_timing_source(config: Arc<CodecConfig>, tick: i64) -> SharedSample {
    Arc::new(
        EncodedSample::new(
            config,
            SampleTiming::new(tick, tick, 1024, 48_000).expect("original coded timing"),
            false,
            Arc::from([0x21, 0x10]),
        )
        .expect("original ownership copy"),
    )
}
fn mono_timing_config() -> Arc<CodecConfig> {
    Arc::new(CodecConfig::Aac(
        AacConfig::new(&[0x11, 0x88]).expect("AAC mono"),
    ))
}

#[test]
fn aac_timing_views_share_exact_original_payload_codec_and_depth_one_provenance() {
    let original = mono_timing_source(mono_timing_config(), 4096);
    let views = AacSampleTimingViews::new(original.clone(), 17, -2112, Some(0))
        .expect("explicit timing choices");
    let recording = views.recording();
    let publishing = views.publishing().expect("selected publisher");
    assert_eq!(recording.timing(), original.timing());
    assert_eq!(publishing.timing().pts(), 0);
    assert_eq!(original.timing().pts(), 4096);
    assert!(original.aac_source().is_none());
    for sample in [recording, publishing] {
        assert!(Arc::ptr_eq(sample.payload(), original.payload()));
        assert!(Arc::ptr_eq(sample.config(), original.config()));
        let source = sample.aac_source().expect("retained source context");
        assert!(Arc::ptr_eq(source.original(), &original));
        assert!(source.original().aac_source().is_none());
        assert_eq!(source.producer_generation(), 17);
        assert_eq!(source.native_pts_us(), -2112);
    }
    assert!(matches!(
        AacSampleTimingViews::new(recording.clone(), 17, 0, Some(0)),
        Err(MediaError::InvalidAudioTiming)
    ));
}

#[test]
fn aac_timing_views_keep_payload_alive_after_original_and_pair_handles_drop() {
    let original = mono_timing_source(mono_timing_config(), 0);
    let weak = Arc::downgrade(original.payload());
    let views = AacSampleTimingViews::new(original.clone(), 1, i64::MIN, Some(10_000))
        .expect("signed source observation is not a media timestamp");
    let recording = views.recording().clone();
    drop(original);
    drop(views);
    assert!(weak.upgrade().is_some());
    assert_eq!(
        recording.aac_source().expect("source").native_pts_us(),
        i64::MIN
    );
    drop(recording);
    assert!(weak.upgrade().is_none());
}

#[test]
fn aac_timing_views_reject_unsupported_profiles_context_and_scalar_overflow() {
    for source in [audio(0), video(0, true)] {
        assert!(matches!(
            AacSampleTimingViews::new(source, 1, 0, Some(0)),
            Err(MediaError::InvalidAudioTiming)
        ));
    }
    let rate = Arc::new(CodecConfig::Aac(
        AacConfig::new(&[0x12, 0x08]).expect("44100 mono"),
    ));
    let source = Arc::new(
        EncodedSample::new(
            rate,
            SampleTiming::new(0, 0, 1024, 44_100).expect("44100 timing"),
            false,
            Arc::from([1, 2]),
        )
        .expect("44100 original"),
    );
    assert!(AacSampleTimingViews::new(source, 1, 0, Some(0)).is_err());
    let original = mono_timing_source(mono_timing_config(), 0);
    assert!(AacSampleTimingViews::new(original.clone(), 0, 0, Some(0)).is_err());
    assert!(matches!(
        AacSampleTimingViews::new(original.clone(), 1, 0, Some(-1)),
        Err(MediaError::InvalidTimestamp)
    ));
    assert!(matches!(
        AacSampleTimingViews::new(original, 1, 0, Some(i64::MAX)),
        Err(MediaError::TimestampOverflow)
    ));
}

#[test]
fn paired_queue_pop_preserves_distinct_timing_and_exact_shared_source() {
    let original = mono_timing_source(mono_timing_config(), 8192);
    let views = AacSampleTimingViews::new(original.clone(), 9, -5, Some(0)).expect("views");
    let mut fanout = SampleFanout::new(limits(1024), limits(1024), false);
    assert_eq!(
        fanout.push_aac_timing_views(&views, 1).expect("queues"),
        (Some(EnqueueOutcome::Queued), EnqueueOutcome::Queued)
    );
    let publishing = fanout
        .publishing()
        .pop(2)
        .expect("clock")
        .expect("publishing");
    let recording = fanout
        .recording()
        .pop(2)
        .expect("clock")
        .expect("recording");
    assert_eq!(publishing.timing().pts(), 0);
    assert_eq!(recording.timing().pts(), 8192);
    assert!(Arc::ptr_eq(recording.payload(), publishing.payload()));
    assert!(Arc::ptr_eq(recording.payload(), original.payload()));
    assert!(Arc::ptr_eq(
        recording.aac_source().expect("record source").original(),
        publishing.aac_source().expect("publish source").original()
    ));
}

#[test]
fn withheld_publishing_is_not_a_drop_or_closed_branch_but_old_backlog_ages() {
    let config = mono_timing_config();
    let withheld = AacSampleTimingViews::new(mono_timing_source(config.clone(), 0), 1, -1000, None)
        .expect("recording-only original");
    let mut fanout = SampleFanout::new(limits(1024), limits(1024), false);
    assert_eq!(
        fanout.push_aac_timing_views(&withheld, 0).expect("queue"),
        (None, EnqueueOutcome::Queued)
    );
    assert_eq!(fanout.publishing().stats().dropped_samples, 0);
    assert_eq!(fanout.publishing().stats().accepted_samples, 0);
    assert!(!fanout.publishing().admission_closed());
    let short = QueueLimits::new(1024, 64, 10_000_000_000, 1_000).expect("publisher age");
    let long = QueueLimits::new(1024, 64, 10_000_000_000, 10_000).expect("recording age");
    let mut ageing = SampleFanout::new(short, long, false);
    let selected = AacSampleTimingViews::new(mono_timing_source(config.clone(), 0), 1, 0, Some(0))
        .expect("selected");
    ageing.push_aac_timing_views(&selected, 0).expect("first");
    let next =
        AacSampleTimingViews::new(mono_timing_source(config, 1024), 1, 1, None).expect("withheld");
    ageing
        .push_aac_timing_views(&next, 1000)
        .expect("old backlog ages");
    assert_eq!(ageing.publishing().stats().dropped_samples, 1);
    assert_eq!(ageing.publishing().stats().overflow_residence, 1);
    assert_eq!(ageing.recording().stats().queued_samples, 2);
}

#[test]
fn paired_queue_clock_failure_is_atomic_and_recording_refusal_stays_independent() {
    let source = mono_timing_source(mono_timing_config(), 0);
    let views = AacSampleTimingViews::new(source, 1, 0, Some(0)).expect("views");
    let mut fanout = SampleFanout::new(limits(1024), limits(1024), false);
    fanout.recording().pop(10).expect("recording clock");
    let before_publishing = fanout.publishing().stats();
    let before_recording = fanout.recording().stats();
    assert_eq!(
        fanout.push_aac_timing_views(&views, 9),
        Err(QueueError::ClockWentBackwards)
    );
    assert_eq!(fanout.publishing().stats(), before_publishing);
    assert_eq!(fanout.recording().stats(), before_recording);
    assert!(fanout.push_aac_timing_views(&views, 10).is_ok());
    let mut small = SampleFanout::new(limits(1024), limits(1), false);
    let (publishing, recording) = small
        .push_aac_timing_views(&views, 0)
        .expect("independent branches");
    assert_eq!(publishing, Some(EnqueueOutcome::Queued));
    assert!(matches!(
        recording,
        EnqueueOutcome::RecordingFailed {
            reason: OverflowReason::Bytes
        }
    ));
    assert_eq!(small.publishing().stats().queued_samples, 1);
    assert_eq!(small.recording().stats().queued_samples, 0);
}

#[test]
fn terminal_publishing_admission_keeps_the_accepted_prefix_and_recording() {
    let mut fanout = SampleFanout::new(limits(1024), limits(1024), true);
    let first = video(0, true);
    fanout.push(first.clone(), 10).expect("first");
    fanout.publishing().close_admission();
    fanout.publishing().close_admission();
    assert_eq!(
        fanout.push(video(1, false), 11).expect("remaining branch"),
        (EnqueueOutcome::AdmissionClosed, EnqueueOutcome::Queued)
    );
    let accepted = fanout.publishing().pop(12).expect("clock").expect("prefix");
    assert!(Arc::ptr_eq(&accepted, &first));
    assert!(fanout.publishing().pop(12).expect("clock").is_none());
    assert!(Arc::ptr_eq(
        &fanout.recording().pop(12).expect("clock").expect("first"),
        &first
    ));
    assert!(fanout.recording().pop(12).expect("clock").is_some());
    assert_eq!(fanout.publishing().stats().overflow_events, 0);
    assert!(fanout.recording().recording_failure().is_none());
    fanout.clear();
    assert_eq!(
        fanout
            .push(video(2, true), 13)
            .expect("clear is not reopen"),
        (EnqueueOutcome::AdmissionClosed, EnqueueOutcome::Queued)
    );
}

#[test]
fn terminal_recording_admission_and_both_closed_keep_truthful_outcomes() {
    let mut fanout = SampleFanout::new(limits(1024), limits(1024), false);
    fanout.push(audio(0), 10).expect("first");
    fanout.recording().close_admission();
    assert_eq!(
        fanout.push(audio(1024), 11).expect("publishing still open"),
        (EnqueueOutcome::Queued, EnqueueOutcome::AdmissionClosed)
    );
    assert!(fanout.recording().pop(12).expect("clock").is_some());
    assert!(fanout.recording().pop(12).expect("clock").is_none());
    fanout.publishing().close_admission();
    assert_eq!(
        fanout.push(audio(2048), 13).expect("both terminal"),
        (
            EnqueueOutcome::AdmissionClosed,
            EnqueueOutcome::AdmissionClosed
        )
    );
    assert_eq!(fanout.publishing().stats().queued_samples, 2);
    assert_eq!(fanout.recording().stats().queued_samples, 0);
    assert_eq!(fanout.recording().stats().overflow_events, 0);
    assert!(fanout.recording().recording_failure().is_none());
    assert!(fanout.publishing().admission_closed());
    assert!(fanout.recording().admission_closed());
}

#[test]
fn terminal_branch_preserves_clock_preflight_and_normal_residence_rules() {
    let mut fanout = SampleFanout::new(limits(1024), limits(1024), false);
    fanout.push(audio(0), 10).expect("first");
    fanout.publishing().close_admission();
    let before = fanout.recording().stats();
    assert_eq!(
        fanout.push(audio(1024), 9),
        Err(QueueError::ClockWentBackwards)
    );
    assert_eq!(fanout.recording().stats(), before);
    let expired = 1_000_000_010; // Fixture residence limit is one second.
    assert!(
        fanout
            .publishing()
            .pop(expired)
            .expect("residence")
            .is_none()
    );
    assert!(fanout.publishing().admission_closed());
    assert!(
        fanout
            .recording()
            .pop(expired)
            .expect("accepted recording prefix")
            .is_some()
    );
    assert!(fanout.recording().recording_failure().is_some());
}
fn limits(bytes: usize) -> QueueLimits {
    QueueLimits::new(bytes, 64, 2_000_000_000, 1_000_000_000).expect("limits")
}

#[test]
fn concurrent_tracks_each_get_the_full_media_budget_and_pop_updates_the_maximum() {
    let cap = QueueLimits::new(1_000_000, 8192, 2_000_000_000, 2_000_000_000)
        .expect("valid queue fixture operation");
    for tracks in [(true, true), (true, false), (false, true)] {
        let mut queue = BoundedQueue::new(cap, OverflowPolicy::Recording);
        // 60 video units ~= 2s; 93 AAC units = 1.984s. All enqueue at a fixed
        // scheduler instant so this tests media duration, independently of age.
        for n in 0..93 {
            if tracks.0 && n < 60 {
                assert_eq!(
                    queue
                        .push(video(n, n == 0), 0)
                        .expect("valid queue fixture operation"),
                    EnqueueOutcome::Queued
                );
            }
            if tracks.1 {
                assert_eq!(
                    queue
                        .push(audio(n * 1024), 0)
                        .expect("valid queue fixture operation"),
                    EnqueueOutcome::Queued
                );
            }
        }
        let stats = queue.stats();
        assert_eq!(
            stats.queued_video_ns,
            if tracks.0 { 1_999_999_980 } else { 0 }
        );
        assert_eq!(
            stats.queued_audio_ns,
            if tracks.1 { 1_983_999_969 } else { 0 }
        );
        assert_eq!(
            stats.queued_media_ns,
            stats.queued_video_ns.max(stats.queued_audio_ns)
        );
        assert_eq!(stats.overflow_events, 0);
        if tracks.0 {
            queue.pop(0).expect("valid queue fixture operation");
            assert_eq!(queue.stats().queued_video_ns, 1_966_666_647);
            assert_eq!(
                queue.stats().queued_media_ns,
                if tracks.1 {
                    1_983_999_969
                } else {
                    1_966_666_647
                }
            );
        }
        let outcome = if tracks.1 {
            queue.push(audio(93 * 1024), 0)
        } else {
            queue
                .push(video(61, false), 0)
                .and_then(|_| queue.push(video(62, false), 0))
        };
        assert_eq!(
            outcome.expect("valid queue fixture operation"),
            EnqueueOutcome::RecordingFailed {
                reason: OverflowReason::MediaDuration
            }
        );
        assert_eq!(queue.stats().overflow_media_duration, 1);
        assert_eq!(queue.stats().overflow_residence, 0);
        queue.clear();
        assert_eq!(queue.stats().queued_video_ns, 0);
        assert_eq!(queue.stats().queued_audio_ns, 0);
        assert_eq!(queue.stats().queued_media_ns, 0);
    }
}
fn publishing(bytes: usize) -> BoundedQueue {
    BoundedQueue::new(
        limits(bytes),
        OverflowPolicy::Publishing { has_video: true },
    )
}

#[test]
fn timing_refuses_invalid_clocks_and_overflow() {
    assert_eq!(
        SampleTiming::new(0, 0, 1, 0),
        Err(MediaError::InvalidTimescale)
    );
    assert_eq!(
        SampleTiming::new(0, 0, 1, 1_000_000_001),
        Err(MediaError::InvalidTimescale)
    );
    assert_eq!(
        SampleTiming::new(-1, 0, 1, 30),
        Err(MediaError::InvalidTimestamp)
    );
    assert_eq!(
        SampleTiming::new(0, 0, 0, 30),
        Err(MediaError::InvalidDuration)
    );
    assert_eq!(
        SampleTiming::new(i64::MAX, 0, 1, 1),
        Err(MediaError::TimestampOverflow)
    );
    assert_eq!(timing(30).decode_time_ns(), 1_000_000_000);
}

#[test]
fn aac_profile_is_narrow_and_exact() {
    let aac = AacConfig::new(&[0x11, 0x90]).expect("48k stereo");
    assert_eq!((aac.sample_rate(), aac.channels()), (48000, 2));
    for bad in [
        &[0x11][..],
        &[0x11, 0x90, 0][..],
        &[0x09, 0x90][..],
        &[0x11, 0x91][..],
        &[0x17, 0x90][..],
        &[0x11, 0x80][..],
        &[0x11, 0x98][..],
    ] {
        assert!(AacConfig::new(bad).is_err(), "{bad:?}");
    }
    assert!(EncodedSample::new(audio_config(), timing(0), false, Arc::from([1])).is_err());
    assert!(
        EncodedSample::new(
            audio_config(),
            SampleTiming::new(0, 0, 1024, 48000).expect("time"),
            false,
            Arc::from([0xff, 0xf1, 0])
        )
        .is_err()
    );
}

#[test]
fn h264_parameter_sets_and_framing_are_checked() {
    assert!(H264Config::new(&[0x67], &[0x68]).is_err());
    let config = video_config();
    let CodecConfig::H264(h264) = config.as_ref() else {
        panic!("video")
    };
    assert!(H264Config::new(h264.sps(), &[0x68]).is_err());
    assert!(H264Config::new(h264.pps(), h264.sps()).is_err());
    for bad in [
        &[0, 0, 0][..],
        &[0, 0, 0, 0][..],
        &[0, 0, 0, 9, 0x65][..],
        &[0, 0, 0, 1, 0xe5][..],
        &[0, 0, 0, 1, 0x67][..],
        &[0, 0, 0, 1, 0x65, 0][..],
    ] {
        assert!(
            EncodedSample::new(config.clone(), timing(0), true, Arc::from(bad)).is_err(),
            "{bad:?}"
        );
    }
    assert!(EncodedSample::new(config, timing(0), false, Arc::from([0, 0, 0, 1, 0x65])).is_err());
}

#[test]
fn every_truncation_is_rejected_without_panicking() {
    let bytes = [0, 0, 0, 4, 0x65, 0x88, 0x84, 0x80];
    let config = video_config();
    for n in 0..bytes.len() {
        assert!(
            EncodedSample::new(config.clone(), timing(0), true, Arc::from(&bytes[..n])).is_err()
        );
    }
    for n in 0..32 {
        let bytes = vec![0; n];
        assert!(H264Config::new(&bytes, &[0x68]).is_err());
    }
}

#[test]
fn continuity_requires_explicit_config_change_and_discontinuity() {
    let config = video_config();
    let mut continuity = SampleContinuity::default();
    assert_eq!(
        continuity.validate(&video_with(config.clone(), 0, false)),
        Err(ContinuityError::NeedsKeyframe)
    );
    continuity
        .validate(&video_with(config.clone(), 0, true))
        .expect("start");
    assert_eq!(
        continuity.validate(&video_with(config.clone(), 0, false)),
        Err(ContinuityError::TimestampRegression)
    );
    continuity
        .validate(&video_with(config.clone(), 1, false))
        .expect("advance");
    assert_eq!(
        continuity.validate(&video(2, true)),
        Err(ContinuityError::ConfigurationChanged)
    );
    continuity.reset();
    continuity.validate(&video(0, true)).expect("new epoch");
}

#[test]
fn single_payload_is_shared_across_independent_sinks() {
    let sample = video(0, true);
    let mut fanout = SampleFanout::new(limits(12), limits(24), true);
    assert_eq!(
        fanout.push(sample.clone(), 0).expect("fanout"),
        (EnqueueOutcome::Queued, EnqueueOutcome::Queued)
    );
    let published = fanout.publishing().pop(0).expect("clock").expect("sample");
    let recorded = fanout.recording().pop(0).expect("clock").expect("sample");
    assert!(Arc::ptr_eq(&sample, &published));
    assert!(Arc::ptr_eq(&published, &recorded));
    assert!(Arc::ptr_eq(published.payload(), recorded.payload()));
    drop((published, recorded));
    fanout.push(sample.clone(), 1).expect("fanout");
    assert_eq!(Arc::strong_count(&sample), 3);
    fanout.clear();
    assert_eq!(Arc::strong_count(&sample), 1);
}

#[test]
fn network_overflow_does_not_stop_recording_and_waits_for_idr() {
    let mut fanout = SampleFanout::new(limits(6), limits(60), true);
    fanout.push(video(0, true), 0).expect("first");
    let (network, recording) = fanout.push(video(1, false), 1).expect("overflow");
    assert_eq!(
        network,
        EnqueueOutcome::WaitingForKeyframe {
            discarded: 1,
            reason: Some(OverflowReason::Bytes)
        }
    );
    assert_eq!(recording, EnqueueOutcome::Queued);
    assert_eq!(fanout.publishing().stats().queued_samples, 0);
    assert_eq!(fanout.recording().stats().queued_samples, 2);
    assert_eq!(
        fanout.push(audio(0), 2).expect("audio").0,
        EnqueueOutcome::WaitingForKeyframe {
            discarded: 0,
            reason: None
        }
    );
    assert_eq!(
        fanout.push(video(2, true), 3).expect("IDR").0,
        EnqueueOutcome::Queued
    );
}

#[test]
fn recording_overflow_keeps_accepted_prefix_and_network_continues() {
    let mut fanout = SampleFanout::new(limits(60), limits(6), true);
    fanout.push(video(0, true), 0).expect("first");
    assert_eq!(
        fanout.push(video(1, false), 1).expect("overflow"),
        (
            EnqueueOutcome::Queued,
            EnqueueOutcome::RecordingFailed {
                reason: OverflowReason::Bytes
            }
        )
    );
    assert_eq!(
        fanout.push(video(2, false), 2).expect("failed sink").1,
        EnqueueOutcome::RecordingAlreadyFailed
    );
    assert_eq!(fanout.recording().stats().queued_bytes, 6);
    assert!(fanout.recording().pop(2).expect("drain").is_some());
    assert_eq!(fanout.publishing().stats().queued_samples, 3);
}

#[test]
fn byte_duration_sample_and_residence_bounds_are_independent() {
    for (limits, reason) in [
        (
            QueueLimits::new(5, 64, 1_000_000_000, 1_000_000_000).expect("limits"),
            OverflowReason::Bytes,
        ),
        (
            QueueLimits::new(60, 1, 1_000_000_000, 1_000_000_000).expect("limits"),
            OverflowReason::Samples,
        ),
        (
            QueueLimits::new(60, 64, 40_000_000, 1_000_000_000).expect("limits"),
            OverflowReason::MediaDuration,
        ),
    ] {
        let mut queue = BoundedQueue::new(limits, OverflowPolicy::Recording);
        let first = queue.push(video(0, true), 0).expect("first");
        let outcome = if first == EnqueueOutcome::Queued {
            queue.push(video(1, false), 1).expect("second")
        } else {
            first
        };
        assert_eq!(outcome, EnqueueOutcome::RecordingFailed { reason });
    }
    let mut queue = publishing(60);
    queue.push(video(0, true), 0).expect("first");
    assert!(queue.pop(1_000_000_000).expect("age").is_none());
    assert!(queue.waiting_for_keyframe());
    assert_eq!(queue.stats().overflow_events, 1);
}

#[test]
fn bounded_backlog_does_not_grow_with_show_length() {
    let mut queue = publishing(60);
    for n in 0..10_000 {
        queue
            .push(video(n, n % 30 == 0), n as u64 * 33_333_333)
            .expect("sample");
        let stats = queue.stats();
        assert!(stats.queued_bytes <= 60);
        assert!(stats.queued_samples <= 64);
        assert!(stats.queued_media_ns <= 2_000_000_000);
    }
    assert!(queue.stats().overflow_events > 0);
}

#[test]
fn audio_only_can_resume_without_video_and_backwards_clock_is_atomic() {
    let mut fanout = SampleFanout::new(limits(2), limits(20), false);
    fanout.push(audio(0), 10).expect("first");
    assert_eq!(
        fanout.push(audio(1024), 11).expect("overflow").0,
        EnqueueOutcome::QueuedAfterReset {
            discarded: 1,
            reason: OverflowReason::Bytes
        }
    );
    let before = (fanout.publishing().stats(), fanout.recording().stats());
    assert_eq!(
        fanout.push(audio(2048), 9),
        Err(QueueError::ClockWentBackwards)
    );
    assert_eq!(
        before,
        (fanout.publishing().stats(), fanout.recording().stats())
    );
}

fn live() -> (Session, Generation) {
    let mut session = Session::new(ReconnectPolicy::default());
    let generation = session.start(0).expect("start");
    assert_eq!(session.prepared(generation), Ok(SessionAction::Connect));
    session.connected(generation, 0).expect("connect");
    assert_eq!(session.state(), SessionState::Connecting);
    session.media_confirmed(generation, 0).expect("media");
    (session, generation)
}

#[test]
fn repeated_failures_and_socket_handshakes_preserve_first_deadline() {
    let (mut session, generation) = live();
    let first_drop = 10_000_000_000;
    session
        .transport_failed(generation, first_drop)
        .expect("drop");
    let deadline = first_drop + DEFAULT_RECONNECT_GRACE_NS;
    assert_eq!(session.reconnect_deadline_ns(), Some(deadline));
    for n in 1..9 {
        let now = first_drop + n * 10_000_000_000;
        assert_eq!(session.tick(generation, now), Ok(SessionAction::Connect));
        session.connected(generation, now).expect("socket");
        session
            .transport_failed(generation, now)
            .expect("repeat drop");
        assert_eq!(session.reconnect_deadline_ns(), Some(deadline));
    }
    assert_eq!(
        session.tick(generation, deadline),
        Ok(SessionAction::GraceExpired)
    );
    assert_eq!(session.state(), SessionState::Failed);
    assert!(!session.transport_connected());
}

#[test]
fn destination_observed_recovery_allows_a_new_independent_outage() {
    let (mut session, generation) = live();
    session.transport_failed(generation, 1).expect("drop");
    session.tick(generation, 500_000_001).expect("retry");
    session.connected(generation, 500_000_001).expect("socket");
    session
        .media_confirmed(generation, 500_000_002)
        .expect("observed recovery");
    assert_eq!(session.reconnect_deadline_ns(), None);
    session
        .transport_failed(generation, 200_000_000_000)
        .expect("new outage");
    assert_eq!(session.reconnect_deadline_ns(), Some(290_000_000_000));
}

#[test]
fn stop_and_cancel_fence_late_callbacks_and_retries() {
    let (mut session, generation) = live();
    session.transport_failed(generation, 1).expect("drop");
    let (stop_generation, action) = session.stop(generation).expect("stop");
    assert_eq!(action, SessionAction::EndDestination);
    assert_eq!(
        session.connected(generation, 2),
        Err(SessionError::StaleGeneration)
    );
    assert_eq!(
        session.tick(generation, 1_000_000_000),
        Err(SessionError::StaleGeneration)
    );
    assert_eq!(
        session.tick(stop_generation, 1_000_000_000),
        Ok(SessionAction::None)
    );
    session.finish_stop(stop_generation).expect("finalise");
    let next = session.start(2_000_000_000).expect("restart");
    assert_ne!(next, generation);
    session.cancel(next).expect("cancel");
    assert_eq!(session.prepared(next), Err(SessionError::StaleGeneration));
    assert_eq!(session.state(), SessionState::Stopped);
}

#[test]
fn interruption_requires_explicit_end_and_restart() {
    let (mut session, generation) = live();
    let interrupted = session.interrupt(generation).expect("interrupt");
    assert_eq!(session.state(), SessionState::Interrupted);
    assert_eq!(
        session.media_confirmed(generation, 1),
        Err(SessionError::StaleGeneration)
    );
    assert_eq!(session.start(1), Err(SessionError::InvalidTransition));
    let (stopping, _) = session.stop(interrupted).expect("end");
    session.finish_stop(stopping).expect("stopped");
    assert!(session.start(2).is_ok());
}

#[test]
fn configured_grace_retry_cap_and_clock_overflow_are_explicit() {
    assert!(ReconnectPolicy::new(10, 0, 1).is_err());
    let mut session = Session::new(ReconnectPolicy::new(10, 2, 4).expect("policy"));
    let generation = session.start(0).expect("start");
    session.prepared(generation).expect("prepared");
    assert_eq!(
        session.transport_failed(generation, 1),
        Ok(SessionAction::RetryAt(3))
    );
    assert_eq!(
        session.transport_failed(generation, 2),
        Ok(SessionAction::RetryAt(6))
    );
    assert_eq!(
        session.transport_failed(generation, 3),
        Ok(SessionAction::RetryAt(7))
    );
    assert_eq!(
        session.transport_failed(generation, 10),
        Ok(SessionAction::AwaitGraceDeadline(11))
    );
    assert_eq!(
        session.tick(generation, 9),
        Err(SessionError::ClockWentBackwards)
    );
    assert_eq!(
        session.tick(generation, 11),
        Ok(SessionAction::GraceExpired)
    );
    let mut session = Session::new(ReconnectPolicy::default());
    let generation = session.start(u64::MAX - 1).expect("start near limit");
    session.prepared(generation).expect("prepared");
    assert_eq!(
        session.transport_failed(generation, u64::MAX - 1),
        Err(SessionError::ClockOverflow)
    );
}

#[test]
fn zero_destination_grace_fails_immediately_without_a_retry() {
    let mut session = Session::new(ReconnectPolicy::new(0, 1, 2).expect("zero grace policy"));
    let generation = session.start(0).expect("start");
    session.prepared(generation).expect("prepare");
    assert_eq!(
        session.transport_failed(generation, 7),
        Ok(SessionAction::GraceExpired)
    );
    assert_eq!(session.outage_deadline_ns(), Some(7));
    assert_eq!(session.reconnect_deadline_ns(), None);
    assert_eq!(session.state(), SessionState::Failed);
    assert_eq!(session.tick(generation, 7), Ok(SessionAction::None));
    assert_eq!(session.stats().retry_attempts, 0);
}
