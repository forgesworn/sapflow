//! Our Opus TOC and framing parser and Opus RTP depacketiser
//! (`sapflow_rtp::opus`): the input as one Opus packet, then as packets
//! with timestamp steps and losses. The timeline must have no holes but
//! at a reported discontinuity. See
//! `sapflow_rtp::fuzzing::media::opus_depacketise`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::media::opus_depacketise(data);
});
