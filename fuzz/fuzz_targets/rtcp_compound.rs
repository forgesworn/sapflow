//! Our RTCP compound parser (`sapflow_rtp::rtcp`): every SRTCP packet sapflow
//! accepts is read by it. What sapflow builds (RR, NACK, PLI, FIR) is rebuilt
//! from what was read and must read back the same. See
//! `sapflow_rtp::fuzzing::rtcp_compound`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::rtcp_compound(data);
});
