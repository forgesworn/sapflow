//! AMF0 decoding (every RTMP command and metadata message), and that
//! re-encoding what was decoded is stable.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_rtmp::amf0;

fn encode(values: &[amf0::Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        amf0::encode(v, &mut out);
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let Ok(values) = amf0::decode_all(data) else {
        return;
    };
    let once = encode(&values);
    let again = amf0::decode_all(&once).expect("our own encoding decodes");
    assert_eq!(encode(&again), once, "encoding is stable");
});
