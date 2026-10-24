use crate::recording_checkpoint::{
    self as v1, CheckpointBinding, CheckpointContext, CheckpointError,
};
use crate::recording_programme::*;
use crate::recording_programme_checkpoint::*;

// Opaque generated metadata fixture, never playable-media or native EOF proof.
fn header() -> Vec<u8> {
    vec![0x5a; 32]
}
fn binding() -> CheckpointBinding {
    CheckpointBinding {
        recording: [7; 32],
        source_file: [9; 32],
    }
}
fn end() -> ProgrammeGenerationEnd {
    ProgrammeGenerationEnd {
        generation: 7,
        coded_frames: 15_360,
        queued: QueuedProgrammeFrames {
            preparation: 8192,
            programme_raw: 1517,
            programme_zero: 1248,
            block_padding: 307,
            codec_drain: 4096,
        },
    }
}
fn prefix() -> ProgrammeCheckpointInfo {
    ProgrammeCheckpointInfo {
        context: ProgrammeCheckpointContext {
            base: CheckpointContext {
                binding: binding(),
                media_epoch_ns: 0,
                initial_header_len: 32,
                initial_header_sha256: v1::metadata_sha256(&header()),
            },
            start: ProgrammeWindowStart {
                generation: 7,
                coded_origin: 48_000,
                preparation_frames: 8192,
                measured_displacement: 2048,
                discard_prefix_frames: 8192,
                movie_delay_ticks: 123,
            },
        },
        facts: ProgrammeCheckpointFacts {
            sequence: 1,
            span: ObservedProgrammeSpan {
                first_dts: 56_192,
                end_dts: 59_264,
                access_units: 3,
            },
            container_end: 59_264,
            leading_frames: 2048,
            trailing_frames: 0,
            window_end: None,
        },
        committed_offset: 132,
        fragment_region_bytes: 100,
        fragment_region_sha256: [13; 32],
    }
}
fn complete() -> ProgrammeCheckpointInfo {
    let mut info = prefix();
    info.facts.sequence = 2;
    info.facts.span.access_units = 5;
    info.facts.span.end_dts = 61_312;
    info.facts.container_end = 61_005;
    info.facts.trailing_frames = 307;
    info.facts.window_end = Some(end());
    info.committed_offset += 200;
    info.fragment_region_bytes += 200;
    info.fragment_region_sha256 = [17; 32];
    info
}
fn encoded(info: &ProgrammeCheckpointInfo) -> Vec<u8> {
    encode(ProgrammeCheckpointRef {
        info,
        header: &header(),
    })
    .expect("canonical generated programme metadata")
}
fn rehash(bytes: &mut [u8]) {
    let at = bytes.len() - 32;
    let hash = v1::metadata_sha256(&bytes[..at]);
    bytes[at..].copy_from_slice(&hash);
}

#[test]
fn programme_v2_roundtrips_prefix_and_complete_with_all_origins_and_components() {
    for info in [prefix(), complete()] {
        let bytes = encoded(&info);
        assert_eq!(&bytes[..8], b"SFCP0002");
        let parsed = decode(&bytes, &binding()).expect("bounded V2 roundtrip");
        assert_eq!(parsed.info, info);
        assert_eq!(parsed.header, header());
        assert_eq!(
            encode(parsed.as_ref()).expect("canonical V2 re-encode"),
            bytes
        );
    }
    let value = presentation(&complete()).expect("complete geometry presentation");
    assert_eq!(value.media_time(), 58_240);
    assert_eq!(value.presented_frames(), 2765);
    assert_eq!(
        value
            .movie_extent()
            .expect("explicit programme movie origin")
            .duration_ticks,
        41_598
    );
}

#[test]
fn programme_v2_accepts_actual_prefix_then_exact_complete_successor() {
    validate_successor(&prefix(), &complete()).expect("exact next sealed prefix");
    assert_eq!(
        presentation(&prefix())
            .expect("unknown-tail prefix")
            .trailing_frames(),
        0
    );
    assert_eq!(
        presentation(&complete())
            .expect("checked complete window")
            .trailing_frames(),
        307
    );
}

#[test]
fn programme_v2_keeps_head_only_prefix_without_claiming_positive_programme() {
    let mut info = prefix();
    info.facts.span.access_units = 1;
    info.facts.span.end_dts = info.facts.span.first_dts + 1024;
    info.facts.container_end = info.facts.span.end_dts;
    let value = decode(&encoded(&info), &binding()).expect("coded head-only prefix");
    assert!(value.info.facts.window_end.is_none());
    assert_eq!(
        presentation(&value.info)
            .expect("zero programme prefix")
            .presented_frames(),
        0
    );
}

#[test]
fn programme_v2_does_not_infer_coded_extent_from_queued_physical_input() {
    let mut info = complete();
    let mut supplied = end();
    supplied.coded_frames += 1024;
    info.facts.window_end = Some(supplied);
    let parsed = decode(&encoded(&info), &binding()).expect("independent coded extent");
    assert_eq!(parsed.info.facts.window_end, Some(supplied));
    assert_ne!(
        supplied.coded_frames,
        supplied.queued.total_queued().expect("queued input sum")
    );
}

#[test]
fn programme_v2_and_legacy_v1_never_reinterpret_each_other() {
    let info = prefix();
    let old = v1::CheckpointInfo {
        context: info.context.base,
        facts: v1::CheckpointFacts {
            sequence: 1,
            audio_units: 3,
            video_units: 0,
            audio_start: 56_192,
            audio_end: 59_264,
            audio_container_end: 59_264,
            final_trailing_frames: 0,
            complete_audio_eof: false,
            video_start: None,
            video_end: None,
            first_leading_frames: 2048,
            completed_generation_count: 0,
        },
        committed_offset: 132,
        fragment_region_bytes: 100,
        fragment_region_sha256: [13; 32],
    };
    let bytes = v1::encode(v1::CheckpointRef {
        info: &old,
        header: &header(),
    })
    .expect("unchanged V1 fixture");
    assert_eq!(
        decode(&bytes, &binding()),
        Err(CheckpointError::Unsupported)
    );
    assert_eq!(
        v1::decode(&encoded(&info), &binding()),
        Err(CheckpointError::Unsupported)
    );
}

#[test]
fn programme_v2_checksum_valid_origin_head_end_and_clock_conflicts_are_fatal() {
    let bytes = encoded(&complete());
    for (at, mask) in [
        (279, 1),
        (303, 1),
        (195, 1),
        (199, 1),
        (159, 1),
        (167, 1),
        (327, 0x20),
        (335, 1),
    ] {
        let mut changed = bytes.clone();
        changed[at] ^= mask;
        rehash(&mut changed);
        assert!(
            matches!(
                decode(&changed, &binding()),
                Err(CheckpointError::Facts | CheckpointError::Overflow)
            ),
            "conflict at {at}"
        );
    }
}

#[test]
fn programme_v2_refuses_unknown_flags_reserved_fields_and_format_tags() {
    let bytes = encoded(&prefix());
    for at in [7, 9, 11, 15, 28, 29, 30, 31, 100, 136, 168, 204, 360, 511] {
        let mut changed = bytes.clone();
        changed[at] ^= 0x40;
        rehash(&mut changed);
        assert_eq!(
            decode(&changed, &binding()),
            Err(CheckpointError::Unsupported),
            "unsupported at {at}"
        );
    }
}

#[test]
fn programme_v2_prefix_cannot_hide_complete_end_fields_or_a_final_tail() {
    let bytes = encoded(&prefix());
    for at in [191, 199, 327, 335, 343, 351, 359] {
        let mut changed = bytes.clone();
        changed[at] = 1;
        rehash(&mut changed);
        assert_eq!(
            decode(&changed, &binding()),
            Err(CheckpointError::Facts),
            "unexpected end field at {at}"
        );
    }
}

#[test]
fn programme_v2_independent_binding_refuses_checksum_valid_foreign_identity() {
    let mut expected = binding();
    expected.source_file[0] ^= 1;
    assert_eq!(
        decode(&encoded(&prefix()), &expected),
        Err(CheckpointError::Binding)
    );
}

#[test]
fn programme_v2_checksum_and_envelope_length_failures_remain_distinct() {
    let mut bytes = encoded(&prefix());
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    assert_eq!(decode(&bytes, &binding()), Err(CheckpointError::Checksum));
    let mut bytes = encoded(&prefix());
    bytes[19] ^= 1;
    rehash(&mut bytes);
    assert_eq!(decode(&bytes, &binding()), Err(CheckpointError::Facts));
    assert_eq!(
        decode(&vec![0; MAX_CHECKPOINT_BYTES + 1], &binding()),
        Err(CheckpointError::Envelope)
    );
}

#[test]
fn programme_v2_maximum_header_is_bounded_and_canonical() {
    let header = vec![0x37; MAX_HEADER_BYTES];
    let mut info = complete();
    info.context.base.initial_header_len = MAX_HEADER_BYTES as u32;
    info.context.base.initial_header_sha256 = v1::metadata_sha256(&header);
    info.committed_offset = MAX_HEADER_BYTES as u64 + info.fragment_region_bytes;
    let bytes = encode(ProgrammeCheckpointRef {
        info: &info,
        header: &header,
    })
    .expect("maximum V2 envelope");
    assert_eq!(bytes.len(), 66_080);
    assert_eq!(bytes.len(), MAX_CHECKPOINT_BYTES);
    assert_eq!(
        decode(&bytes, &binding())
            .expect("maximum V2 decode")
            .header,
        header
    );
}

#[test]
fn programme_v2_terminal_context_byte_and_media_rollbacks_refuse_successors() {
    let prior = prefix();
    let mut next = complete();
    let mut changed_context = next;
    changed_context.context.start.movie_delay_ticks += 1;
    let mut same_sequence = next;
    same_sequence.facts.sequence = 1;
    let mut same_bytes = next;
    same_bytes.committed_offset = prior.committed_offset;
    same_bytes.fragment_region_bytes = prior.fragment_region_bytes;
    for invalid in [changed_context, same_sequence, same_bytes] {
        assert_eq!(
            validate_successor(&prior, &invalid),
            Err(CheckpointError::Successor)
        );
    }
    next.facts.sequence = 3;
    next.facts.window_end = None;
    next.facts.trailing_frames = 0;
    next.facts.span.access_units = 6;
    next.facts.span.end_dts += 1024;
    next.facts.container_end = next.facts.span.end_dts;
    next.committed_offset += 200;
    next.fragment_region_bytes += 200;
    assert_eq!(
        validate_successor(&complete(), &next),
        Err(CheckpointError::Successor)
    );
}
