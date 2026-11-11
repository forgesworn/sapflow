//! Our H.264 RTP depacketiser (`sapflow_rtp::h264`): single NAL units,
//! STAP-A, FU-A, the refused interleaved types, losses and limits. Every
//! access unit must re-parse within its limits. See
//! `sapflow_rtp::fuzzing::media::h264_depacketise` for the input format.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::media::h264_depacketise(data);
});
