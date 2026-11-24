//! Bounded MPEG-TS ingest benchmark, with no sockets or external media.
//!
//! Pushes a small generated fixture (FFmpeg testsrc and sine, three seconds)
//! through a fresh [`TsIngest`] in SRT-sized pieces, over and over: demux,
//! access units and the muxer together. Build this same example against two
//! revisions and run the binaries in alternating order. It is a library
//! timing, not a server or network result.
use sapflow_hls::{MuxerConfig, TsIngest};
use std::hint::black_box;
use std::time::{Duration, Instant, SystemTime};

const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/testsrc-3s.ts");
const TS_SIZE: usize = 188;
/// Seven transport packets: an SRT live payload.
const PIECE: usize = 7 * TS_SIZE;

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let rounds: u32 = match args.as_slice() {
        [] => 2_000,
        [flag, count] if flag == "--rounds" => match count.parse() {
            Ok(n) if (1..=1_000_000).contains(&n) => n,
            _ => {
                eprintln!("rounds must be between 1 and 1000000");
                return std::process::ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("use --rounds COUNT");
            return std::process::ExitCode::from(2);
        }
    };
    let mut packets = 0u64;
    let mut accepted = 0u64;
    let started = Instant::now();
    for _ in 0..rounds {
        let mut ingest = TsIngest::new(MuxerConfig::default());
        let mut now = SystemTime::UNIX_EPOCH;
        for piece in FIXTURE.chunks(PIECE) {
            now += Duration::from_millis(2);
            accepted += u64::from(black_box(ingest.push(black_box(piece), now)).is_ok());
            packets += (piece.len() / TS_SIZE) as u64;
        }
        black_box(&ingest);
    }
    let elapsed = started.elapsed().as_nanos();
    let pieces = u64::from(rounds) * FIXTURE.len().div_ceil(PIECE) as u64;
    assert_eq!(accepted, pieces, "every piece of the fixture is accepted");
    println!(
        "{{\"profile\":\"ts-ingest\",\"fixture_bytes\":{},\"rounds\":{rounds},\"operations\":{packets},\"elapsed_ns\":{elapsed},\"accepted_pushes\":{accepted}}}",
        FIXTURE.len()
    );
    std::process::ExitCode::SUCCESS
}
