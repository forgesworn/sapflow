//! The ClientHello, Certificate (and X.509 leaf), ClientKeyExchange (and
//! P-256 point), CertificateVerify (and DER signature), Finished and SDP
//! fingerprint parsers, chosen by the first byte. See
//! `sapflow_dtls::fuzzing`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sapflow_dtls::fuzzing::dtls_handshake_messages(data);
});
