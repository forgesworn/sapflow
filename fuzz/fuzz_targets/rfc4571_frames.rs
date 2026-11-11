//! A byte stream through the RFC 4571 frame decoder in chunks, re-encoded
//! and compared. See `sapflow_whip::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_whip::fuzzing::rfc4571_frames(data);
});
