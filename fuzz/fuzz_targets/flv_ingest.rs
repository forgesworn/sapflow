//! Arbitrary RTMP video and audio tag bodies, as an RTMP publisher sends them.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_hls::{FlvIngest, MuxerConfig};

fuzz_target!(|data: &[u8]| {
    let mut ing = FlvIngest::new(MuxerConfig::default());
    let mut rest = data;
    // Each record: kind (1), timestamp (4), length (2), body.
    while rest.len() >= 7 {
        let kind = rest[0];
        let ms = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]);
        let len = usize::from(u16::from_be_bytes([rest[5], rest[6]])).min(rest.len() - 7);
        let body = &rest[7..7 + len];
        rest = &rest[7 + len..];
        match kind % 3 {
            0 => {
                if ing.video(ms, body, std::time::SystemTime::UNIX_EPOCH).is_err() {
                    return;
                }
            }
            1 => ing.audio(ms, body),
            _ => ing.expect_audio(body.first().is_some_and(|b| b & 1 == 1)),
        }
    }
});
