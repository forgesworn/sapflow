//! A scripted `sapflow_dtls::Server` run against the test client: raw,
//! mutated, dropped, replayed and duplicated datagrams, timers and sealed
//! records, with invariants on state size, output per input, total output,
//! crypto work and timers after every step. See `sapflow_dtls::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_dtls::fuzzing::dtls_server(data);
});
