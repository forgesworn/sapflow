//! Arbitrary frame timing into the LL-HLS muxer: decode-time steps, stalls,
//! steps backwards, keyframes anywhere and audio in between. After every
//! push the rendered playlist must keep the rules `playlist::RuleCheck`
//! holds it to: fixed `TARGETDURATION` and `PART-TARGET`, no `EXTINF` over
//! the target, the part rules, consistent discontinuities.
#![no_main]
use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use sapflow_hls::codec::{AacConfig, H264Config};
use sapflow_hls::{Muxer, MuxerConfig, playlist};

const SPS: &[u8] = &[
    0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03, 0x00, 0x08,
    0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
];
const PPS: &[u8] = &[0x68, 0xef, 0xbc, 0x80];

fuzz_target!(|data: &[u8]| {
    let Some(video) = H264Config::new(SPS, PPS) else {
        return;
    };
    let aac = AacConfig {
        object_type: 2,
        sample_rate_index: 3,
        sample_rate: 48000,
        channels: 2,
    };
    let mut m = Muxer::new(MuxerConfig::default(), video, Some(aac));
    let (mut dts, mut audio) = (0i64, 0i64);
    let mut rules = playlist::RuleCheck::default();
    // Each record: a step in ticks (2 bytes), then flags: bit 0 keyframe,
    // bit 1 the step is a stall (x 16), bit 2 the step goes backwards,
    // bits 3-5 AAC frames before the frame, bits 6-7 a composition offset.
    for rec in data.chunks_exact(3) {
        let step = i64::from(u16::from_le_bytes([rec[0], rec[1]]));
        let flags = rec[2];
        let step = if flags & 2 != 0 { step * 16 } else { step };
        dts += if flags & 4 != 0 { -step } else { step };
        for _ in 0..(flags >> 3) & 7 {
            m.push_audio(audio, Bytes::from_static(b"aac"));
            audio += 1920;
        }
        let pts = dts + i64::from(flags >> 6) * 3000;
        if m.push_video(
            dts,
            pts,
            flags & 1 == 1,
            Bytes::from_static(&[0; 8]),
            std::time::UNIX_EPOCH,
        )
        .is_err()
        {
            return;
        }
        if !m.has_content() {
            continue;
        }
        let pl = playlist::media(&m, "p");
        if let Err(e) = rules.check(&pl) {
            panic!("{e}\n{pl}");
        }
    }
});
