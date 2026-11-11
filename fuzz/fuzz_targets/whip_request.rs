//! The WHIP listener's request rules (`whip_request.rs`) over the heads
//! `http_lite.rs` parses: arbitrary bytes in, no I/O.
#![no_main]

#[cfg(not(feature = "fuzzing"))]
compile_error!("whip_request requires the fuzzing feature");

use libfuzzer_sys::fuzz_target;

// Compile the production modules, rather than a second fuzz-only parser.
#[allow(dead_code)]
#[path = "../../crates/sapflow-server/src/http_lite.rs"]
mod http_lite;
#[allow(dead_code)]
#[path = "../../crates/sapflow-server/src/whip_request.rs"]
mod whip_request;

fuzz_target!(|data: &[u8]| whip_request::check(data));
