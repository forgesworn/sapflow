//! Our receiver's start hold, sender report timeline and RTCP output
//! (`sapflow_rtp::receive`, `sapflow_rtp::timeline`): timestamps never go
//! back or run ahead of arrival time, and every datagram parses. See
//! `sapflow_rtp::fuzzing::feedback::sr_timeline`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::feedback::sr_timeline(data);
});
