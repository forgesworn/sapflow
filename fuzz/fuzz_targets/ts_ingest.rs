//! Arbitrary bytes as an SRT publisher's MPEG-TS, in arbitrary chunks.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_hls::{MuxerConfig, TsIngest, playlist};

fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else {
        return;
    };
    let mut ing = TsIngest::new(MuxerConfig::default());
    let mut rules = playlist::RuleCheck::default();
    for chunk in rest.chunks(usize::from(split).max(1) * 7) {
        if ing.push(chunk, std::time::SystemTime::UNIX_EPOCH).is_err() {
            return;
        }
        if let Some(muxer) = ing.muxer().filter(|muxer| muxer.has_content()) {
            let rendered = playlist::media(muxer, "p");
            if let Err(error) = rules.check(&rendered) {
                panic!("{error}\n{rendered}");
            }
        }
    }
});
