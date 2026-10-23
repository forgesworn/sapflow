use crate::recording_checkpoint::*;

fn value(video: bool) -> PersistedCheckpoint {
    PersistedCheckpoint {
        info: CheckpointInfo {
            context: CheckpointContext {
                binding: CheckpointBinding {
                    recording: [7; 32],
                    source_file: [9; 32],
                },
                media_epoch_ns: 123_456,
                initial_header_len: 16,
                initial_header_sha256: [11; 32],
            },
            facts: CheckpointFacts {
                sequence: 1,
                audio_units: 4,
                video_units: u64::from(video),
                audio_start: 17,
                audio_end: 4113,
                audio_container_end: 4113,
                final_trailing_frames: 0,
                complete_audio_eof: false,
                video_start: video.then_some(31),
                video_end: video.then_some(3031),
                first_leading_frames: 2112,
                completed_generation_count: 0,
            },
            committed_offset: 816,
            fragment_region_bytes: 800,
            fragment_region_sha256: [13; 32],
        },
        header: (0..16).collect(),
    }
}
fn roundtrip(v: &PersistedCheckpoint) -> Vec<u8> {
    let bytes = encode(v.as_ref()).expect("valid native snapshot");
    let decoded = decode(&bytes, &v.info.context.binding).expect("canonical decode");
    assert_eq!(&decoded, v);
    assert_eq!(
        encode(decoded.as_ref()).expect("canonical re-encode"),
        bytes
    );
    bytes
}
fn resign(bytes: &mut [u8]) {
    let n = bytes.len() - 32;
    let digest = metadata_sha256(&bytes[..n]);
    bytes[n..].copy_from_slice(&digest);
}
fn successor(prior: &PersistedCheckpoint) -> PersistedCheckpoint {
    let mut n = prior.clone();
    n.info.facts.sequence += 1;
    n.info.facts.audio_units += 1;
    n.info.facts.audio_end += 1024;
    n.info.facts.audio_container_end += 1024;
    n.info.committed_offset += 123;
    n.info.fragment_region_bytes += 123;
    n
}

#[test]
fn checkpoint_canonical_ao_and_av_keep_opaque_headers_and_delayed_starts() {
    for video in [false, true] {
        let v = value(video);
        let b = roundtrip(&v);
        assert_eq!(&b[..8], b"SFCP0001");
        assert_eq!(&b[512..528], v.header);
        assert_eq!(
            u64::from_be_bytes(b[144..152].try_into().expect("fixed field")),
            17
        );
    }
}
#[test]
fn checkpoint_sha256_known_answer() {
    assert_eq!(
        metadata_sha256(b"abc"),
        [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad
        ]
    );
}
#[test]
fn checkpoint_actual_video_only_sealed_prefix_and_zero_short_audio_are_valid() {
    let mut v = value(true);
    v.info.facts.audio_units = 0;
    v.info.facts.audio_end = 17;
    v.info.facts.audio_container_end = 17;
    roundtrip(&v);
    for units in [1, 2] {
        let mut v = value(false);
        v.info.facts.audio_units = units;
        v.info.facts.audio_end = 17 + units * 1024;
        v.info.facts.audio_container_end = v.info.facts.audio_end;
        roundtrip(&v);
    }
}
#[test]
fn checkpoint_header_and_slot_exact_bounds_and_all_representative_truncations() {
    for length in [1, MAX_HEADER_BYTES] {
        let mut v = value(false);
        v.header = vec![0xa5; length];
        v.info.context.initial_header_len = length as u32;
        v.info.committed_offset = length as u64 + v.info.fragment_region_bytes;
        let b = roundtrip(&v);
        assert_eq!(b.len(), 512 + length + 32);
        let mut extra = b.clone();
        extra.push(0);
        assert!(decode(&extra, &v.info.context.binding).is_err());
    }
    let v = value(false);
    let b = roundtrip(&v);
    for end in 0..b.len() {
        assert!(
            decode(&b[..end], &v.info.context.binding).is_err(),
            "prefix {end}"
        );
    }
    for n in [0, MAX_HEADER_BYTES + 1] {
        let mut v = value(false);
        v.header = vec![0; n];
        v.info.context.initial_header_len = n as u32;
        assert!(encode(v.as_ref()).is_err());
    }
}
#[test]
fn checkpoint_reserved_versions_flags_clocks_and_format_checked_after_valid_checksum() {
    let v = value(false);
    let b = roundtrip(&v);
    for at in [0, 8, 10, 28, 29, 30, 31, 100, 101, 102, 103, 200, 204]
        .into_iter()
        .chain(272..512)
    {
        let mut m = b.clone();
        m[at] ^= 0x80;
        resign(&mut m);
        assert_eq!(
            decode(&m, &v.info.context.binding).expect_err("reserved/format refusal"),
            CheckpointError::Unsupported,
            "offset {at}"
        );
    }
    for flags in [0_u32, 1, 8, 0x8000_0002] {
        let mut m = b.clone();
        m[12..16].copy_from_slice(&flags.to_be_bytes());
        resign(&mut m);
        assert_eq!(
            decode(&m, &v.info.context.binding).expect_err("flag refusal"),
            CheckpointError::Unsupported
        );
    }
}
#[test]
fn checkpoint_corrupt_digest_and_valid_foreign_binding_are_different() {
    let v = value(false);
    let mut b = roundtrip(&v);
    b[32] ^= 1;
    assert_eq!(
        decode(&b, &v.info.context.binding).expect_err("corruption"),
        CheckpointError::Checksum
    );
    resign(&mut b);
    assert_eq!(
        decode(&b, &v.info.context.binding).expect_err("valid foreign identity"),
        CheckpointError::Binding
    );
}
#[test]
fn checkpoint_semantic_decode_checks_reach_past_checksum() {
    let v = value(false);
    let b = roundtrip(&v);
    for (at, n) in [
        (128, 0_u64),
        (152, 4114),
        (160, 4112),
        (168, 1),
        (176, 1),
        (184, 5),
        (104, 815),
        (112, 0),
    ] {
        let mut m = b.clone();
        m[at..at + 8].copy_from_slice(&n.to_be_bytes());
        resign(&mut m);
        assert!(
            decode(&m, &v.info.context.binding).is_err(),
            "semantic field {at}"
        );
    }
    for (at, n) in [
        (24, 0_u32),
        (24, u32::MAX),
        (96, 15),
        (192, 8192),
        (196, 1024),
    ] {
        let mut m = b.clone();
        m[at..at + 4].copy_from_slice(&n.to_be_bytes());
        resign(&mut m);
        assert!(
            decode(&m, &v.info.context.binding).is_err(),
            "semantic field {at}"
        );
    }
}
#[test]
fn checkpoint_eof_requires_real_positive_extent_and_generation_but_allows_tail_zero() {
    for tail in [0, 1023] {
        let mut v = value(false);
        v.info.facts.complete_audio_eof = true;
        v.info.facts.completed_generation_count = 1;
        v.info.facts.final_trailing_frames = tail;
        v.info.facts.audio_container_end -= u64::from(tail);
        roundtrip(&v);
        v.info.facts.complete_audio_eof = false;
        if tail > 0 {
            assert!(encode(v.as_ref()).is_err());
        }
    }
    let mut v = value(false);
    v.info.facts.complete_audio_eof = true;
    assert!(
        encode(v.as_ref()).is_err(),
        "EOF has no measured generation"
    );
    v.info.facts.completed_generation_count = 1;
    v.info.facts.first_leading_frames = 4096;
    assert!(
        encode(v.as_ref()).is_err(),
        "no positive complete presentation"
    );
}
#[test]
fn checkpoint_counts_offsets_and_movie_clocks_are_checked_without_saturation() {
    let base = value(false);
    for units in [u64::MAX, u64::MAX / 1024] {
        let mut v = base.clone();
        v.info.facts.audio_units = units;
        assert!(encode(v.as_ref()).is_err());
    }
    let mut v = base.clone();
    v.info.committed_offset = i64::MAX as u64 + 1;
    v.info.fragment_region_bytes = v.info.committed_offset - 16;
    assert!(encode(v.as_ref()).is_err());
    let mut v = base.clone();
    v.info.facts.audio_start = i64::MAX as u64;
    v.info.facts.audio_end = v.info.facts.audio_start + 4096;
    v.info.facts.audio_container_end = v.info.facts.audio_end;
    assert!(encode(v.as_ref()).is_err());
    let mut v = base;
    v.info.facts.first_leading_frames = 8191;
    roundtrip(&v);
    v.info.facts.first_leading_frames = 8192;
    assert!(encode(v.as_ref()).is_err());
}
#[test]
fn checkpoint_successor_accepts_real_single_track_progress_and_repeated_generation_count() {
    let p = value(true);
    let mut n = successor(&p);
    roundtrip(&n);
    validate_successor(&p.info, &n.info).expect("audio-only fragment progress");
    n = p.clone();
    n.info.facts.sequence += 1;
    n.info.committed_offset += 99;
    n.info.fragment_region_bytes += 99;
    n.info.facts.video_units += 1;
    n.info.facts.video_end = Some(6031);
    validate_successor(&p.info, &n.info).expect("video-only fragment progress");
}
#[test]
fn checkpoint_successor_rejects_context_drift_missing_progress_and_terminal_eof() {
    let p = value(true);
    let n = successor(&p);
    for k in 0..12 {
        let mut m = n.clone();
        match k {
            0 => m.info.context.binding.recording[0] ^= 1,
            1 => m.info.context.binding.source_file[0] ^= 1,
            2 => m.info.context.media_epoch_ns += 1,
            3 => m.info.context.initial_header_sha256[0] ^= 1,
            4 => m.info.facts.sequence = p.info.facts.sequence,
            5 => m.info.facts.sequence += 1,
            6 => {
                m.info.committed_offset = p.info.committed_offset;
                m.info.fragment_region_bytes = p.info.fragment_region_bytes;
            }
            7 => m.info.facts.first_leading_frames += 1,
            8 => m.info.facts.video_start = Some(32),
            9 => {
                m.info.facts.video_start = None;
                m.info.facts.video_end = None;
                m.info.facts.video_units = 0;
            }
            10 => {
                m.info.facts.audio_units = p.info.facts.audio_units;
                m.info.facts.audio_end = p.info.facts.audio_end;
                m.info.facts.audio_container_end = p.info.facts.audio_container_end;
            }
            _ => {
                m.info.facts.audio_start += 1;
                m.info.facts.audio_end += 1;
                m.info.facts.audio_container_end += 1;
            }
        }
        assert!(
            validate_successor(&p.info, &m.info).is_err(),
            "successor drift {k}"
        );
    }
    let mut done = p;
    done.info.facts.complete_audio_eof = true;
    done.info.facts.completed_generation_count = 1;
    assert!(validate_successor(&done.info, &n.info).is_err());
}
#[test]
fn checkpoint_successor_validates_both_operands_and_generation_coverage() {
    let mut p = value(false);
    p.info.facts.completed_generation_count = 2;
    let mut n = successor(&p);
    n.info.facts.completed_generation_count = 1;
    assert!(validate_successor(&p.info, &n.info).is_err());
    n.info.facts.completed_generation_count = 2;
    validate_successor(&p.info, &n.info).expect("count can stay");
    p.info.facts.audio_end += 1;
    assert!(validate_successor(&p.info, &n.info).is_err());
}

#[test]
fn checkpoint_video_count_and_endpoint_coverage_must_agree() {
    let mut v = value(true);
    v.info.facts.video_units = 0;
    assert!(
        encode(v.as_ref()).is_err(),
        "no units cannot claim positive video extent"
    );
    v.info.facts.video_end = v.info.facts.video_start;
    roundtrip(&v);
    v.info.facts.video_units = 1;
    assert!(encode(v.as_ref()).is_err(), "positive video needs extent");
}

#[test]
fn checkpoint_checksum_valid_length_conflicts_are_facts_not_corrupt_slot_fallback() {
    let v = value(false);
    let original = roundtrip(&v);
    for (offset, values) in [
        (
            16,
            [
                0_u32,
                MAX_CHECKPOINT_BYTES as u32 + 1,
                original.len() as u32 - 1,
            ],
        ),
        (
            20,
            [
                0_u32,
                MAX_HEADER_BYTES as u32 + 1,
                v.header.len() as u32 + 1,
            ],
        ),
        (
            96,
            [
                0_u32,
                MAX_HEADER_BYTES as u32 + 1,
                v.header.len() as u32 + 1,
            ],
        ),
    ] {
        for declared in values {
            let mut changed = original.clone();
            changed[offset..offset + 4].copy_from_slice(&declared.to_be_bytes());
            assert_eq!(
                decode(&changed, &v.info.context.binding)
                    .expect_err("unsigned contradictory length"),
                CheckpointError::Checksum,
                "bad checksum at {offset}, declared {declared}",
            );
            resign(&mut changed);
            assert_eq!(
                decode(&changed, &v.info.context.binding).expect_err("signed contradictory length"),
                CheckpointError::Facts,
                "checksum-valid conflict at {offset}, declared {declared}",
            );
        }
    }
    for declared in [0, MAX_HEADER_BYTES as u32 + 1, v.header.len() as u32 + 1] {
        let mut changed = v.clone();
        changed.info.context.initial_header_len = declared;
        assert_eq!(
            encode(changed.as_ref()).expect_err("shared encode validator"),
            CheckpointError::Facts
        );
    }
    for size in [0, PREFIX_BYTES + 32, MAX_CHECKPOINT_BYTES + 1] {
        assert_eq!(
            decode(&vec![0; size], &v.info.context.binding).expect_err("actual-size admission"),
            CheckpointError::Envelope,
        );
    }
}

#[test]
fn checkpoint_sha256_multiblock_known_answer() {
    let input = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
    assert_eq!(input.len(), 112);
    assert_eq!(
        metadata_sha256(input),
        [
            0xcf, 0x5b, 0x16, 0xa7, 0x78, 0xaf, 0x83, 0x80, 0x03, 0x6c, 0xe5, 0x9e, 0x7b, 0x04,
            0x92, 0x37, 0x0b, 0x24, 0x9b, 0x11, 0xe8, 0xf0, 0x7a, 0x51, 0xaf, 0xac, 0x45, 0x03,
            0x7a, 0xfe, 0xe9, 0xd1,
        ]
    );
}

#[test]
fn checkpoint_sha256_max_header_pattern_matches_independent_vector_and_chunked_updates() {
    use sha2::{Digest, Sha256};
    // Expected value comes from the independently recorded Python/OpenSSL
    // hashlib oracle, not from metadata_sha256 or this Rust dependency.
    let input: Vec<u8> = (0..MAX_HEADER_BYTES).map(|n| (n % 251) as u8).collect();
    let expected = [
        0x4b, 0x64, 0x0d, 0x85, 0xab, 0x3b, 0xa3, 0x0f, 0xd0, 0x2c, 0x9f, 0xc9, 0xdb, 0x4a, 0x89,
        0x28, 0xf4, 0x16, 0x32, 0x2a, 0xd2, 0x70, 0x22, 0xea, 0x58, 0xa6, 0x5a, 0xae, 0xe6, 0x8a,
        0x4d, 0xf2,
    ];
    assert_eq!(input.len(), 65536);
    assert_eq!(metadata_sha256(&input), expected);
    for chunk_size in [1, 63, 64, 65, 257, 4096] {
        let mut hash = Sha256::new();
        for chunk in input.chunks(chunk_size) {
            hash.update(chunk);
        }
        let actual: [u8; 32] = hash.finalize().into();
        assert_eq!(actual, expected, "chunk size {chunk_size}");
    }
}
