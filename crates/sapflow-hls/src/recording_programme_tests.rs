use crate::codec::AacConfig;
use crate::fmp4;
use crate::recording_programme::*;
use crate::recording_timing::{RecordingPresentation, TimingError};
use bytes::Bytes;
use mp4_atom::{Decode, Moov};

// These are supplied scalar fixtures, never measured native leading facts.
fn start() -> ProgrammeWindowStart {
    ProgrammeWindowStart {
        generation: 7,
        coded_origin: 48_000,
        preparation_frames: 8192,
        measured_displacement: 2048,
        discard_prefix_frames: 8192,
        movie_delay_ticks: 0,
    }
}

fn end() -> ProgrammeGenerationEnd {
    ProgrammeGenerationEnd {
        generation: 7,
        queued: QueuedProgrammeFrames {
            preparation: 8192,
            programme_raw: 1517,
            programme_zero: 1248,
            block_padding: 307,
            codec_drain: 4096,
        },
        coded_frames: 15_360,
    }
}

fn observed() -> ObservedProgrammeSpan {
    ObservedProgrammeSpan {
        first_dts: 56_192,
        end_dts: 61_312,
        access_units: 5,
    }
}

fn begun(value: ProgrammeWindowStart) -> ProgrammeWindowLedger {
    let mut ledger = ProgrammeWindowLedger::default();
    ledger.begin(value, 0).expect("checked scalar start");
    ledger
}

fn complete() -> CheckedProgrammeWindow {
    begun(start())
        .complete(end(), observed())
        .expect("checked scalar window")
}

fn audio() -> AacConfig {
    AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("canonical AAC48 mono")
}

fn movie(bytes: &Bytes) -> Moov {
    let mut input = std::io::Cursor::new(bytes.as_ref());
    mp4_atom::Ftyp::decode(&mut input).expect("ftyp");
    Moov::decode(&mut input).expect("moov")
}

#[test]
fn queued_source_components_never_relabel_preparation_padding_or_drain_as_programme() {
    let window = complete();
    assert_eq!(window.queued(), end().queued);
    assert_eq!(window.queued().total_queued(), Ok(15_360));
    assert_eq!(window.programme_frames(), 2765);
    assert_eq!(window.queued().programme_raw, 1517);
    assert_eq!(window.queued().programme_zero, 1248);
    assert_eq!(window.selected_frames(), 5120);
    assert_ne!(
        window
            .queued()
            .total_queued()
            .expect("bounded queued frame sum"),
        window.programme_frames()
    );
}

#[test]
fn selected_original_extent_conserves_discarded_prefix_and_suffix() {
    let value = complete();
    assert_eq!(value.generation(), 7);
    assert_eq!(value.discarded_prefix_frames(), 8192);
    assert_eq!(value.selected_end(), 13_312);
    assert_eq!(value.selected_frames(), 5120);
    assert_eq!(value.discarded_suffix_frames(), 2048);
    assert_eq!(value.coded_frames(), 15_360);
    assert_eq!(value.leading_frames(), 2048);
    assert_eq!(value.trailing_frames(), 307);
    assert_eq!(
        value.selected_frames() - 2048 - 307,
        value.programme_frames()
    );
    assert_eq!(
        value.discarded_prefix_frames() + value.selected_frames() + value.discarded_suffix_frames(),
        value.coded_frames()
    );
}

#[test]
fn coded_extent_is_not_inferred_from_successfully_queued_input() {
    let mut supplied = end();
    supplied.coded_frames += 1024;
    let window = begun(start())
        .complete(supplied, observed())
        .expect("complete window with independently supplied coded extent");
    assert_eq!(window.queued().total_queued(), Ok(15_360));
    assert_eq!(window.coded_frames(), 16_384);
    assert_eq!(window.discarded_suffix_frames(), 3072);
}

#[test]
fn aligned_programme_start_can_exclude_all_original_preparation_aus() {
    let mut mapping = start();
    mapping.discard_prefix_frames = 10_240;
    let span = ObservedProgrammeSpan {
        first_dts: 58_240,
        end_dts: 61_312,
        access_units: 3,
    };
    let window = begun(mapping)
        .complete(end(), span)
        .expect("aligned complete programme window");
    assert_eq!(window.leading_frames(), 0);
    assert_eq!(window.selected_frames(), 3072);
    assert_eq!(window.programme_frames(), 2765);
    assert_eq!(window.presentation().media_time(), 58_240);
}

#[test]
fn head_8191_and_tail_1023_are_supported_exact_boundaries() {
    let mapping = ProgrammeWindowStart {
        generation: 1,
        coded_origin: 0,
        preparation_frames: 8191,
        measured_displacement: 0,
        discard_prefix_frames: 0,
        movie_delay_ticks: 0,
    };
    let supplied = ProgrammeGenerationEnd {
        generation: 1,
        queued: QueuedProgrammeFrames {
            preparation: 8191,
            programme_raw: 2,
            programme_zero: 0,
            block_padding: 1023,
            codec_drain: 0,
        },
        coded_frames: 9216,
    };
    let span = ObservedProgrammeSpan {
        first_dts: 0,
        end_dts: 9216,
        access_units: 9,
    };
    let window = begun(mapping)
        .complete(supplied, span)
        .expect("head and tail boundary window");
    assert_eq!(window.leading_frames(), 8191);
    assert_eq!(window.trailing_frames(), 1023);
    assert_eq!(window.presentation().presented_frames(), 2);
}

#[test]
fn aligned_programme_end_keeps_zero_tail_without_adding_a_whole_au() {
    let mut supplied = end();
    supplied.queued.programme_raw = 1824;
    supplied.queued.block_padding = 0;
    let window = begun(start())
        .complete(supplied, observed())
        .expect("complete AU-aligned programme end");
    assert_eq!(window.programme_frames(), 3072);
    assert_eq!(window.trailing_frames(), 0);
    assert_eq!(window.selected_end(), 13_312);
}

#[test]
fn coded_deficit_cannot_be_repaired_by_queue_eos_or_accepted_input_counts() {
    let mut ledger = begun(start());
    let before = ledger.clone();
    let mut supplied = end();
    supplied.coded_frames = 12_288;
    assert_eq!(
        ledger.complete(supplied, observed()),
        Err(TimingError::Inconsistent)
    );
    assert_eq!(ledger, before);
    assert!(ledger.completed().is_none());
}

#[test]
fn mismatched_generation_preparation_or_non_au_coded_extent_leaves_state_unchanged() {
    let mut wrong_generation = end();
    wrong_generation.generation += 1;
    let mut wrong_preparation = end();
    wrong_preparation.queued.preparation -= 1;
    let mut partial_au = end();
    partial_au.coded_frames -= 1;
    for supplied in [wrong_generation, wrong_preparation, partial_au] {
        let mut ledger = begun(start());
        let before = ledger.clone();
        assert_eq!(
            ledger.complete(supplied, observed()),
            Err(TimingError::Inconsistent)
        );
        assert_eq!(ledger, before);
    }
}

#[test]
fn retained_first_clock_endpoint_and_count_must_match_exact_selected_window() {
    let span = observed();
    for wrong in [
        ObservedProgrammeSpan {
            first_dts: span.first_dts + 1024,
            ..span
        },
        ObservedProgrammeSpan {
            end_dts: span.end_dts + 1024,
            ..span
        },
        ObservedProgrammeSpan {
            access_units: span.access_units + 1,
            end_dts: span.end_dts + 1024,
            ..span
        },
    ] {
        let mut ledger = begun(start());
        let before = ledger.clone();
        assert_eq!(
            ledger.complete(end(), wrong),
            Err(TimingError::Inconsistent)
        );
        assert_eq!(ledger, before);
    }
}

#[test]
fn start_refuses_zero_identity_late_begin_unaligned_discard_and_head_8192() {
    let mut zero_generation = start();
    zero_generation.generation = 0;
    let mut unaligned = start();
    unaligned.discard_prefix_frames += 1;
    let mut after_programme = start();
    after_programme.discard_prefix_frames = 11_264;
    let mut too_much_head = start();
    too_much_head.discard_prefix_frames = 2048;
    for mapping in [zero_generation, unaligned, after_programme, too_much_head] {
        let mut ledger = ProgrammeWindowLedger::default();
        assert!(ledger.begin(mapping, 0).is_err());
        assert_eq!(ledger, ProgrammeWindowLedger::default());
    }
    let mut ledger = ProgrammeWindowLedger::default();
    assert_eq!(ledger.begin(start(), 1), Err(TimingError::Inconsistent));
    assert_eq!(ledger, ProgrammeWindowLedger::default());
}

#[test]
fn duplicate_or_unbegun_operations_cannot_issue_a_second_complete_window() {
    let mut empty = ProgrammeWindowLedger::default();
    assert_eq!(
        empty.complete(end(), observed()),
        Err(TimingError::Inconsistent)
    );
    assert_eq!(
        empty.prefix_presentation(observed()),
        Err(TimingError::Inconsistent)
    );
    let mut ledger = begun(start());
    assert_eq!(ledger.begin(start(), 0), Err(TimingError::Inconsistent));
    let checked = ledger
        .complete(end(), observed())
        .expect("first complete window");
    assert_eq!(ledger.completed(), Some(&checked));
    assert_eq!(ledger.complete(end(), observed()), Err(TimingError::Closed));
    assert_eq!(ledger.begin(start(), 0), Err(TimingError::Closed));
    assert_eq!(
        ledger.prefix_presentation(observed()),
        Err(TimingError::Closed)
    );
}

#[test]
fn preparation_pad_and_drain_without_positive_programme_cannot_complete() {
    let mut supplied = end();
    supplied.queued.programme_raw = 0;
    supplied.queued.programme_zero = 0;
    let mut ledger = begun(start());
    let before = ledger.clone();
    assert_eq!(
        ledger.complete(supplied, observed()),
        Err(TimingError::Inconsistent)
    );
    assert_eq!(ledger, before);
}

#[test]
fn intentionally_zero_programme_is_distinct_from_codec_padding() {
    let mut supplied = end();
    supplied.queued.programme_zero += supplied.queued.programme_raw;
    supplied.queued.programme_raw = 0;
    let window = begun(start())
        .complete(supplied, observed())
        .expect("intentional-zero programme window");
    assert_eq!(window.programme_frames(), 2765);
    assert_eq!(window.queued().programme_zero, 2765);
    assert_eq!(window.queued().block_padding, 307);
    assert_eq!(window.queued().codec_drain, 4096);
}

#[test]
fn source_and_coded_arithmetic_overflows_fail_without_a_window() {
    let mut programme_sum = end();
    programme_sum.queued.programme_raw = u64::MAX;
    let mut physical_sum = end();
    physical_sum.queued.codec_drain = u64::MAX;
    let mut endpoint = end();
    endpoint.coded_frames = u64::MAX - 1023;
    for supplied in [programme_sum, physical_sum, endpoint] {
        let mut ledger = begun(start());
        let before = ledger.clone();
        assert_eq!(
            ledger.complete(supplied, observed()),
            Err(TimingError::Overflow)
        );
        assert_eq!(ledger, before);
    }
}

#[test]
fn start_clock_and_head_arithmetic_overflows_are_checked_before_commit() {
    let mut displacement = start();
    displacement.measured_displacement = u64::MAX;
    let mut origin = start();
    origin.coded_origin = u64::MAX;
    let mut signed_media = start();
    signed_media.coded_origin = i64::MAX as u64;
    for mapping in [displacement, origin, signed_media] {
        let mut ledger = ProgrammeWindowLedger::default();
        assert_eq!(ledger.begin(mapping, 0), Err(TimingError::Overflow));
        assert_eq!(ledger, ProgrammeWindowLedger::default());
    }
}

#[test]
fn au_count_multiplication_and_movie_duration_overflow_do_not_commit() {
    let mut span = observed();
    span.access_units = u64::MAX;
    let mut ledger = begun(start());
    let before = ledger.clone();
    assert_eq!(ledger.complete(end(), span), Err(TimingError::Overflow));
    assert_eq!(ledger, before);
    let mut mapping = start();
    mapping.movie_delay_ticks = u64::MAX;
    let mut ledger = begun(mapping);
    let before = ledger.clone();
    assert_eq!(
        ledger.complete(end(), observed()),
        Err(TimingError::Overflow)
    );
    assert_eq!(ledger, before);
}

#[test]
fn programme_endpoint_and_au_ceiling_overflow_are_checked_without_rounding_down() {
    let mapping = ProgrammeWindowStart {
        generation: 1,
        coded_origin: 0,
        preparation_frames: 0,
        measured_displacement: 1,
        discard_prefix_frames: 0,
        movie_delay_ticks: 0,
    };
    for frames in [u64::MAX, u64::MAX - 1] {
        let supplied = ProgrammeGenerationEnd {
            generation: 1,
            queued: QueuedProgrammeFrames {
                preparation: 0,
                programme_raw: frames,
                programme_zero: 0,
                block_padding: 0,
                codec_drain: 0,
            },
            coded_frames: u64::MAX - 1023,
        };
        let mut ledger = begun(mapping);
        let before = ledger.clone();
        assert_eq!(
            ledger.complete(
                supplied,
                ObservedProgrammeSpan {
                    first_dts: 0,
                    end_dts: 0,
                    access_units: 0
                }
            ),
            Err(TimingError::Overflow)
        );
        assert_eq!(ledger, before);
    }
}

#[test]
fn incomplete_prefix_retains_unknown_tail_and_can_have_zero_presentation() {
    let ledger = begun(start());
    let span = ObservedProgrammeSpan {
        first_dts: 56_192,
        end_dts: 57_216,
        access_units: 1,
    };
    let prefix = ledger
        .prefix_presentation(span)
        .expect("unknown-tail prefix metadata");
    assert_eq!(
        prefix.completion(),
        ProgrammePresentationCompletion::IncompletePrefix
    );
    assert_eq!(prefix.coded_frames(), 1024);
    assert_eq!(prefix.leading_frames(), 2048);
    assert_eq!(prefix.trailing_frames(), 0);
    assert_eq!(prefix.presented_frames(), 0);
    assert_eq!(
        prefix
            .movie_extent()
            .expect("zero-presentation prefix movie extent")
            .duration_ticks,
        0
    );
    assert!(ledger.completed().is_none());
}

#[test]
fn positive_prefix_does_not_guess_complete_window_or_final_tail() {
    let mut ledger = begun(start());
    let prefix = ledger
        .prefix_presentation(observed())
        .expect("positive unknown-tail prefix");
    assert_eq!(prefix.presented_frames(), 3072);
    assert_eq!(prefix.trailing_frames(), 0);
    assert_eq!(
        prefix.completion(),
        ProgrammePresentationCompletion::IncompletePrefix
    );
    let complete = ledger
        .complete(end(), observed())
        .expect("complete programme presentation")
        .presentation();
    assert_eq!(
        complete.completion(),
        ProgrammePresentationCompletion::CompleteWindow
    );
    assert_eq!(complete.presented_frames(), 2765);
    assert_eq!(complete.trailing_frames(), 307);
}

#[test]
fn original_coded_start_is_separate_from_movie_origin_and_exact_ceil_milliseconds() {
    let value = complete().presentation();
    assert_eq!(value.coded_start(), 56_192);
    assert_eq!(value.media_time(), 58_240);
    assert_eq!(value.movie_delay_ticks(), 0);
    let extent = value.movie_extent().expect("exact programme movie extent");
    assert_eq!(extent.movie_timescale, 720_000);
    assert_eq!(extent.duration_ticks, 41_475);
    assert_eq!(extent.duration_ms_ceil, 58);
}

#[test]
fn programme_header_keeps_nonzero_media_edit_without_a_preparation_movie_gap() {
    let bytes = fmp4::init_segment_for_programme_recording(&audio(), &complete().presentation())
        .expect("programme MP4 header");
    let parsed = movie(&bytes);
    assert_eq!(parsed.mvhd.timescale, 720_000);
    assert_eq!(parsed.mvhd.duration, 41_475);
    assert_eq!(parsed.trak.len(), 1);
    assert_eq!(parsed.trak[0].tkhd.duration, 41_475);
    assert_eq!(parsed.trak[0].mdia.mdhd.duration, 0);
    let edits = &parsed.trak[0]
        .edts
        .as_ref()
        .expect("audio edit container")
        .elst
        .as_ref()
        .expect("audio edit list")
        .entries;
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].media_time, Some(58_240));
    assert_eq!(edits[0].segment_duration, 41_475);
}

#[test]
fn explicit_movie_delay_is_not_replaced_by_original_coded_clock() {
    let mut mapping = start();
    mapping.movie_delay_ticks = 123;
    let value = begun(mapping)
        .complete(end(), observed())
        .expect("complete window with explicit movie delay")
        .presentation();
    let parsed = movie(
        &fmp4::init_segment_for_programme_recording(&audio(), &value)
            .expect("programme header with movie delay"),
    );
    assert_eq!(parsed.mvhd.duration, 41_598);
    let edits = &parsed.trak[0]
        .edts
        .as_ref()
        .expect("audio edit container")
        .elst
        .as_ref()
        .expect("audio edit list")
        .entries;
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].media_time, None);
    assert_eq!(edits[0].segment_duration, 123);
    assert_eq!(edits[1].media_time, Some(58_240));
    assert_eq!(edits[1].segment_duration, 41_475);
}

#[test]
fn initial_prefix_and_complete_programme_headers_keep_identical_fixed_length() {
    let mut ledger = begun(start());
    let prefix = ledger
        .prefix_presentation(observed())
        .expect("initial unknown-tail prefix");
    let final_window = ledger
        .complete(end(), observed())
        .expect("complete programme window")
        .presentation();
    let initial = fmp4::init_segment_for_programme_recording(&audio(), &prefix)
        .expect("initial programme header");
    let final_header = fmp4::init_segment_for_programme_recording(&audio(), &final_window)
        .expect("complete programme header");
    assert_eq!(initial.len(), final_header.len());
    assert_eq!(movie(&initial).mvhd.duration, 3072 * 15);
    assert_eq!(movie(&final_header).mvhd.duration, 2765 * 15);
}

#[test]
fn old_wrapper_bytes_keep_old_clock_semantics_when_movie_delay_is_explicitly_equal() {
    let mut mapping = start();
    mapping.movie_delay_ticks = observed().first_dts * 15;
    let window = begun(mapping)
        .complete(end(), observed())
        .expect("complete window with legacy movie delay");
    let legacy = RecordingPresentation {
        audio_start: observed().first_dts,
        audio_frames: 5120,
        leading_frames: 2048,
        trailing_frames: 307,
        video_start: None,
        video_end: None,
    };
    let old = fmp4::init_segment_for_native_recording(None, &audio(), &legacy)
        .expect("legacy native recording header");
    let new = fmp4::init_segment_for_programme_recording(&audio(), &window.presentation())
        .expect("explicit programme recording header");
    assert_eq!(old, new);
    let parsed = movie(&old);
    assert_eq!(parsed.mvhd.duration, (56_192 + 2765) * 15);
    let edits = &parsed.trak[0]
        .edts
        .as_ref()
        .expect("legacy audio edit container")
        .elst
        .as_ref()
        .expect("legacy audio edit list")
        .entries;
    assert_eq!(edits[0].media_time, None);
    assert_eq!(edits[0].segment_duration, 56_192 * 15);
}

#[test]
fn programme_header_refuses_otherwise_valid_stereo_configuration() {
    let stereo =
        AacConfig::from_audio_specific_config(&[0x11, 0x90]).expect("valid stereo AAC fixture");
    assert_eq!(
        fmp4::init_segment_for_programme_recording(&stereo, &complete().presentation()),
        Err(TimingError::Unsupported)
    );
}

#[test]
fn completed_window_can_describe_immutable_incomplete_prefix_without_reopening() {
    let mut ledger = begun(start());
    let old = ledger.prefix_presentation(observed()).expect("live prefix");
    let window = ledger
        .complete(end(), observed())
        .expect("closed scalar geometry");
    let prefix = window
        .prefix_presentation(observed())
        .expect("read-only closed-window prefix");
    assert_eq!(prefix, old);
    assert_eq!(prefix.trailing_frames(), 0);
    assert_eq!(
        prefix.completion(),
        ProgrammePresentationCompletion::IncompletePrefix
    );
    assert_eq!(window.presentation().trailing_frames(), 307);
    assert_eq!(
        ledger.prefix_presentation(observed()),
        Err(TimingError::Closed)
    );
    assert_eq!(ledger.completed(), Some(&window));
}

#[test]
fn completed_window_prefix_rejects_future_or_foreign_original_spans() {
    let window = complete();
    for span in [
        ObservedProgrammeSpan {
            first_dts: 56_192,
            end_dts: 62_336,
            access_units: 6,
        },
        ObservedProgrammeSpan {
            first_dts: 56_193,
            end_dts: 61_313,
            access_units: 5,
        },
    ] {
        assert_eq!(
            window.prefix_presentation(span),
            Err(TimingError::Inconsistent)
        );
    }
}
