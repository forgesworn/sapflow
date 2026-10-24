//! Generated cache-storage model, not a full Muxer/server or media-quality test.
//! Run each variant as a fresh process under /usr/bin/time -l after building.

use bytes::{Bytes, BytesMut};
use sapflow_hls::muxer::{Initialization, Part, SegmentData};
use std::{error::Error, hint::black_box, sync::Arc, time::Duration};

#[derive(Clone, Copy)]
enum Variant {
    Shared,
    Joined,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Joined => "joined",
        }
    }
}

struct Config {
    variant: Variant,
    streams: usize,
    segments: usize,
    parts: usize,
    part_bytes: usize,
}

impl Config {
    fn original_bytes(&self) -> Result<usize, Box<dyn Error>> {
        if !(1..=1000).contains(&self.streams)
            || !(1..=1000).contains(&self.segments)
            || !(1..=4096).contains(&self.parts)
            || !(1..=64 * 1024 * 1024).contains(&self.part_bytes)
        {
            return Err("dimensions must be positive and within their finite limits".into());
        }
        let count = self
            .streams
            .checked_mul(self.segments)
            .and_then(|n| n.checked_mul(self.parts))
            .filter(|n| *n <= 1_000_000)
            .ok_or("part count exceeds checked metadata limit of 1000000")?;
        let bytes = count
            .checked_mul(self.part_bytes)
            .ok_or("payload model overflow")?;
        if bytes
            .checked_mul(2)
            .is_none_or(|n| n > 2 * 1024 * 1024 * 1024usize)
        {
            return Err("joined logical owned payload exceeds 2 GiB".into());
        }
        Ok(bytes)
    }
}

enum Backing {
    Shared(SegmentData),
    Joined(Bytes),
}

impl Backing {
    fn chunks(&self) -> &[Bytes] {
        match self {
            Self::Shared(data) => data.chunks(),
            Self::Joined(data) => std::slice::from_ref(data),
        }
    }
}

struct CachedSegment {
    parts: Vec<Arc<Part>>,
    body: Backing,
}

struct CachedStream {
    segments: Vec<CachedSegment>,
    // Like Muxer's independent part lookup index: additional handles, not copies.
    part_index: Vec<Arc<Part>>,
}

const WORD_STEP: u64 = 0x9e3779b97f4a7c15;

fn part_seed(stream: usize, segment: usize, part: usize) -> u64 {
    // The checked dimension limits keep these fields disjoint. Each part
    // receives a different word stream, rather than compressible page repeats.
    0x6a09e667f3bcc909 ^ ((stream as u64) << 32) ^ ((segment as u64) << 16) ^ part as u64
}

fn mix_word(mut word: u64) -> u64 {
    word = (word ^ (word >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
    word ^ (word >> 31)
}

fn generated_payload(stream: usize, segment: usize, part: usize, len: usize) -> Vec<u8> {
    let mut state = part_seed(stream, segment, part);
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        state = state.wrapping_add(WORD_STEP);
        let word = mix_word(state).to_le_bytes();
        let count = (len - bytes.len()).min(word.len());
        bytes.extend_from_slice(&word[..count]);
    }
    bytes
}

fn build(config: &Config) -> Vec<CachedStream> {
    (0..config.streams)
        .map(|stream| {
            let init = Arc::new(Initialization {
                id: 0,
                data: Bytes::from_static(b"generated cache model, not a media init segment"),
            });
            let mut part_index = Vec::with_capacity(config.segments * config.parts);
            let segments = (0..config.segments)
                .map(|segment| {
                    let parts: Vec<_> = (0..config.parts)
                        .map(|part| {
                            let data = generated_payload(stream, segment, part, config.part_bytes);
                            let part = Arc::new(Part {
                                initialization: init.clone(),
                                id: (segment * config.parts + part) as u64,
                                duration: Duration::from_millis(200),
                                span: Duration::from_millis(200),
                                independent: part == 0,
                                data: Bytes::from(data),
                            });
                            part_index.push(part.clone());
                            part
                        })
                        .collect();
                    let body = match config.variant {
                        Variant::Shared => Backing::Shared(
                            SegmentData::from_chunks(
                                parts.iter().map(|p| p.data.clone()).collect(),
                            )
                            .expect("checked finite model"),
                        ),
                        Variant::Joined => {
                            let mut joined =
                                BytesMut::with_capacity(config.parts * config.part_bytes);
                            for part in &parts {
                                joined.extend_from_slice(&part.data);
                            }
                            Backing::Joined(joined.freeze())
                        }
                    };
                    CachedSegment { parts, body }
                })
                .collect();
            CachedStream {
                segments,
                part_index,
            }
        })
        .collect()
}

fn verify(cache: &[CachedStream], config: &Config) -> (usize, u64) {
    let mut total = 0usize;
    let mut checksum = 0xcbf29ce484222325u64;
    for (stream_id, stream) in cache.iter().enumerate() {
        for (segment_id, segment) in stream.segments.iter().enumerate() {
            let mut body = segment.body.chunks().iter().flat_map(|b| b.iter().copied());
            for (part_id, part) in segment.parts.iter().enumerate() {
                assert!(Arc::ptr_eq(
                    part,
                    &stream.part_index[segment_id * config.parts + part_id]
                ));
                if let Backing::Shared(data) = &segment.body {
                    assert_eq!(data.chunks()[part_id].as_ptr(), part.data.as_ptr());
                    assert_eq!(data.len(), config.parts * config.part_bytes);
                }
                let seed = part_seed(stream_id, segment_id, part_id);
                for (index, word) in part.data.chunks(8).enumerate() {
                    // Counter-derived expectation is independent of the fill
                    // loop's evolving PRNG state; no payload-sized scratch.
                    let counter = WORD_STEP.wrapping_mul(index as u64 + 1);
                    let expected = mix_word(seed.wrapping_add(counter)).to_le_bytes();
                    assert_eq!(word, &expected[..word.len()]);
                    for byte in word.iter().copied() {
                        assert_eq!(body.next(), Some(byte), "exact body byte/order equality");
                        checksum = (checksum ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                        total += 1;
                    }
                }
            }
            assert!(body.next().is_none(), "no trailing body bytes");
        }
    }
    (total, checksum)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut config = Config {
        variant: Variant::Shared,
        streams: 2,
        segments: 2,
        parts: 5,
        part_bytes: 4096,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!(
                "hls-cache-memory [--variant shared|joined] [--streams 2] [--segments 2] [--parts 5] [--part-bytes 4096]"
            );
            return Ok(());
        }
        let value = args.next().ok_or("missing option value")?;
        match arg.as_str() {
            "--variant" => {
                config.variant = match value.as_str() {
                    "shared" => Variant::Shared,
                    "joined" => Variant::Joined,
                    _ => return Err("variant must be shared or joined".into()),
                }
            }
            "--streams" => config.streams = value.parse()?,
            "--segments" => config.segments = value.parse()?,
            "--parts" => config.parts = value.parse()?,
            "--part-bytes" => config.part_bytes = value.parse()?,
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let original_bytes = config.original_bytes()?;
    let cache = build(&config);
    let (verified_bytes, checksum) = verify(&cache, &config);
    assert_eq!(verified_bytes, original_bytes);
    let retained_payload = original_bytes
        * match config.variant {
            Variant::Shared => 1,
            Variant::Joined => 2,
        };
    println!(
        "{{\"model\":\"generated cache storage only\",\"payload_pattern\":\"splitmix64-per-part-v1\",\"variant\":\"{}\",\"architecture\":\"{}\",\"streams\":{},\"segments\":{},\"parts_per_segment\":{},\"part_bytes\":{},\"original_payload_bytes\":{},\"logical_owned_payload_bytes\":{},\"verified_body_bytes\":{},\"body_fnv1a64\":\"{:016x}\"}}",
        config.variant.name(),
        std::env::consts::ARCH,
        config.streams,
        config.segments,
        config.parts,
        config.part_bytes,
        original_bytes,
        retained_payload,
        verified_bytes,
        checksum,
    );
    black_box(&cache);
    Ok(())
}
