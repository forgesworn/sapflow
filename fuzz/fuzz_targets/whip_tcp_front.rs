//! The ICE-TCP listener's per-connection front (`whip_tcp_front.rs`): an
//! optional PROXY protocol v2 header from the trusted proxy, then RFC 4571
//! frames, cut two ways; arbitrary bytes in, no I/O.
#![no_main]

#[cfg(not(feature = "fuzzing"))]
compile_error!("whip_tcp_front requires the fuzzing feature");

use libfuzzer_sys::fuzz_target;

// Compile the production module, rather than a second fuzz-only copy.
#[allow(dead_code)]
#[path = "../../crates/sapflow-server/src/whip_tcp_front.rs"]
mod whip_tcp_front;

fuzz_target!(|data: &[u8]| whip_tcp_front::check(data));
