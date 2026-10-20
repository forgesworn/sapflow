//! Arbitrary bytes as an SRT publisher's MPEG-TS, in arbitrary chunks.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_hls::{MuxerConfig, TsIngest};

fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else { return };
    let mut ing = TsIngest::new(MuxerConfig::default());
    for chunk in rest.chunks(usize::from(split).max(1) * 7) {
        if ing.push(chunk, std::time::SystemTime::UNIX_EPOCH).is_err() {
            return;
        }
    }
});
