//! Two scripted publishers' `sapflow_whip::Session`s in one
//! `sapflow_whip::Router`, with strangers sending raw bytes and copies:
//! every tuple at most `amplification_limit` times what it sent, nothing to
//! a stranger that has sent only its own bytes, the deadline index exact
//! and every owned tuple's owner live. See `sapflow_whip::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_whip::fuzzing::whip_router(data);
});
