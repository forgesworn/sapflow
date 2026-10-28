//! Arbitrary bytes as an SRT publisher's MPEG-TS, in arbitrary chunks.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_hls::{IngestError, MuxerConfig, TsIngest, Update, playlist};

fuzz_target!(|data: &[u8]| {
    let (quota, data) = if let Some(rest) = data.strip_prefix(b"FBUD") {
        if rest.len() < 2 {
            return;
        }
        let maximum = usize::from(u16::from_le_bytes([rest[0], rest[1]]));
        (
            Some((
                std::sync::Arc::new(sapflow_core::fragment::FragmentQuota::new(maximum)),
                maximum,
            )),
            &rest[2..],
        )
    } else {
        (None, data)
    };
    let Some((&split, rest)) = data.split_first() else {
        return;
    };
    let mut ing = match &quota {
        Some((quota, _)) => TsIngest::with_fragment_quota(MuxerConfig::default(), quota.clone()),
        None => TsIngest::new(MuxerConfig::default()),
    };
    let mut rules = playlist::RuleCheck::default();
    for chunk in rest.chunks(usize::from(split).max(1) * 7) {
        if ing.push(chunk, std::time::SystemTime::UNIX_EPOCH).is_err() {
            break;
        }
        if let Some(muxer) = ing.muxer().filter(|muxer| muxer.has_content()) {
            let rendered = playlist::media(muxer, "p");
            if let Err(error) = rules.check(&rendered) {
                panic!("{error}\n{rendered}");
            }
        }
    }
    // Real EOF differs from starvation. Exercise valid/truncated terminal
    // PES and semantic error cleanup as well as ordinary streaming chunks.
    let _ = ing.finish(std::time::SystemTime::UNIX_EPOCH);
    if let Some(muxer) = ing.muxer().filter(|muxer| muxer.has_content()) {
        let rendered = playlist::media(muxer, "p");
        if let Err(error) = rules.check(&rendered) {
            panic!("{error}\n{rendered}");
        }
    }
    assert_eq!(
        ing.finish(std::time::SystemTime::UNIX_EPOCH),
        Ok(Update::default())
    );
    assert_eq!(
        ing.push(rest, std::time::SystemTime::UNIX_EPOCH),
        Err(IngestError::InputFinished)
    );
    if let Some((quota, maximum)) = quota {
        assert!(quota.used() <= maximum);
        drop(ing);
        assert_eq!(quota.used(), 0);
    }
});
