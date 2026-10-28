//! Arbitrary RTMP video and audio tag bodies, as an RTMP publisher sends them.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_hls::{FlvIngest, MuxerConfig, TrackSet};

fuzz_target!(|data: &[u8]| {
    let (quota, data) = if let Some(rest) = data.strip_prefix(b"FBUD") {
        if rest.len() < 2 {
            return;
        }
        let maximum = usize::from(u16::from_le_bytes([rest[0], rest[1]]));
        (
            Some((
                std::sync::Arc::new(sapflow_core::fragment::FragmentQuota::new(maximum)),
                maximum,
            )),
            &rest[2..],
        )
    } else {
        (None, data)
    };
    let mut ing = match &quota {
        Some((quota, _)) => FlvIngest::with_fragment_quota(MuxerConfig::default(), quota.clone()),
        None => FlvIngest::new(MuxerConfig::default()),
    };
    // Keep historical three-kind crash inputs unchanged. The explicit prefix
    // adds track-declaration records for new seeds without reinterpreting them.
    let declarations = data.starts_with(b"TRKS");
    let mut rest = if declarations { &data[4..] } else { data };
    // Each record: kind (1), timestamp (4), length (2), body.
    while rest.len() >= 7 {
        let kind = rest[0];
        let ms = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]);
        let len = usize::from(u16::from_be_bytes([rest[5], rest[6]])).min(rest.len() - 7);
        let body = &rest[7..7 + len];
        rest = &rest[7 + len..];
        match kind % if declarations { 4 } else { 3 } {
            0 => {
                if ing
                    .video(ms, body, std::time::SystemTime::UNIX_EPOCH)
                    .is_err()
                {
                    break;
                }
            }
            1 => {
                if ing
                    .audio_at(ms, body, std::time::SystemTime::UNIX_EPOCH)
                    .is_err()
                {
                    break;
                }
            }
            2 => ing.expect_audio(body.first().is_some_and(|b| b & 1 == 1)),
            _ => {
                let tracks = match body.first().copied().unwrap_or(0) % 3 {
                    0 => TrackSet::AudioOnly,
                    1 => TrackSet::VideoOnly,
                    _ => TrackSet::AudioVideo,
                };
                if ing
                    .declare_tracks(tracks, std::time::SystemTime::UNIX_EPOCH)
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    if let Some((quota, maximum)) = quota {
        assert!(quota.used() <= maximum);
        drop(ing);
        assert_eq!(quota.used(), 0);
    }
});
