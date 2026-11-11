//! Our receiver (`sapflow_rtp::receive`) as the server drives it: video,
//! audio and RTX packets, clock steps forward and back. Buffers must stay
//! bounded, NACKs must name only missing packets, and timers must move
//! on. See `sapflow_rtp::fuzzing::media::reorder_nack`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_rtp::fuzzing::media::reorder_nack(data);
});
