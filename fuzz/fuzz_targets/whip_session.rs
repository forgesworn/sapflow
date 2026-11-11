//! A scripted `sapflow_whip::Session` run against the scripted publisher:
//! raw and honest datagrams from two tuples, checks, DTLS flights, SRTP,
//! SRTCP and time, with nothing sent to a stranger and DTLS and RTCP only
//! to the selected tuple. See `sapflow_whip::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_whip::fuzzing::whip_session(data);
});
