//! Our receiver report statistics (`sapflow_rtp::stats`, RFC 3550 A.3 and
//! A.8) against a model that keeps every counted sequence number: counts,
//! loss, fraction, DLSR and the RR round trip must match. See
//! `sapflow_rtp::fuzzing::feedback::rr_stats`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::feedback::rr_stats(data);
});
