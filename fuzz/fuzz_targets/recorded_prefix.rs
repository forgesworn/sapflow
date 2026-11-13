//! The recording recovery scanner (`fmp4::recorded_prefix`) over damaged
//! files: raw bytes, or a real generated recording cut short and with bytes
//! flipped, as a crash, a power cut or a damaged disk would leave it.
#![no_main]
use std::io::Cursor;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use libfuzzer_sys::fuzz_target;
use sapflow_hls::fmp4::{self, RecordedPrefix};
use sapflow_hls::{MuxerConfig, PartLookup, TsIngest};

/// A recording as the server writes one: the init segment, then every part
/// rebased for the file. And its audio rate.
fn recording() -> &'static (Vec<u8>, Option<u32>) {
    static RECORDING: OnceLock<(Vec<u8>, Option<u32>)> = OnceLock::new();
    RECORDING.get_or_init(|| {
        let mut ingest = TsIngest::new(MuxerConfig::default());
        for bytes in
            include_bytes!("../../crates/sapflow-hls/tests/fixtures/testsrc-3s.ts").chunks(1316)
        {
            ingest
                .push(bytes, SystemTime::UNIX_EPOCH)
                .expect("generated fixture");
        }
        let mut muxer = ingest.take_muxer().expect("muxer");
        muxer.interrupt(SystemTime::UNIX_EPOCH);
        let rate = muxer.audio().map(|audio| audio.sample_rate);
        let mut file = fmp4::init_segment_for_tracks(muxer.video(), muxer.audio(), Duration::ZERO)
            .expect("init")
            .to_vec();
        let mut origin = None;
        for id in 0..muxer.next_part_id() {
            let PartLookup::Found(part) = muxer.part(id) else {
                panic!("generated part retained");
            };
            let (output, _) =
                fmp4::rebase_part_for_file_timed(&part.data, rate, &mut origin).expect("rebase");
            file.extend_from_slice(&output);
        }
        (file, rate)
    })
}

fn scan(bytes: &[u8], rate: Option<u32>) -> RecordedPrefix {
    fmp4::recorded_prefix(&mut Cursor::new(bytes), bytes.len() as u64, rate)
        .expect("in-memory reads never fail")
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let (recorded, recorded_rate) = recording();
    let rate = match mode >> 4 {
        0 => *recorded_rate,
        1 => None,
        2 => Some(0),
        _ => Some(u32::from(mode) * 1000),
    };
    let bytes = if mode & 1 == 0 {
        rest.to_vec()
    } else {
        // Two bytes of cut length (as a fraction of the file), then
        // (offset, xor) triples.
        let mut file = recorded.clone();
        if let [high, low, flips @ ..] = rest {
            let fraction = u64::from(u16::from_be_bytes([*high, *low]));
            let keep = (file.len() as u64 * (fraction + 1) / 65_536) as usize;
            file.truncate(keep);
            for flip in flips.chunks_exact(3) {
                if file.is_empty() {
                    break;
                }
                let at = usize::from(u16::from_be_bytes([flip[0], flip[1]])) * 7 % file.len();
                file[at] ^= flip[2];
            }
        }
        file
    };
    let prefix = scan(&bytes, rate);
    assert!(prefix.bytes <= bytes.len() as u64);
    assert!(prefix.fragments <= bytes.len() as u64 / 16);
    if prefix.fragments == 0 {
        assert_eq!(prefix, RecordedPrefix::default());
    }
    // What was decodable stays decodable, and nothing more is found, once
    // the file is cut where its prefix ends.
    assert_eq!(scan(&bytes[..prefix.bytes as usize], rate), prefix);
});
