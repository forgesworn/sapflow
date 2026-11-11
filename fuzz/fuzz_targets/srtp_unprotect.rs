//! SRTP unprotect (`sapflow_rtp::srtp::Inbound`) with fixed test keys: a mode
//! byte, then `[u16 length, packet]` records into one context, so mutation
//! reaches index estimation, the sequence wrap and the replay window. A mode
//! bit makes the records plaintext that must round-trip through protect and
//! unprotect. See `sapflow_rtp::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::srtp_unprotect(data);
});
