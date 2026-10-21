#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_signet_ffi::{SignetCredential, validate_login_contract};
fuzz_target!(|data: &[u8]| {
    if data.len() > 16_384 {
        return;
    }
    let _ = SignetCredential::restore(data.to_vec());
    if let Ok(text) = std::str::from_utf8(data) {
        let mut fields = text.splitn(5, '\n');
        let persona = fields.next().unwrap_or("");
        let challenge = fields.next().unwrap_or("");
        let origin = fields.next().unwrap_or("");
        let app = fields.next().unwrap_or("");
        let created_at = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let _ = validate_login_contract(persona, challenge, origin, app, created_at);
    }
});
