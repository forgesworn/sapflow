//! SRTCP unprotect (`sapflow_rtp::srtp::Inbound`) with fixed test keys, in the
//! record format of `srtp_unprotect`, including the E flag and 31-bit index
//! and a plaintext round-trip mode. See `sapflow_rtp::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::srtcp_unprotect(data);
});
