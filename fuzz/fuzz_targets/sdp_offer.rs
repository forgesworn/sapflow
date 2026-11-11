//! One SDP offer through `sapflow_whip::sdp`: parse, answer, and parse the
//! answer back. See `sapflow_whip::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_whip::fuzzing::sdp_offer(data);
});
