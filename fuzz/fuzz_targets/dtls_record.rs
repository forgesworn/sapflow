//! One datagram through DTLS record framing, handshake fragment framing,
//! epoch 1 AES-GCM opening with a fixed key, a seal and open round trip and
//! the replay window. See `sapflow_dtls::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_dtls::fuzzing::dtls_record(data);
});
