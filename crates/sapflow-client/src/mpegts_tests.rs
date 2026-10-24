use super::*;
use crate::media::{EncodedSample, SampleTiming};
use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::{ReadTsPacket, TsPacketReader};
use std::sync::Arc;

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
fn config() -> MpegTsConfig {
    MpegTsConfig {
        video: Some(video_config()),
        audio: Some(audio_config()),
        ..MpegTsConfig::default()
    }
}
fn video_at(pts: i64, dts: i64, key: bool, size: usize) -> SharedSample {
    let len = size.max(2);
    let mut payload = (len as u32).to_be_bytes().to_vec();
    payload.push(if key { 0x65 } else { 0x41 });
    payload.resize(len + 4, 0x80);
    Arc::new(
        EncodedSample::new(
            Arc::new(CodecConfig::H264(video_config())),
            SampleTiming::new(pts, dts, 3000, 90000).expect("time"),
            key,
            Arc::from(payload),
        )
        .expect("sample"),
    )
}
fn audio_at(ticks: i64, bytes: &[u8]) -> SharedSample {
    Arc::new(
        EncodedSample::new(
            Arc::new(CodecConfig::Aac(audio_config())),
            SampleTiming::new(ticks, ticks, 1024, 48000).expect("time"),
            false,
            Arc::from(bytes),
        )
        .expect("sample"),
    )
}
fn drain(mux: &mut MpegTsMuxer) -> Vec<u8> {
    let mut bytes = vec![];
    while let Some(chunk) = mux.poll_chunk().expect("chunk") {
        assert!(chunk.len() <= 1316);
        assert_eq!(chunk.len() % 188, 0);
        bytes.extend(chunk);
    }
    bytes
}
fn packets(bytes: &[u8]) -> Vec<TsPacket> {
    let mut reader = TsPacketReader::new(bytes);
    let mut packets = vec![];
    while let Some(packet) = reader.read_ts_packet().expect("TS") {
        packets.push(packet);
    }
    packets
}

#[test]
fn programme_tables_and_aac_header_have_independent_values() {
    assert_eq!(
        adts_header(audio_config(), 3).expect("ADTS"),
        [0xff, 0xf1, 0x4c, 0x80, 0x01, 0x5f, 0xfc]
    );
    assert!(adts_header(audio_config(), 8184).is_ok());
    assert_eq!(adts_header(audio_config(), 8185), Err(MuxError::AdtsLength));
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    mux.push(audio_at(0, &[1, 2, 3])).expect("push");
    let bytes = drain(&mut mux);
    let packets = packets(&bytes);
    let Some(TsPayload::Pat(pat)) = &packets[0].payload else {
        panic!("PAT")
    };
    assert_eq!(pat.table[0].program_num, 1);
    assert_eq!(pat.table[0].program_map_pid.as_u16(), PMT_PID);
    let Some(TsPayload::Pmt(pmt)) = &packets[1].payload else {
        panic!("PMT")
    };
    assert_eq!(pmt.pcr_pid.expect("PCR PID").as_u16(), VIDEO_PID);
    assert_eq!(
        pmt.es_info
            .iter()
            .map(|e| e.stream_type)
            .collect::<Vec<_>>(),
        vec![StreamType::H264, StreamType::AdtsAac]
    );
    let mut reader = PesPacketReader::new(TsPacketReader::new(&bytes[..]));
    let pes = reader.read_pes_packet().expect("PES").expect("AAC");
    assert_eq!(
        pes.data,
        [0xff, 0xf1, 0x4c, 0x80, 0x01, 0x5f, 0xfc, 1, 2, 3]
    );
}

#[test]
fn idr_annexb_parameter_sets_and_timestamps_round_trip() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    let sample = video_at(9000, 6000, true, 500);
    let weak = Arc::downgrade(&sample);
    mux.push(sample).expect("push");
    assert!(weak.upgrade().is_some());
    let bytes = drain(&mut mux);
    assert!(weak.upgrade().is_none(), "mux released its sample handle");
    let mut reader = PesPacketReader::new(TsPacketReader::new(&bytes[..]));
    let pes = reader.read_pes_packet().expect("PES").expect("video");
    assert_eq!(pes.header.pts.expect("PTS").as_u64(), 9000);
    assert_eq!(pes.header.dts.expect("DTS").as_u64(), 6000);
    let mut expected = vec![0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1];
    expected.extend(video_config().sps());
    expected.extend([0, 0, 0, 1]);
    expected.extend(video_config().pps());
    expected.extend([0, 0, 0, 1, 0x65]);
    expected.extend(vec![0x80; 499]);
    assert_eq!(pes.data, expected);
    let stats = mux.stats();
    assert_eq!(stats.elementary_staging_copy_bytes, 184 - 2 - 19);
    assert_eq!(stats.packet_payload_copy_bytes, expected.len() as u64);
    assert_eq!(stats.writer_payload_copy_bytes, expected.len() as u64);
    assert_eq!(stats.buffered_sample_bytes, 0);
}

#[test]
fn multi_nal_sample_advances_beyond_a_partially_consumed_piece() {
    fn nal(header: u8, len: usize, fill: u8) -> Vec<u8> {
        let mut nal = vec![header];
        nal.resize(len, fill);
        nal
    }

    let first = nal(0x65, 400, 0x80);
    let second = nal(0x41, 100, 0x81);
    let mut payload = Vec::new();
    for nal in [&first, &second] {
        payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        payload.extend_from_slice(nal);
    }
    let sample = Arc::new(
        EncodedSample::new(
            Arc::new(CodecConfig::H264(video_config())),
            SampleTiming::new(9000, 6000, 3000, 90000).expect("time"),
            true,
            Arc::from(payload),
        )
        .expect("multi-NAL sample"),
    );
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    mux.push(sample).expect("push");
    let bytes = drain(&mut mux);
    assert!(mux.is_idle());
    let mut reader = PesPacketReader::new(TsPacketReader::new(&bytes[..]));
    let pes = reader.read_pes_packet().expect("PES").expect("video");
    let mut expected = vec![0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1];
    expected.extend(video_config().sps());
    expected.extend([0, 0, 0, 1]);
    expected.extend(video_config().pps());
    expected.extend([0, 0, 0, 1]);
    expected.extend(first);
    expected.extend([0, 0, 0, 1]);
    expected.extend(second);
    assert_eq!(pes.data, expected);
}

#[test]
fn contiguous_audio_payload_skips_staging_after_header_boundary() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    let payload: Vec<u8> = (0..320).map(|i| i as u8).collect();
    mux.push(audio_at(48000, &payload)).expect("push");
    let bytes = drain(&mut mux);
    let mut reader = PesPacketReader::new(TsPacketReader::new(&bytes[..]));
    let pes = reader.read_pes_packet().expect("PES").expect("AAC");
    let mut expected = adts_header(audio_config(), payload.len())
        .expect("ADTS")
        .to_vec();
    expected.extend_from_slice(&payload);
    assert_eq!(pes.data, expected);
    let stats = mux.stats();
    assert_eq!(stats.elementary_staging_copy_bytes, 184 - 2 - 14);
    assert_eq!(stats.packet_payload_copy_bytes, expected.len() as u64);
    assert_eq!(stats.writer_payload_copy_bytes, expected.len() as u64);
    assert!(stats.elementary_staging_copy_bytes < stats.packet_payload_copy_bytes);
}

#[test]
fn audio_only_pcr_has_independent_wire_layout_and_reserved_bits() {
    let cfg = MpegTsConfig {
        audio: Some(audio_config()),
        ..MpegTsConfig::default()
    };
    let mut mux = MpegTsMuxer::new(cfg).expect("mux");
    mux.push(audio_at(48000, &[1, 2, 3])).expect("push");
    let bytes = drain(&mut mux);
    let raw = bytes
        .chunks_exact(188)
        .find(|b| b[3] & 0x30 == 0x20)
        .expect("PCR");
    // PID=257, adaptation-only, CC=0, length=183, discontinuity+PCR.
    assert_eq!(&raw[..6], &[0x47, 0x01, 0x01, 0x20, 0xb7, 0x90]);
    // PCR base=90000: independent integer field vector, reserved bits 111111.
    assert_eq!(&raw[6..12], &[0, 0, 0xaf, 0xc8, 0x7e, 0]);
    assert!(raw[12..].iter().all(|b| *b == 0xff));
    let packets = packets(&bytes);
    let Some(TsPayload::Pmt(pmt)) = &packets[1].payload else {
        panic!("PMT")
    };
    assert_eq!(pmt.es_info.len(), 1);
    assert_eq!(pmt.pcr_pid.expect("PCR").as_u16(), AUDIO_PID);
    assert_eq!(
        packets[2]
            .adaptation_field
            .as_ref()
            .expect("adaptation")
            .pcr
            .expect("PCR")
            .as_u64(),
        27_000_000
    );
}

#[test]
fn bounded_pull_busy_and_explicit_discontinuity() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    let sample = video_at(0, 0, true, 40_000);
    let weak = Arc::downgrade(&sample);
    mux.push(sample).expect("push");
    assert_eq!(mux.push(audio_at(0, &[1])), Err(MuxError::Busy));
    assert_eq!(mux.poll_chunk().expect("chunk").expect("bytes").len(), 1316);
    assert!(!mux.is_idle());
    assert!(mux.stats().buffered_sample_bytes <= MAX_SAMPLE_BYTES);
    mux.reset(config()).expect("reset");
    assert!(weak.upgrade().is_none());
    assert!(mux.is_idle());
    assert!(mux.flush().expect("flush").is_none());
    assert_eq!(mux.stats().discarded_sample_bytes, 40004);
    assert_eq!(
        mux.push(video_at(3000, 3000, false, 2)),
        Err(MuxError::NeedsKeyframe)
    );
    mux.push(video_at(0, 0, true, 2)).expect("new epoch");
    let packets = packets(&drain(&mut mux));
    let Some(TsPayload::Pat(pat)) = &packets[0].payload else {
        panic!("PAT")
    };
    assert_eq!(pat.version_number.as_u8(), 1);
}

#[test]
fn pcr_cadence_tables_and_counters_are_bounded_across_rollovers() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    let mut bytes = vec![];
    for i in 0..40 {
        mux.push(video_at(i * 3000, i * 3000, i == 0, 200))
            .expect("push");
        bytes.extend(drain(&mut mux));
    }
    let parsed = packets(&bytes);
    let pcr: Vec<_> = parsed
        .iter()
        .filter_map(|p| p.adaptation_field.as_ref()?.pcr.map(|p| p.as_u64()))
        .collect();
    assert!(pcr.windows(2).all(|w| w[1] - w[0] == 1_080_000));
    assert!(
        parsed
            .iter()
            .filter(|p| matches!(p.payload, Some(TsPayload::Pat(_))))
            .count()
            >= 13
    );
    let mut last = std::collections::HashMap::new();
    for packet in parsed {
        let pid = packet.header.pid.as_u16();
        let cc = packet.header.continuity_counter.as_u8();
        if let Some(previous) = last.get(&pid) {
            assert_eq!(
                cc,
                if packet.payload.is_some() {
                    (previous + 1) & 15
                } else {
                    *previous
                }
            );
        }
        if packet.payload.is_some() {
            last.insert(pid, cc);
        }
    }
}

#[test]
fn intertrack_skew_does_not_regress_video_pcr_and_track_changes_are_refused() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    mux.push(audio_at(4096, &[1])).expect("audio ahead");
    drain(&mut mux);
    mux.push(video_at(0, 0, true, 2))
        .expect("video may lag audio");
    drain(&mut mux);
    assert_eq!(
        mux.push(audio_at(1024, &[1])),
        Err(MuxError::TimestampRegression)
    );
    let changed = AacConfig::new(&[0x12, 0x10]).expect("44.1k stereo");
    let sample = Arc::new(
        EncodedSample::new(
            Arc::new(CodecConfig::Aac(changed)),
            SampleTiming::new(44100, 44100, 1024, 44100).expect("time"),
            false,
            Arc::from([1]),
        )
        .expect("sample"),
    );
    assert_eq!(mux.push(sample), Err(MuxError::ConfigurationChanged));
    assert_eq!(
        mux.push(video_at(270000, 270000, false, 2)),
        Err(MuxError::ClockGap)
    );
}

#[test]
fn a_keyframe_after_a_dropped_backlog_starts_a_discontinuity() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    mux.push(video_at(0, 0, true, 2)).expect("first");
    drain(&mut mux);
    mux.push(audio_at(1024, &[1])).expect("audio");
    drain(&mut mux);
    // About 2.5 s of media was dropped upstream; the next frame is an IDR.
    mux.push(video_at(225_000, 225_000, true, 2))
        .expect("a keyframe after the gap carries on");
    assert_eq!(mux.stats().resets, 1);
    let packets = packets(&drain(&mut mux));
    let Some(TsPayload::Pat(pat)) = &packets[0].payload else {
        panic!("PAT")
    };
    assert_eq!(pat.version_number.as_u8(), 1);
    let video = packets
        .iter()
        .find(|p| matches!(p.payload, Some(TsPayload::PesStart(_))))
        .expect("video PES");
    assert!(
        video
            .adaptation_field
            .as_ref()
            .is_some_and(|a| a.discontinuity_indicator)
    );
    // Audio continues on the new clock without a regression error.
    mux.push(audio_at(225_000 * 48 / 90, &[1]))
        .expect("audio after");
    drain(&mut mux);
    assert_eq!(
        mux.push(video_at(500_000, 500_000, false, 2)),
        Err(MuxError::ClockGap)
    );
}

#[test]
fn pts_dts_and_pcr_wrap_at_33_bits_without_resetting_track_clock() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    let dts = (1i64 << 33) - 1500;
    mux.push(video_at(dts + 3000, dts, true, 2)).expect("push");
    let bytes = drain(&mut mux);
    let mut reader = PesPacketReader::new(TsPacketReader::new(&bytes[..]));
    let pes = reader.read_pes_packet().expect("PES").expect("video");
    assert_eq!(pes.header.pts.expect("PTS").as_u64(), 1500);
    assert_eq!(pes.header.dts.expect("DTS").as_u64(), (1 << 33) - 1500);
    mux.push(video_at(dts + 6000, dts + 3000, false, 2))
        .expect("after wrap");
    drain(&mut mux);
}

#[test]
fn pcr_reserved_shim_rejects_payload_and_short_packets() {
    assert_eq!(fix_pcr_reserved_bits(&mut [0; 16]), Err(MuxError::Writer));
    let mut payload = [0xff; 188];
    payload[..6].copy_from_slice(&[0x47, 1, 0, 0x30, 183, 0x10]);
    assert_eq!(fix_pcr_reserved_bits(&mut payload), Err(MuxError::Writer));
}

/// External decoders are executable black boxes; generated fixtures have no
/// source/credential dependency. Run explicitly with --ignored --nocapture.
#[test]
#[ignore = "requires ffmpeg and ffprobe executables"]
fn ffmpeg_generated_av_and_audio_only_decode() {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn run(args: &[&str]) -> Vec<u8> {
        let result = Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(args)
            .output()
            .expect("ffmpeg executable");
        assert!(
            result.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.stderr.is_empty(),
            "decoder warnings/errors: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }
    fn path(path: &Path) -> &str {
        path.to_str().expect("fixture path")
    }
    fn annexb(bytes: &[u8]) -> Vec<&[u8]> {
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 <= bytes.len() {
            let width = if bytes.get(i..i + 4) == Some(&[0, 0, 0, 1][..]) {
                4
            } else if bytes.get(i..i + 3) == Some(&[0, 0, 1][..]) {
                3
            } else {
                0
            };
            if width > 0 {
                starts.push((i, i + width));
                i += width;
            } else {
                i += 1;
            }
        }
        starts
            .iter()
            .enumerate()
            .map(|(i, (_, start))| {
                &bytes[*start..starts.get(i + 1).map_or(bytes.len(), |(start, _)| *start)]
            })
            .collect()
    }
    fn frame_hashes(bytes: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.rsplit(',').next().map(|hash| hash.trim().to_owned()))
            .collect()
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let scratch = Scratch(root.join(format!(
            "sapflow-mpegts-interop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
    std::fs::create_dir(&scratch.0).expect("task scratch");
    let h264 = scratch.0.join("fixture.h264");
    let aac = scratch.0.join("fixture.aac");
    run(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=160x90:rate=30",
        "-t",
        "1",
        "-an",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-x264-params",
        "aud=1:repeat-headers=1:bframes=0:keyint=15",
        "-f",
        "h264",
        path(&h264),
    ]);
    run(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=997:sample_rate=48000",
        "-t",
        "1",
        "-ac",
        "2",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-f",
        "adts",
        path(&aac),
    ]);
    let h264_bytes = std::fs::read(&h264).expect("video fixture");
    let nals = annexb(&h264_bytes);
    let sps = nals.iter().find(|nal| nal[0] & 31 == 7).expect("SPS");
    let pps = nals.iter().find(|nal| nal[0] & 31 == 8).expect("PPS");
    let video = H264Config::new(sps, pps).expect("generated H264 configuration");
    let video_arc = Arc::new(CodecConfig::H264(video.clone()));
    let mut frames = Vec::new();
    let mut current = Vec::new();
    let mut key = false;
    for nal in nals {
        let kind = nal[0] & 31;
        if kind == 9 && !current.is_empty() {
            frames.push((std::mem::take(&mut current), key));
            key = false;
        }
        if matches!(kind, 1 | 5 | 6 | 12) {
            key |= kind == 5;
            current.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            current.extend_from_slice(nal);
        }
    }
    if !current.is_empty() {
        frames.push((current, key));
    }
    assert_eq!(frames.len(), 30);
    let mut samples = Vec::new();
    for (i, (bytes, key)) in frames.into_iter().enumerate() {
        samples.push(Arc::new(
            EncodedSample::new(
                video_arc.clone(),
                SampleTiming::new(i as i64, i as i64, 1, 30).expect("time"),
                key,
                Arc::from(bytes),
            )
            .expect("video sample"),
        ));
    }
    let aac_bytes = std::fs::read(&aac).expect("AAC fixture");
    let mut rest = &aac_bytes[..];
    let mut audio_samples = Vec::new();
    while !rest.is_empty() {
        assert!(rest.len() >= 7);
        assert_eq!(rest[0], 0xff);
        assert_eq!(rest[1] & 1, 1);
        let len = (usize::from(rest[3] & 3) << 11)
            | (usize::from(rest[4]) << 3)
            | usize::from(rest[5] >> 5);
        assert!((7..=8191).contains(&len));
        assert!(len <= rest.len());
        assert_eq!((rest[2] >> 2) & 15, 3, "48k frequency index");
        audio_samples.push(audio_at(audio_samples.len() as i64 * 1024, &rest[7..len]));
        rest = &rest[len..];
    }
    assert!(audio_samples.len() >= 47);
    let expected_video = frame_hashes(&run(&[
        "-i",
        path(&h264),
        "-map",
        "0:v:0",
        "-f",
        "framemd5",
        "-",
    ]));
    let expected_audio = run(&["-i", path(&aac), "-map", "0:a:0", "-f", "s16le", "-"]);
    for audio_only in [false, true] {
        let cfg = MpegTsConfig {
            video: (!audio_only).then_some(video.clone()),
            audio: Some(audio_config()),
            ..MpegTsConfig::default()
        };
        let mut mux = MpegTsMuxer::new(cfg).expect("mux");
        let output = scratch.0.join(if audio_only {
            "audio-only.ts"
        } else {
            "audio-video.ts"
        });
        let mut file = std::fs::File::create(&output).expect("output");
        let mut all = audio_samples.clone();
        if !audio_only {
            all.extend(samples.iter().cloned());
        }
        all.sort_by_key(|sample| sample.timing().decode_time_ns());
        for sample in all {
            mux.push(sample).expect("mux sample");
            while let Some(chunk) = mux.poll_chunk().expect("chunk") {
                file.write_all(&chunk).expect("output");
            }
        }
        drop(file);
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,width,height,sample_rate,channels",
                "-of",
                "compact",
                path(&output),
            ])
            .output()
            .expect("ffprobe");
        assert!(
            probe.status.success(),
            "{}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let probe = String::from_utf8(probe.stdout).expect("probe text");
        assert!(probe.contains("codec_name=aac"));
        if !audio_only {
            assert!(probe.contains("codec_name=h264"));
            let actual = frame_hashes(&run(&[
                "-i",
                path(&output),
                "-map",
                "0:v:0",
                "-f",
                "framemd5",
                "-",
            ]));
            assert_eq!(actual, expected_video, "decoded video frame hashes");
        }
        let actual_audio = run(&["-i", path(&output), "-map", "0:a:0", "-f", "s16le", "-"]);
        assert_eq!(actual_audio, expected_audio, "decoded PCM bytes");
        let ts = std::fs::read(&output).expect("TS");
        assert!(
            packets(&ts)
                .iter()
                .any(|p| p.adaptation_field.as_ref().is_some_and(|a| a.pcr.is_some()))
        );
        println!(
            "MPEGTS independent decode: audio_only={audio_only} ts_bytes={} video_frames={} pcm_bytes={} pcr_present=true mux_stats={:?}",
            ts.len(),
            if audio_only { 0 } else { expected_video.len() },
            actual_audio.len(),
            mux.stats()
        );
    }
}

#[test]
fn sample_byte_time_nal_and_adts_limits_refuse_atomically() {
    let mut cfg = config();
    cfg.max_sample_bytes = 8;
    let mut mux = MpegTsMuxer::new(cfg).expect("mux");
    assert_eq!(
        mux.push(video_at(0, 0, true, 10)),
        Err(MuxError::SampleTooLarge)
    );
    assert!(mux.is_idle());
    assert_eq!(mux.stats().accepted_samples, 0);
    mux.push(video_at(0, 0, true, 2))
        .expect("valid retry after rejection");
    drain(&mut mux);

    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    let long = Arc::new(
        EncodedSample::new(
            Arc::new(CodecConfig::H264(video_config())),
            SampleTiming::new(0, 0, 1, 1).expect("long time"),
            true,
            Arc::from([0, 0, 0, 2, 0x65, 0x80]),
        )
        .expect("sample"),
    );
    assert_eq!(mux.push(long), Err(MuxError::SampleTooLong));
    assert_eq!(
        mux.push(audio_at(0, &vec![1; 8185])),
        Err(MuxError::AdtsLength)
    );
    let mut many = vec![0, 0, 0, 2, 0x65, 0x80];
    for _ in 0..MAX_NALS {
        many.extend_from_slice(&[0, 0, 0, 2, 6, 0x80]);
    }
    let many = Arc::new(
        EncodedSample::new(
            Arc::new(CodecConfig::H264(video_config())),
            SampleTiming::new(0, 0, 1, 30).expect("time"),
            true,
            Arc::from(many),
        )
        .expect("framed sample"),
    );
    assert_eq!(mux.push(many), Err(MuxError::TooManyNals));
    assert!(mux.is_idle());
    assert_eq!(mux.stats().accepted_samples, 0);
    mux.push(video_at(0, 0, true, 2))
        .expect("rejections do not consume first keyframe");
    drain(&mut mux);
}

#[test]
fn input_aud_is_replaced_with_exactly_one_before_sps_pps() {
    let mut mux = MpegTsMuxer::new(config()).expect("mux");
    let sample = Arc::new(
        EncodedSample::new(
            Arc::new(CodecConfig::H264(video_config())),
            SampleTiming::new(0, 0, 1, 30).expect("time"),
            true,
            Arc::from([0, 0, 0, 2, 9, 0xf0, 0, 0, 0, 2, 0x65, 0x80]),
        )
        .expect("sample"),
    );
    mux.push(sample).expect("push");
    let bytes = drain(&mut mux);
    let mut reader = PesPacketReader::new(TsPacketReader::new(&bytes[..]));
    let pes = reader.read_pes_packet().expect("PES").expect("video");
    assert_eq!(
        pes.data
            .windows(5)
            .filter(|w| *w == [0, 0, 0, 1, 9])
            .count(),
        1
    );
    assert_eq!(&pes.data[..6], &[0, 0, 0, 1, 9, 0xf0]);
}
