//! Finite, generated-input MPEG-TS packaging benchmark.
//!
//! cargo run --release -p sapflow-client --example mpegts-mux-bench -- \
//!   --iterations 10000
//!
//! Inputs model compressed payload sizes only. They are not decodable media or
//! codec-quality evidence. Run alone on the chosen benchmark machine and retain
//! source and binary hashes.

use sapflow_client::mpegts::{MpegTsConfig, MpegTsMuxer, MuxStats};
use sapflow_client::{
    AacConfig, CodecConfig, EncodedSample, H264Config, SampleTiming, SharedSample,
};
use std::{error::Error, hint::black_box, sync::Arc, time::Instant};

#[derive(Clone, Copy)]
enum Kind {
    Audio,
    Video,
    Idr,
}

#[derive(Clone, Copy)]
struct Workload {
    name: &'static str,
    kind: Kind,
    payload_bytes: usize,
}

struct Measurement {
    elapsed_ns: u128,
    output_bytes: u64,
    verification_fnv1a64: u64,
    stats: MuxStats,
}

fn video_config() -> H264Config {
    H264Config::new(
        &[
            0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0,
            3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
        ],
        &[0x68, 0xef, 0xbc, 0x80],
    )
    .expect("SPS/PPS")
}

fn audio_config() -> AacConfig {
    AacConfig::new(&[0x11, 0x90]).expect("48k stereo")
}

fn fixtures(workload: Workload, iterations: u32) -> (MpegTsConfig, Vec<SharedSample>) {
    let (config, codec) = match workload.kind {
        Kind::Audio => (
            MpegTsConfig {
                audio: Some(audio_config()),
                ..MpegTsConfig::default()
            },
            CodecConfig::Aac(audio_config()),
        ),
        Kind::Video | Kind::Idr => (
            MpegTsConfig {
                video: Some(video_config()),
                ..MpegTsConfig::default()
            },
            CodecConfig::H264(video_config()),
        ),
    };
    let codec = Arc::new(codec);
    let video_payload = |keyframe| {
        let nal_len = workload.payload_bytes - 4;
        let mut payload = (nal_len as u32).to_be_bytes().to_vec();
        payload.push(if keyframe { 0x65 } else { 0x41 });
        payload.resize(workload.payload_bytes, 0x80);
        Arc::<[u8]>::from(payload)
    };
    let (payload, first_key_payload) = match workload.kind {
        Kind::Audio => (
            Arc::from(
                (0..workload.payload_bytes)
                    .map(|i| (i as u8).wrapping_mul(31).wrapping_add(7))
                    .collect::<Vec<u8>>(),
            ),
            None,
        ),
        Kind::Video => (video_payload(false), Some(video_payload(true))),
        Kind::Idr => (video_payload(true), None),
    };
    let samples = (0..iterations)
        .map(|sequence| {
            let (keyframe, payload) = match workload.kind {
                Kind::Audio => (false, payload.clone()),
                Kind::Video if sequence == 0 => {
                    (true, first_key_payload.clone().expect("first IDR"))
                }
                Kind::Video => (false, payload.clone()),
                Kind::Idr => (true, payload.clone()),
            };
            let timing = match workload.kind {
                Kind::Audio => {
                    let ticks = i64::from(sequence) * 1024;
                    SampleTiming::new(ticks, ticks, 1024, 48000).expect("audio timing")
                }
                Kind::Video | Kind::Idr => {
                    let ticks = i64::from(sequence) * 3000;
                    SampleTiming::new(ticks, ticks, 3000, 90000).expect("video timing")
                }
            };
            Arc::new(
                EncodedSample::new(codec.clone(), timing, keyframe, payload.clone())
                    .expect("encoded sample"),
            )
        })
        .collect();
    (config, samples)
}

fn checksum_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

fn measure(workload: Workload, iterations: u32) -> Measurement {
    let (config, samples) = fixtures(workload, iterations);
    let mut samples = samples.into_iter();
    let mut mux = MpegTsMuxer::new(config).expect("mux");
    let started = Instant::now();
    let mut output_bytes = 0u64;
    for _ in 0..iterations {
        mux.push(samples.next().expect("fixture")).expect("push");
        while let Some(chunk) = mux.poll_chunk().expect("chunk") {
            output_bytes += black_box(chunk.len()) as u64;
            drop(black_box(chunk));
        }
    }
    let elapsed_ns = started.elapsed().as_nanos();
    let stats = mux.stats();
    assert_eq!(stats.accepted_samples, u64::from(iterations));
    assert_eq!(stats.emitted_bytes, output_bytes);
    assert_eq!(stats.emitted_packets * 188, output_bytes);
    assert!(mux.is_idle());

    let (config, samples) = fixtures(workload, 4);
    let mut verification = MpegTsMuxer::new(config).expect("verification mux");
    let mut verification_fnv1a64 = 0xcbf29ce484222325;
    for sample in samples {
        verification.push(sample).expect("verification push");
        while let Some(chunk) = verification.poll_chunk().expect("verification chunk") {
            verification_fnv1a64 = checksum_update(verification_fnv1a64, &chunk);
        }
    }
    Measurement {
        elapsed_ns,
        output_bytes,
        verification_fnv1a64,
        stats,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut iterations = 10_000u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => iterations = args.next().ok_or("missing iterations")?.parse()?,
            "--help" => {
                println!("mpegts-mux-bench [--iterations 10000]");
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if !(1..=100_000).contains(&iterations) {
        return Err("iterations must be 1..100000".into());
    }
    println!(
        "{{\"kind\":\"method\",\"synthetic_payloads\":true,\"architecture\":\"{}\",\"debug_assertions\":{},\"iterations\":{},\"timing\":\"push, drain and output drop; excludes fixture generation, warm-up and checksum\"}}",
        std::env::consts::ARCH,
        cfg!(debug_assertions),
        iterations,
    );
    for workload in [
        Workload {
            name: "aac-320",
            kind: Kind::Audio,
            payload_bytes: 320,
        },
        Workload {
            name: "h264-8500",
            kind: Kind::Video,
            payload_bytes: 8_500,
        },
        Workload {
            name: "h264-idr-524288",
            kind: Kind::Idr,
            payload_bytes: 512 * 1024,
        },
    ] {
        black_box(measure(workload, 16));
        let measured = measure(workload, iterations);
        println!(
            "{{\"workload\":\"{}\",\"iterations\":{},\"elapsed_ns\":{},\"ns_per_sample\":{:.3},\"output_bytes\":{},\"verification_fnv1a64\":\"{:016x}\",\"staging_copy_bytes\":{},\"packet_copy_bytes\":{},\"writer_copy_bytes\":{},\"output_allocations\":{}}}",
            workload.name,
            iterations,
            measured.elapsed_ns,
            measured.elapsed_ns as f64 / f64::from(iterations),
            measured.output_bytes,
            measured.verification_fnv1a64,
            measured.stats.elementary_staging_copy_bytes,
            measured.stats.packet_payload_copy_bytes,
            measured.stats.writer_payload_copy_bytes,
            measured.stats.output_allocations,
        );
    }
    Ok(())
}
