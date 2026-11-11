//! One STUN message through `sapflow_whip::stun` and the ICE-lite agent:
//! raw, or wrapped in an honest USERNAME, MESSAGE-INTEGRITY and
//! FINGERPRINT so the authenticated paths are reached. Answers are never
//! larger than the request. See `sapflow_whip::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_whip::fuzzing::stun_message(data);
});
