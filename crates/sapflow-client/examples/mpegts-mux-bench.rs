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
    fixtures_with_gap(workload, iterations, None)
}

fn fixtures_with_gap(
    workload: Workload,
    iterations: u32,
    gap_every: Option<u32>,
) -> (MpegTsConfig, Vec<SharedSample>) {
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
                    let gaps = gap_every.map_or(0, |every| sequence / every);
                    let ticks = i64::from(sequence) * 3000 + i64::from(gaps) * 270_000;
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

fn stream_header(output: &mut impl std::io::Write, name: &str, iterations: u32) {
    output.write_all(b"WORK").expect("workload tag");
    output
        .write_all(
            &u32::try_from(name.len())
                .expect("bounded name")
                .to_be_bytes(),
        )
        .expect("name length");
    output.write_all(name.as_bytes()).expect("name");
    output
        .write_all(&iterations.to_be_bytes())
        .expect("sample count");
}

fn stream_record(output: &mut impl std::io::Write, sequence: u32, chunk: u32, bytes: &[u8]) {
    output.write_all(&sequence.to_be_bytes()).expect("sequence");
    output.write_all(&chunk.to_be_bytes()).expect("chunk index");
    output
        .write_all(
            &u32::try_from(bytes.len())
                .expect("bounded chunk")
                .to_be_bytes(),
        )
        .expect("chunk length");
    output.write_all(bytes).expect("complete output chunk");
}

fn stream_footer(output: &mut impl std::io::Write, bytes: u64, chunks: u64) {
    output.write_all(b"DONE").expect("workload end");
    output.write_all(&bytes.to_be_bytes()).expect("byte count");
    output
        .write_all(&chunks.to_be_bytes())
        .expect("chunk count");
}

fn verify_stream(
    workload: Workload,
    iterations: u32,
    gap_every: Option<u32>,
    output: &mut impl std::io::Write,
) {
    let (config, samples) = fixtures_with_gap(workload, iterations, gap_every);
    let mut mux = MpegTsMuxer::new(config).expect("verification mux");
    stream_header(output, workload.name, iterations);
    let mut output_bytes = 0u64;
    let mut chunks = 0u64;
    for (sequence, sample) in samples.into_iter().enumerate() {
        let sequence = u32::try_from(sequence).expect("bounded sample index");
        mux.push(sample).expect("verification push");
        let mut chunk_index = 0u32;
        while let Some(chunk) = mux.poll_chunk().expect("verification drain") {
            assert!(!chunk.is_empty());
            assert!(chunk.len() <= 1316 && chunk.len() % 188 == 0);
            stream_record(output, sequence, chunk_index, &chunk);
            output_bytes += chunk.len() as u64;
            chunks += 1;
            chunk_index += 1;
        }
        assert!(chunk_index > 0 && mux.is_idle());
        stream_record(output, sequence, chunk_index, &[]);
    }
    let stats = mux.stats();
    assert_eq!(stats.accepted_samples, u64::from(iterations));
    assert_eq!(stats.emitted_bytes, output_bytes);
    assert_eq!(stats.emitted_packets * 188, output_bytes);
    assert_eq!(
        stats.resets,
        gap_every.map_or(0, |every| u64::from((iterations - 1) / every))
    );
    assert_eq!(stats.discarded_sample_bytes, 0);
    stream_footer(output, output_bytes, chunks);
    println!(
        "{{\"kind\":\"verification_workload\",\"component\":\"mpegts\",\"workload\":\"{}\",\"samples\":{},\"output_bytes\":{},\"chunks\":{},\"resets\":{}}}",
        workload.name, iterations, output_bytes, chunks, stats.resets,
    );
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
    let workloads = [
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
    ];

    if let Some(mode) = std::env::var_os("SAPFLOW_BENCH_VERIFY_STREAM") {
        if mode != "1" || iterations != 10_000 {
            return Err("verification requires mode=1 and exactly 10000 samples".into());
        }
        use std::io::Write;
        let stderr = std::io::stderr();
        let mut output = std::io::BufWriter::with_capacity(65536, stderr.lock());
        output.write_all(b"SFPERF1T")?;
        for workload in workloads {
            verify_stream(workload, iterations, None, &mut output);
        }
        verify_stream(
            Workload {
                name: "h264-gap-recovery-8500",
                kind: Kind::Idr,
                payload_bytes: 8500,
            },
            iterations,
            Some(256),
            &mut output,
        );
        output.write_all(b"END!")?;
        output.flush()?;
        println!(
            "{{\"kind\":\"verification_complete\",\"component\":\"mpegts\",\"workloads\":4,\"samples_per_workload\":10000,\"architecture\":\"{}\",\"debug_assertions\":{}}}",
            std::env::consts::ARCH,
            cfg!(debug_assertions)
        );
        return Ok(());
    }

    println!(
        "{{\"kind\":\"method\",\"synthetic_payloads\":true,\"architecture\":\"{}\",\"debug_assertions\":{},\"iterations\":{},\"timing\":\"sum of fresh-instance inner batches; push, drain and output drop; excludes fixture generation, warm-up and checksum\"}}",
        std::env::consts::ARCH,
        cfg!(debug_assertions),
        iterations,
    );
    for workload in workloads {
        black_box(measure(workload, 16));
        let batches: u32 = match workload.kind {
            Kind::Audio => 128,
            Kind::Video => 16,
            Kind::Idr => 1,
        };
        let mut batch_elapsed_ns = Vec::with_capacity(batches as usize);
        let mut summed_elapsed_ns = 0u128;
        let mut total_output_bytes = 0u64;
        let mut per_batch = None;
        for _ in 0..batches {
            let measured = measure(workload, iterations);
            let signature = (
                measured.output_bytes,
                measured.verification_fnv1a64,
                measured.stats.elementary_staging_copy_bytes,
                measured.stats.packet_payload_copy_bytes,
                measured.stats.writer_payload_copy_bytes,
                measured.stats.output_allocations,
            );
            if let Some(expected) = per_batch {
                assert_eq!(signature, expected, "repeat fresh deterministic batch");
            }
            per_batch = Some(signature);
            batch_elapsed_ns.push(measured.elapsed_ns);
            summed_elapsed_ns += measured.elapsed_ns;
            total_output_bytes += measured.output_bytes;
        }
        let (output_bytes_per_batch, witness, staging, packet, writer, allocations) =
            per_batch.expect("positive batches");
        let operations = u64::from(iterations) * u64::from(batches);
        println!(
            "{{\"workload\":\"{}\",\"iterations_per_batch\":{},\"batches\":{},\"operations\":{},\"elapsed_ns\":{},\"ns_per_sample\":{:.3},\"output_bytes\":{},\"output_bytes_per_batch\":{},\"legacy_four_sample_fnv1a64\":\"{:016x}\",\"staging_copy_bytes\":{},\"packet_copy_bytes\":{},\"writer_copy_bytes\":{},\"output_allocations\":{},\"batch_elapsed_ns\":{:?}}}",
            workload.name,
            iterations,
            batches,
            operations,
            summed_elapsed_ns,
            summed_elapsed_ns as f64 / operations as f64,
            total_output_bytes,
            output_bytes_per_batch,
            witness,
            staging * u64::from(batches),
            packet * u64::from(batches),
            writer * u64::from(batches),
            allocations * u64::from(batches),
            batch_elapsed_ns,
        );
    }
    Ok(())
}
