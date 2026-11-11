//! Our transport-cc recorder and feedback encoder (`sapflow_rtp::twcc`):
//! every packet recorded is reported received exactly once at its exact
//! arrival time, within the size cap, read back by our own parser. See
//! `sapflow_rtp::fuzzing::feedback::twcc_feedback`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::feedback::twcc_feedback(data);
});
