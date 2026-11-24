//! Bounded RTMP chunk-reassembly benchmark, with no sockets or media.
//!
//! One generated 12 kB message, written in 4096-byte chunks, is read back
//! over and over. Build this same example against two revisions of the
//! chunk reader and run the binaries in alternating order. It is a library
//! timing, not a server or network result.
use sapflow_rtmp::chunk::{ChunkReader, ChunkWriter, Read};
use std::hint::black_box;
use std::time::Instant;

const MESSAGE_BYTES: usize = 12_000;
const CHUNK_SIZE: u32 = 4096;

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let rounds: u64 = match args.as_slice() {
        [] => 5_000_000,
        [flag, count] if flag == "--rounds" => match count.parse() {
            Ok(n) if (1..=2_000_000_000).contains(&n) => n,
            _ => {
                eprintln!("rounds must be between 1 and 2000000000");
                return std::process::ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("use --rounds COUNT");
            return std::process::ExitCode::from(2);
        }
    };
    let payload: Vec<u8> = (0..MESSAGE_BYTES).map(|i| i as u8).collect();
    let mut writer = ChunkWriter::default();
    writer.set_chunk_size(CHUNK_SIZE);
    let mut bytes = Vec::new();
    writer.write(&mut bytes, 6, 9, 1, 40, &payload);
    let mut reader = ChunkReader::new(40 << 20);
    if reader.set_chunk_size(CHUNK_SIZE).is_err() {
        return std::process::ExitCode::FAILURE;
    }
    let mut verified = 0u64;
    let started = Instant::now();
    for _ in 0..rounds {
        let mut rest = black_box(bytes.as_slice());
        loop {
            match reader.read(rest) {
                Ok(Read::Chunk { consumed, message }) => {
                    rest = &rest[consumed..];
                    if let Some(message) = message {
                        verified += u64::from(message.payload.len() == MESSAGE_BYTES);
                        black_box(message);
                    }
                }
                Ok(Read::NeedMore) => break,
                Err(_) => return std::process::ExitCode::FAILURE,
            }
        }
    }
    let elapsed = started.elapsed().as_nanos();
    assert_eq!(verified, rounds, "every message is read back whole");
    println!(
        "{{\"profile\":\"chunk-reassembly\",\"message_bytes\":{MESSAGE_BYTES},\"chunk_size\":{CHUNK_SIZE},\"operations\":{rounds},\"elapsed_ns\":{elapsed},\"verified_messages\":{verified}}}"
    );
    std::process::ExitCode::SUCCESS
}
