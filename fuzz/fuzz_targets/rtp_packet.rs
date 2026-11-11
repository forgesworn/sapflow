//! Our RTP packet view (`sapflow_rtp::rtp`), RFC 8285 extensions included:
//! every SRTP packet sapflow accepts is read by it. See
//! `sapflow_rtp::fuzzing::rtp_packet` for the checks.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::rtp_packet(data);
});
