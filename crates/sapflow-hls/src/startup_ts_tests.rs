//! Generated startup quality and resource-policy regressions; no sockets.
use super::*;
use mp4_atom::Decode;
use std::time::Duration;

const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/testsrc-3s.ts");
const NOW: SystemTime = SystemTime::UNIX_EPOCH;

fn source_frames(fixture: &[u8]) -> Vec<(i64, i64, Vec<u8>)> {
    let mut frames = Vec::new();
    tests_support::elementary(fixture, |event| {
        if let tests_support::Es::Video { dts, pts, annexb } = event {
            frames.push((dts, pts, annexb));
        }
    });
    assert!(frames.len() > 3);
    assert!(codec::access_unit(&frames[0].2).idr);
    frames
}

fn frames() -> Vec<(i64, i64, Vec<u8>)> {
    source_frames(FIXTURE)
}

fn aac_config() -> AacConfig {
    let mut config = None;
    tests_support::elementary(FIXTURE, |event| {
        if let tests_support::Es::Audio { config: aac, .. } = event {
            config.get_or_insert(aac);
        }
    });
    config.expect("fixture AAC")
}

fn declared() -> TsIngest {
    let mut ts = TsIngest::new(MuxerConfig::default());
    ts.es
        .declare_tracks(TrackSet::AudioVideo, NOW)
        .expect("A/V declaration");
    ts
}

fn frame(ts: &mut TsIngest, n: usize, now: SystemTime) -> Result<(), IngestError> {
    let frames = frames();
    let (dts, pts, annexb) = &frames[n];
    ts.on_video(*pts as u64, Some(*dts as u64), annexb, now)
}

fn retained(ts: &TsIngest) -> usize {
    ts.startup_bytes
        + ts.es.sps.as_ref().map_or(0, Vec::capacity)
        + ts.es.pps.as_ref().map_or(0, Vec::capacity)
}

fn released(ts: &TsIngest) {
    assert!(ts.startup.is_empty());
    assert_eq!(ts.startup_bytes, 0);
    assert!(ts.es.sps.is_none() && ts.es.pps.is_none());
    assert!(ts.pes.is_none(), "refusal releases pending PES too");
    assert!(ts.input.lock().expect("input").buf.is_empty());
    assert!(ts.muxer().is_none(), "no provisional video-only init");
}

fn discarded_startup(ts: &TsIngest) {
    assert!(ts.startup.is_empty());
    assert_eq!(ts.startup_bytes, 0);
    assert!(retained(ts) <= ts.startup_limits.bytes);
    assert!(ts.startup_error.is_none() && ts.waiting_keyframe);
    assert!(ts.pes.is_some(), "transport/PES parser remains recoverable");
    assert!(!ts.input.lock().expect("input").limit_exceeded);
    assert!(ts.pending.media_loss);
    assert!(ts.muxer().is_none(), "no provisional init");
}

fn video_samples(ts: &TsIngest) -> Vec<Vec<u8>> {
    let muxer = ts.muxer().expect("A/V muxer");
    let mut output = Vec::new();
    for id in 0..muxer.next_part_id() {
        let crate::PartLookup::Found(part) = muxer.part(id) else {
            continue;
        };
        let moof = mp4_atom::Moof::decode(&mut part.data.as_ref()).expect("actual fragment");
        for track in moof.traf.iter().filter(|track| track.tfhd.track_id == 1) {
            for run in &track.trun {
                let mut offset = run.data_offset.expect("offset") as usize;
                for entry in &run.entries {
                    let end = offset + entry.size.expect("size") as usize;
                    output.push(part.data[offset..end].to_vec());
                    offset = end;
                }
            }
        }
    }
    output
}

#[test]
fn generated_av_preserves_the_first_idr_and_entire_known_duration_prefix() {
    let expected: Vec<_> = frames()
        .iter()
        .map(|(_, _, data)| codec::access_unit(data).data)
        .collect();
    for chunk in [188, 1000, 1316] {
        let mut ts = TsIngest::new(MuxerConfig::default());
        let mut held = false;
        for bytes in FIXTURE.chunks(chunk) {
            ts.push(bytes, NOW).expect("complete generated TS");
            held |= !ts.startup.is_empty();
            assert!(ts.startup.len() <= 64);
            if !ts.startup.is_empty() {
                assert!(retained(&ts) <= 1024 * 1024);
                if ts.waiting_for_startup_audio() {
                    assert!(
                        ts.muxer().is_none(),
                        "AAC is required before A/V initialization"
                    );
                } else {
                    let muxer = ts
                        .muxer()
                        .expect("AAC-known catch-up has authoritative config");
                    assert_eq!(muxer.tracks(), TrackSet::AudioVideo);
                    assert!(muxer.audio().is_some());
                }
            }
        }
        if chunk == 188 {
            assert!(held, "fixture must exercise IDR-before-AAC ordering");
        }
        assert!(!ts.finish(NOW).expect("normal terminal policy").media_loss);
        // Keep the existing two-tail policy: the unbounded final video PES
        // and the muxer's unknown-duration pending AU are still excluded.
        assert_eq!(
            video_samples(&ts).as_slice(),
            &expected[..expected.len() - 2],
            "chunk={chunk}"
        );
        assert_eq!(ts.dropped_packets(), 0);
        assert_eq!(ts.muxer().expect("muxer").tracks(), TrackSet::AudioVideo);
        assert!(ts.startup.is_empty());
        assert_eq!(ts.startup_bytes, 0);
    }
}

#[test]
fn startup_count_and_owned_capacity_bounds_discard_and_release() {
    let mut count = declared();
    count.startup_limits.aus = 2;
    frame(&mut count, 0, NOW).expect("first IDR");
    frame(&mut count, 1, NOW).expect("second AU at count bound");
    assert_eq!(count.startup.len(), 2);
    frame(&mut count, 2, NOW).expect("optional prefix count overflow is recoverable");
    discarded_startup(&count);
    count.push(&[], NOW).expect("transport remains open");

    let mut bytes = declared();
    frame(&mut bytes, 0, NOW).expect("first IDR");
    let second = codec::access_unit(&frames()[1].2).data.capacity();
    bytes.startup_limits.bytes = retained(&bytes) + second;
    frame(&mut bytes, 1, NOW).expect("exact byte bound");
    assert_eq!(retained(&bytes), bytes.startup_limits.bytes);
    frame(&mut bytes, 2, NOW).expect("optional prefix byte overflow is recoverable");
    discarded_startup(&bytes);

    let mut parameters = declared();
    parameters.startup_limits.bytes = 8;
    let metadata = codec::AccessUnit {
        sps: Some(Vec::with_capacity(9)),
        ..Default::default()
    };
    parameters
        .startup_video(0, 0, metadata, NOW)
        .expect("oversized optional parameters are discarded");
    discarded_startup(&parameters);
    assert!(parameters.es.sps.is_none() && parameters.es.pps.is_none());
}

#[test]
fn startup_media_span_has_an_exact_bound() {
    let source = frames();
    let mut span = declared();
    span.startup_limits.ticks = source[1].0 - source[0].0;
    frame(&mut span, 0, NOW).expect("IDR");
    frame(&mut span, 1, NOW).expect("exact media span");
    frame(&mut span, 2, NOW).expect("optional prefix span overflow is recoverable");
    discarded_startup(&span);
}

#[test]
fn civil_clock_adjustments_do_not_refuse_valid_queued_startup() {
    let first_wall = NOW + Duration::from_secs(3600);
    let mut ts = declared();
    frame(&mut ts, 0, first_wall).expect("held first IDR");
    ts.push(&[], NOW)
        .expect("backwards civil clock is not admission policy");
    ts.push(&[], NOW + Duration::from_secs(7200))
        .expect("forwards civil clock is not admission policy");
    assert_eq!(ts.startup.front().expect("held IDR").now, first_wall);
    frame(&mut ts, 1, NOW).expect("next AU with earlier wall association");
    let mut config = None;
    tests_support::elementary(FIXTURE, |event| {
        if let tests_support::Es::Audio { config: aac, .. } = event {
            config.get_or_insert(aac);
        }
    });
    ts.startup_audio(config.expect("fixture AAC"))
        .expect("valid AAC prepares the first IDR");
    ts.release_startup_video()
        .expect("replay after AAC admission");
    assert_eq!(ts.es.origin, Some(frames()[0].0));
    assert!(ts.startup.is_empty());
    assert_eq!(ts.startup_bytes, 0);
    assert!(ts.startup_error.is_none());
    assert_eq!(ts.muxer().expect("A/V init").tracks(), TrackSet::AudioVideo);
}

#[test]
fn preinit_video_configuration_change_replaces_the_held_gop() {
    // Both clips are existing generated test patterns. A new encoder config
    // before the first A/V init must not disconnect an otherwise valid input.
    let replacement = source_frames(include_bytes!("../tests/fixtures/transport-loss-6s.ts"));
    let first = codec::access_unit(&replacement[0].2);
    let video = H264Config::new(
        first.sps.as_ref().expect("new SPS"),
        first.pps.as_ref().expect("new PPS"),
    )
    .expect("supported replacement config");
    let expected_first = first.data.clone();
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("old IDR");
    frame(&mut ts, 1, NOW).expect("old following AU");
    assert_ne!(ts.es.sps.as_ref(), first.sps.as_ref());
    ts.startup_video(replacement[0].0, replacement[0].1, first, NOW)
        .expect("new IDR starts a bounded replacement GOP");
    assert_eq!(ts.startup.len(), 1);
    assert_eq!(ts.startup.front().expect("new IDR").au.data, expected_first);
    assert_eq!(ts.startup.front().expect("new IDR").dts, replacement[0].0);
    assert!(!ts.waiting_keyframe, "following AUs must still be retained");
    ts.on_video(
        replacement[1].1 as u64,
        Some(replacement[1].0 as u64),
        &replacement[1].2,
        NOW,
    )
    .expect("new GOP continuation");
    assert_eq!(ts.startup.len(), 2);
    assert!(retained(&ts) <= ts.startup_limits.bytes);
    assert!(ts.pending.media_loss, "discarded held GOP is visible loss");
    assert!(ts.startup_error.is_none() && ts.muxer().is_none());
    ts.startup_audio(aac_config())
        .expect("authoritative A/V init");
    ts.release_startup_video()
        .expect("replay after AAC admission");
    assert_eq!(ts.es.origin, Some(replacement[0].0));
    assert_eq!(ts.muxer().expect("new init").video(), Some(&video));
    assert_eq!(ts.muxer().expect("A/V init").tracks(), TrackSet::AudioVideo);
    assert!(ts.startup.is_empty());
    assert_eq!(ts.startup_bytes, 0);
}

#[test]
fn non_idr_configuration_change_waits_for_a_fresh_idr_and_keeps_limits() {
    let replacement = source_frames(include_bytes!("../tests/fixtures/transport-loss-6s.ts"));
    let parameters = codec::access_unit(&replacement[0].2);
    let mut non_idr = codec::access_unit(&replacement[1].2);
    assert!(!non_idr.idr);
    non_idr.sps = parameters.sps;
    non_idr.pps = parameters.pps;
    let mut ts = declared();
    ts.startup_limits.aus = 1;
    frame(&mut ts, 0, NOW).expect("old held IDR");
    ts.startup_video(replacement[1].0, replacement[1].1, non_idr, NOW)
        .expect("new parameters without an IDR do not disconnect");
    assert!(ts.startup.is_empty() && ts.waiting_keyframe);
    assert_eq!(ts.startup_bytes, 0);
    assert!(ts.muxer().is_none() && ts.pending.media_loss);
    assert!(retained(&ts) <= ts.startup_limits.bytes);
    ts.on_video(
        replacement[2].1 as u64,
        Some(replacement[2].0 as u64),
        &replacement[2].2,
        NOW,
    )
    .expect("intervening non-IDR is ignored");
    assert!(ts.startup.is_empty());
    ts.startup_video(
        replacement[0].0,
        replacement[0].1,
        codec::access_unit(&replacement[0].2),
        NOW,
    )
    .expect("fresh matching IDR can restart");
    assert_eq!(ts.startup.len(), 1);
    ts.startup_video(
        replacement[1].0,
        replacement[1].1,
        codec::access_unit(&replacement[1].2),
        NOW,
    )
    .expect("count bound still drops the optional GOP after a config restart");
    discarded_startup(&ts);
    ts.push(&[], NOW)
        .expect("transport still accepts valid input");
}

#[test]
fn unsupported_startup_aac_still_refuses_permanently() {
    let mut audio = declared();
    frame(&mut audio, 0, NOW).expect("IDR");
    let mut config = aac_config();
    config.object_type = 1;
    assert_eq!(audio.startup_audio(config), Err(IngestError::InvalidAudio));
    released(&audio);
    assert_eq!(audio.push(&[], NOW), Err(IngestError::InvalidAudio));
}

fn startup_video_packet(pes_start: bool) -> usize {
    let mut reader = TsPacketReader::new(FIXTURE);
    let video = loop {
        let packet = reader.read_ts_packet().expect("fixture TS").expect("PMT");
        if let Some(TsPayload::Pmt(pmt)) = packet.payload
            && let Some(video) = pmt
                .es_info
                .iter()
                .find(|es| es.stream_type == StreamType::H264)
        {
            break video.elementary_pid.as_u16();
        }
    };
    let mut probe = TsIngest::new(MuxerConfig::default());
    for (index, packet) in FIXTURE.chunks_exact(TS_SIZE).enumerate() {
        let pid = u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2]);
        if !probe.startup.is_empty()
            && pid == video
            && packet[3] & 0x10 != 0
            && (packet[1] & 0x40 != 0) == pes_start
        {
            assert!(probe.es.known_audio().is_none());
            return index;
        }
        probe.push(packet, NOW).expect("intact startup probe");
    }
    panic!("fixture must exercise a held GOP before AAC");
}

fn recovery_matches_the_generated_suffix(damaged: &[u8]) {
    let source = frames();
    let recovery = source
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, frame)| codec::access_unit(&frame.2).idr)
        .map(|(index, _)| index)
        .expect("fixture has a later intact IDR");
    let expected: Vec<_> = source
        .iter()
        .map(|(_, _, annexb)| codec::access_unit(annexb).data)
        .collect();
    for chunk in [188, 1000, 1316] {
        let mut ts = TsIngest::new(MuxerConfig::default());
        let mut visible_loss = false;
        for bytes in damaged.chunks(chunk) {
            visible_loss |= ts
                .push(bytes, NOW)
                .expect("recover without reconnect")
                .media_loss;
            assert!(ts.startup.len() <= ts.startup_limits.aus);
            if !ts.startup.is_empty() {
                assert!(retained(&ts) <= ts.startup_limits.bytes);
                if ts.waiting_for_startup_audio() {
                    assert!(ts.muxer().is_none(), "no provisional A/V init");
                } else {
                    let muxer = ts.muxer().expect("AAC-known catch-up");
                    assert_eq!(muxer.tracks(), TrackSet::AudioVideo);
                    assert!(muxer.audio().is_some());
                }
            }
            assert!(ts.startup_error.is_none());
            assert!(!ts.input.lock().expect("private input").limit_exceeded);
        }
        assert!(
            visible_loss,
            "discarded startup GOP is visible in push update"
        );
        assert!(ts.finish(NOW).expect("valid surviving input").media_loss);
        let actual = video_samples(&ts);
        assert!(!actual.is_empty(), "useful media recovers");
        // Compare bytes and order with the SAME generated input, including
        // every known-duration AU after the next clean IDR. The deliberate
        // two-tail exclusions and truncated-PES refusal remain unchanged.
        assert_eq!(
            actual.as_slice(),
            &expected[recovery..expected.len() - 2],
            "chunk={chunk}"
        );
        assert!(ts.dropped_packets() > 0);
        assert_eq!(
            ts.muxer().expect("recovered init").tracks(),
            TrackSet::AudioVideo
        );
        assert!(ts.startup.is_empty());
        assert_eq!(ts.startup_bytes, 0);
    }
}

#[test]
fn one_startup_continuity_gap_recovers_the_exact_remaining_generated_aus() {
    let lost = startup_video_packet(false);
    // Removing one continuation introduces one continuity gap. Every later
    // byte is original; do not mutate one counter and accidentally add a
    // second gap when the original counter sequence resumes.
    let damaged: Vec<_> = FIXTURE
        .chunks_exact(TS_SIZE)
        .enumerate()
        .filter(|(index, _)| *index != lost)
        .flat_map(|(_, packet)| packet.iter().copied())
        .collect();
    recovery_matches_the_generated_suffix(&damaged);
}

#[test]
fn malformed_startup_pes_recovers_the_exact_remaining_generated_aus() {
    let rejected = startup_video_packet(true);
    let mut damaged = FIXTURE.to_vec();
    let packet = &mut damaged[rejected * TS_SIZE..(rejected + 1) * TS_SIZE];
    let payload = 4 + if packet[3] & 0x20 != 0 {
        1 + usize::from(packet[4])
    } else {
        0
    };
    assert_eq!(&packet[payload..payload + 3], &[0, 0, 1]);
    packet[payload] = 0xff; // Reject this PES header, never publish its payload.
    recovery_matches_the_generated_suffix(&damaged);
}

#[test]
fn missing_aac_terminal_input_and_owner_drop_release_startup() {
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("IDR");
    assert!(retained(&ts) > 0);
    assert_eq!(ts.finish(NOW), Err(IngestError::InvalidTracks));
    released(&ts);
    assert_eq!(ts.finish(NOW), Ok(Update::default()));
    assert_eq!(ts.push(&[], NOW), Err(IngestError::InputFinished));

    let mut dropped = declared();
    frame(&mut dropped, 0, NOW).expect("IDR");
    let private_parser_owner = Arc::downgrade(&dropped.input);
    drop(dropped);
    assert!(
        private_parser_owner.upgrade().is_none(),
        "no private reader owner cycle"
    );
}

fn av_tables() -> Vec<u8> {
    use mpeg2ts::ts::{TsPacketWriter, WriteTsPacket};
    let mut reader = TsPacketReader::new(FIXTURE);
    let mut wire = Vec::new();
    while let Some(packet) = reader.read_ts_packet().expect("fixture TS") {
        let table = matches!(packet.payload, Some(TsPayload::Pat(_) | TsPayload::Pmt(_)));
        let pmt = matches!(packet.payload, Some(TsPayload::Pmt(_)));
        if table {
            TsPacketWriter::new(&mut wire)
                .write_ts_packet(&packet)
                .expect("table");
        }
        if pmt {
            break;
        }
    }
    wire
}

#[test]
fn authoritative_av_tables_alone_do_not_finish_as_a_valid_startup() {
    let mut ts = TsIngest::new(MuxerConfig::default());
    ts.push(&av_tables(), NOW).expect("authoritative tables");
    assert_eq!(ts.finish(NOW), Err(IngestError::InvalidTracks));
    released(&ts);
}

#[test]
fn av_tables_do_not_mask_the_existing_resource_failed_finish_contract() {
    let mut ts = TsIngest::new(MuxerConfig::default());
    ts.push(&av_tables(), NOW).expect("authoritative tables");
    // The existing PES-limit tests exercise real oversized allocations. Here
    // inject their exact terminal failure state to isolate the new A/V
    // readiness guard without another multi-megabyte test allocation.
    {
        let mut input = ts.input.lock().expect("private input");
        input.limit_exceeded = true;
        input.dropped += 1;
    }
    ts.release_failed_input();
    assert!(
        ts.finish(NOW)
            .expect("resource-failed prefix still seals")
            .media_loss
    );
    assert_eq!(ts.finish(NOW), Ok(Update::default()));
    assert_eq!(ts.push(&[], NOW), Err(IngestError::InputFinished));
    assert!(ts.muxer().is_none());
}

// Additive v5 regressions. These compile against v4 for independent red proof.
fn v5_audio_block(frames: usize) -> Vec<u8> {
    let fixture = include_bytes!("../tests/fixtures/transport-loss-6s.ts");
    let mut reader = PesPacketReader::new(TsPacketReader::new(&fixture[..]));
    let mut output = Vec::new();
    let mut count = 0;
    while let Some(pes) = reader.read_pes_packet().expect("generated PES") {
        if !(0xc0..=0xdf).contains(&pes.header.stream_id.as_u8()) {
            continue;
        }
        let mut data = pes.data.as_slice();
        while !data.is_empty() {
            let header = adts_reader::AdtsHeader::from_bytes(data).expect("generated ADTS");
            let bytes = usize::from(header.frame_length());
            output.extend_from_slice(&data[..bytes]);
            count += 1;
            if count == frames {
                return output;
            }
            data = &data[bytes..];
        }
    }
    panic!("fixture contains the requested generated audio prefix");
}

fn v5_budget_recovery(kind: &str) {
    let source = frames();
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("first held IDR");
    match kind {
        "count" => ts.startup_limits.aus = 1,
        "bytes" => {
            ts.startup_limits.bytes =
                retained(&ts) + codec::access_unit(&source[1].2).data.capacity() - 1;
        }
        "span" => ts.startup_limits.ticks = source[1].0 - source[0].0 - 1,
        _ => panic!("unknown injected budget"),
    }
    frame(&mut ts, 1, NOW).expect("late AAC budget discards only the optional held GOP");
    assert!(ts.startup.is_empty() && ts.waiting_keyframe);
    assert_eq!(ts.startup_bytes, 0);
    assert!(retained(&ts) <= ts.startup_limits.bytes);
    assert!(ts.pending.media_loss && ts.startup_error.is_none());
    assert!(!ts.input.lock().expect("private input").limit_exceeded);
    ts.push(&[], NOW).expect("transport input remains open");

    // Repeating a complete generated IDR at a later timestamp is a valid
    // encoder restart; the injected budget still applies unchanged.
    let dts = source[0].0 + 90_000;
    ts.on_video(
        (source[0].1 + 90_000) as u64,
        Some(dts as u64),
        &source[0].2,
        NOW,
    )
    .expect("fresh IDR is recoverable");
    assert_eq!(ts.startup.len(), 1);
    assert!(retained(&ts) <= ts.startup_limits.bytes);
    ts.on_audio(dts as u64, &v5_audio_block(16), NOW)
        .expect("late valid AAC starts the authoritative A/V presentation");
    assert_eq!(ts.es.origin, Some(dts));
    assert!(ts.startup.is_empty() && ts.startup_error.is_none());
    for (at, pts, data) in source.iter().skip(1) {
        ts.on_video(
            (*pts + 90_000) as u64,
            Some((*at + 90_000) as u64),
            data,
            NOW,
        )
        .expect("remaining valid generated AUs");
    }
    assert!(ts.muxer().expect("recovered A/V init").has_content());
    assert_eq!(ts.muxer().expect("A/V").tracks(), TrackSet::AudioVideo);
    assert_eq!(
        video_samples(&ts).first(),
        Some(&codec::access_unit(&source[0].2).data)
    );
}

#[test]
fn v5_startup_count_budget_overflow_recovers_without_disconnect() {
    v5_budget_recovery("count");
}

#[test]
fn v5_startup_byte_budget_overflow_recovers_without_disconnect() {
    v5_budget_recovery("bytes");
}

#[test]
fn v5_startup_span_budget_overflow_recovers_without_disconnect() {
    v5_budget_recovery("span");
}

#[test]
fn v5_parameter_capacity_budget_releases_and_recovers() {
    let mut ts = declared();
    ts.startup_limits.bytes = 8;
    let metadata = codec::AccessUnit {
        sps: Some(Vec::with_capacity(9)),
        ..Default::default()
    };
    ts.startup_video(0, 0, metadata, NOW)
        .expect("oversized optional metadata is discarded");
    assert!(ts.startup.is_empty() && ts.es.sps.is_none() && ts.es.pps.is_none());
    assert!(ts.pending.media_loss && ts.startup_error.is_none());
    assert!(ts.pes.is_some());
    ts.push(&[], NOW).expect("transport remains open");
    ts.startup_limits = StartupLimits::default();
    frame(&mut ts, 0, NOW).expect("new bounded IDR/config");
    let at = frames()[0].0;
    ts.on_audio(at as u64, &v5_audio_block(16), NOW)
        .expect("new supported A/V config");
    assert!(ts.muxer().is_some() && ts.startup.is_empty());
}

fn v5_fragment_samples(ts: &TsIngest, track_id: u32) -> Vec<(u64, Vec<u8>)> {
    let muxer = ts.muxer().expect("known A/V muxer");
    let mut samples = Vec::new();
    for id in 0..muxer.next_part_id() {
        let crate::PartLookup::Found(part) = muxer.part(id) else {
            panic!("all generated startup parts retained");
        };
        let moof = mp4_atom::Moof::decode(&mut part.data.as_ref()).expect("actual fragment");
        let video = moof
            .traf
            .iter()
            .find(|t| t.tfhd.track_id == 1)
            .expect("video track");
        let start = video
            .tfdt
            .as_ref()
            .expect("video tfdt")
            .base_media_decode_time;
        let end = start
            + video
                .trun
                .iter()
                .flat_map(|r| &r.entries)
                .map(|e| u64::from(e.duration.expect("video duration")))
                .sum::<u64>();
        let track = moof
            .traf
            .iter()
            .find(|t| t.tfhd.track_id == track_id)
            .expect("A/V track");
        let mut at = track
            .tfdt
            .as_ref()
            .expect("track tfdt")
            .base_media_decode_time;
        for run in &track.trun {
            let mut offset = run.data_offset.expect("actual payload offset") as usize;
            for entry in &run.entries {
                let bytes = entry.size.expect("actual payload length") as usize;
                if track_id == 2 {
                    let ticks = at * 90_000 / 48_000;
                    assert!(
                        ticks >= start && ticks < end,
                        "audio must belong to its actual video part, part={id} audio={ticks} video={start}..{end}"
                    );
                }
                samples.push((at, part.data[offset..offset + bytes].to_vec()));
                offset += bytes;
                at += u64::from(entry.duration.expect("actual sample duration"));
            }
        }
    }
    samples
}

#[test]
fn v5_exact_bound_late_aac_payloads_keep_their_part_and_timeline() {
    let source = source_frames(include_bytes!("../tests/fixtures/transport-loss-6s.ts"));
    let first_dts = source[0].0;
    let last = source
        .iter()
        .position(|f| f.0 - first_dts == 90_000)
        .expect("one-second generated boundary");
    assert!(codec::access_unit(&source[last].2).idr);
    let audio = v5_audio_block(60);
    let decoded = codec::adts_frames(&audio);
    assert_eq!(decoded.len(), 60);
    assert!(decoded.iter().all(|(c, _)| c.sample_rate == 48_000));
    for offset in [-1920i64, 0, 1920] {
        let mut ts = declared();
        for (dts, pts, data) in &source[..=last] {
            ts.on_video(*pts as u64, Some(*dts as u64), data, NOW)
                .expect("exact-bound video");
        }
        assert_eq!(ts.startup.len(), last + 1);
        assert!(retained(&ts) <= ts.startup_limits.bytes);
        assert!(ts.muxer().is_none(), "AAC before immutable init");
        ts.on_audio((first_dts + offset) as u64, &audio, NOW)
            .expect("late complete AAC batch");
        let muxer = ts.muxer().expect("A/V init");
        assert!(
            muxer.has_content(),
            "only replayed known video durations close a segment"
        );
        assert_eq!(muxer.tracks(), TrackSet::AudioVideo);
        let expected_video: Vec<_> = source[..last]
            .iter()
            .map(|(dts, _, data)| ((*dts - first_dts) as u64, codec::access_unit(data).data))
            .collect();
        assert_eq!(
            v5_fragment_samples(&ts, 1),
            expected_video,
            "original video bytes/DTS"
        );
        let expected_audio: Vec<_> = decoded
            .iter()
            .enumerate()
            .filter_map(|(index, (_, payload))| {
                let ticks = offset + index as i64 * 1920;
                if (0..90_000).contains(&ticks) {
                    Some(((ticks * 48_000 / 90_000) as u64, payload.to_vec()))
                } else {
                    None
                }
            })
            .collect();
        assert!(!expected_audio.is_empty());
        assert_eq!(
            v5_fragment_samples(&ts, 2),
            expected_audio,
            "exact generated AAC payloads/tfdt; before-IDR audio remains dropped, offset={offset}"
        );
    }
}

#[test]
fn v5_aac_before_second_video_au_cannot_publish_unknown_video_duration() {
    let source = frames();
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("one real IDR");
    ts.on_audio(source[0].0 as u64, &v5_audio_block(16), NOW)
        .expect("supported AAC");
    let muxer = ts.muxer().expect("known A/V config");
    assert!(!muxer.has_content());
    assert_eq!(muxer.next_part_id(), 0);
    ts.finish(NOW).expect("EOF without a second AU");
    assert!(
        video_samples(&ts).is_empty(),
        "no guessed duration or AAC-only fragment"
    );
    assert_eq!(ts.muxer().expect("A/V config").next_part_id(), 0);
    assert!(!ts.muxer().expect("A/V config").has_content());
}

// Further additive witnesses against the frozen v5 implementation.
fn v6_audio_timeline(ts: &TsIngest) -> Vec<(u64, Vec<u8>)> {
    let muxer = ts.muxer().expect("known A/V presentation");
    let mut samples = Vec::new();
    for id in 0..muxer.next_part_id() {
        let crate::PartLookup::Found(part) = muxer.part(id) else {
            panic!("all generated parts retained");
        };
        let moof = mp4_atom::Moof::decode(&mut part.data.as_ref()).expect("actual fragment");
        let video = moof
            .traf
            .iter()
            .find(|t| t.tfhd.track_id == 1)
            .expect("video track");
        let start = video
            .tfdt
            .as_ref()
            .expect("video tfdt")
            .base_media_decode_time;
        let end = start
            + video
                .trun
                .iter()
                .flat_map(|r| &r.entries)
                .map(|e| u64::from(e.duration.expect("video duration")))
                .sum::<u64>();
        let Some(track) = moof.traf.iter().find(|t| t.tfhd.track_id == 2) else {
            continue; // The final exact-payload comparison detects any omission.
        };
        let mut at = track
            .tfdt
            .as_ref()
            .expect("AAC tfdt")
            .base_media_decode_time;
        for run in &track.trun {
            let mut offset = run.data_offset.expect("actual AAC offset") as usize;
            for entry in &run.entries {
                let bytes = entry.size.expect("actual AAC length") as usize;
                let ticks = at * 90_000 / 48_000;
                assert!(
                    ticks >= start && ticks < end,
                    "late AAC is in the wrong actual video part: part={id} audio={ticks} video={start}..{end}"
                );
                samples.push((at, part.data[offset..offset + bytes].to_vec()));
                offset += bytes;
                at += u64::from(entry.duration.expect("actual AAC duration"));
            }
        }
    }
    samples
}

#[test]
fn v6_short_first_aac_then_catchup_never_misplaces_payloads() {
    let source = source_frames(include_bytes!("../tests/fixtures/transport-loss-6s.ts"));
    let initial = source[0].0;
    let last = source
        .iter()
        .position(|f| f.0 - initial == 90_000)
        .expect("generated one-second IDR");
    assert!(codec::access_unit(&source[last].2).idr);
    let first = v5_audio_block(16);
    let full = v5_audio_block(60);
    assert!(full.starts_with(&first));
    let mut ts = declared();
    for (dts, pts, data) in &source[..=last] {
        ts.on_video(*pts as u64, Some(*dts as u64), data, NOW)
            .expect("held valid one-second video");
    }
    ts.on_audio(initial as u64, &first, NOW)
        .expect("first short valid AAC PES");
    let mut discarded = ts.pending.media_loss;
    ts.on_audio((initial + 16 * 1920) as u64, &full[first.len()..], NOW)
        .expect("subsequent catch-up AAC PES");
    discarded |= ts.pending.media_loss;
    for (dts, pts, data) in &source[last + 1..=last + 6] {
        ts.on_video(*pts as u64, Some(*dts as u64), data, NOW)
            .expect("following valid video");
    }
    let origin = if ts.es.origin.is_none() {
        // An explicit bounded GOP-discard/fresh-IDR policy is permitted.
        assert!(discarded && ts.waiting_keyframe && ts.startup.is_empty());
        assert!(ts.startup_error.is_none());
        assert!(
            ts.muxer()
                .is_none_or(|m| !m.has_content() && m.next_part_id() == 0),
            "discard must not leave a servable wrong-timeline prefix"
        );
        let recovery = source
            .iter()
            .enumerate()
            .skip(last + 7)
            .find(|(_, f)| codec::access_unit(&f.2).idr)
            .map(|(i, _)| i)
            .expect("later generated recovery IDR");
        for (dts, pts, data) in &source[recovery..=recovery + 1] {
            ts.on_video(*pts as u64, Some(*dts as u64), data, NOW)
                .expect("fresh IDR plus real lookahead");
        }
        ts.on_audio(source[recovery].0 as u64, &full, NOW)
            .expect("new aligned AAC prefix");
        for (dts, pts, data) in &source[recovery + 2..=recovery + last + 6] {
            ts.on_video(*pts as u64, Some(*dts as u64), data, NOW)
                .expect("useful recovered source video");
        }
        recovery
    } else {
        0
    };
    let end = origin + last + 6;
    let expected_video: Vec<_> = source[origin..end]
        .iter()
        .map(|(dts, _, data)| {
            (
                (*dts - source[origin].0) as u64,
                codec::access_unit(data).data,
            )
        })
        .collect();
    assert_eq!(
        v5_fragment_samples(&ts, 1),
        expected_video,
        "exact useful video bytes/DTS"
    );
    let stop = source[end].0 - source[origin].0;
    let decoded = codec::adts_frames(&full);
    let expected_audio: Vec<_> = decoded
        .iter()
        .enumerate()
        .filter_map(|(i, (_, payload))| {
            let ticks = i as i64 * 1920;
            if ticks < stop {
                Some(((ticks * 48_000 / 90_000) as u64, payload.to_vec()))
            } else {
                None
            }
        })
        .collect();
    assert!(!expected_audio.is_empty());
    assert_eq!(
        v6_audio_timeline(&ts),
        expected_audio,
        "all known generated AAC payloads belong to their video parts/timestamps"
    );
    assert_eq!(
        ts.muxer().expect("recovered A/V").tracks(),
        TrackSet::AudioVideo
    );
}

#[test]
fn v6_one_idr_continuing_aac_cannot_escape_the_startup_bound() {
    let source = frames();
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("one real complete IDR");
    // Exercise fixed audio defaults without a new v5-incompatible field.
    let audio = v5_audio_block(16);
    for batch in 0..64 {
        let pts = source[0].0 + i64::from(batch) * 16 * 1920;
        ts.on_audio(pts as u64, &audio, NOW)
            .expect("valid ongoing AAC without another video AU");
        let (bytes, count, span) = ts
            .muxer()
            .map_or((0, 0, None), Muxer::staged_audio_for_test);
        assert!(
            bytes <= 64 * 1024,
            "unserved startup AAC owns {bytes} logical payload bytes"
        );
        assert!(count <= 64, "unserved startup AAC retains {count} frames");
        assert!(
            span.is_none_or(|ticks| ticks <= 90_000),
            "unserved startup AAC span {span:?} exceeds one second"
        );
        assert!(ts.startup_error.is_none());
        assert!(!ts.input.lock().expect("private input").limit_exceeded);
        assert!(
            ts.muxer()
                .is_none_or(|m| !m.has_content() && m.next_part_id() == 0)
        );
    }
    ts.finish(NOW)
        .expect("terminal release without guessed video duration");
    assert!(
        ts.muxer()
            .is_none_or(|m| !m.has_content() && m.next_part_id() == 0)
    );
}

#[test]
fn v6_backwards_successor_keeps_audio_bounded() {
    let source = frames();
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("first generated IDR");
    ts.on_audio(source[0].0 as u64, &v5_audio_block(1), NOW)
        .expect("one real AAC frame establishes the pending frontier");
    assert!(ts.startup_replaying);
    // A complete second AU can still be unusable as duration lookahead.
    // Muxer::push_video returns Ok without accepting a negative relative DTS;
    // that must not silently turn off the bounded one-IDR startup phase.
    ts.on_video(
        (source[0].1 - 3000) as u64,
        Some((source[0].0 - 3000) as u64),
        &source[1].2,
        NOW,
    )
    .expect("out-of-order successor remains bounded");
    let audio = v5_audio_block(16);
    for batch in 0..64 {
        let pts = source[0].0 + 1920 + i64::from(batch) * 16 * 1920;
        ts.on_audio(pts as u64, &audio, NOW)
            .expect("ongoing valid AAC");
        let (bytes, count, span) = ts
            .muxer()
            .map_or((0, 0, None), Muxer::staged_audio_for_test);
        assert!(
            bytes <= 64 * 1024,
            "rejected successor left {bytes} unserved AAC bytes"
        );
        assert!(
            count <= 64,
            "rejected successor left {count} unserved AAC frames"
        );
        assert!(
            span.is_none_or(|ticks| ticks <= 90_000),
            "rejected successor escaped the effective startup span: {span:?}"
        );
        assert!(
            ts.muxer()
                .is_none_or(|m| !m.has_content() && m.next_part_id() == 0)
        );
    }
    ts.finish(NOW)
        .expect("terminal release without invented duration");
}

// Resource enforcement only: valid generated ADTS envelopes with opaque
// hostile AAC payloads. These tests make no codec/decode-validity claim.
fn v6_resource_adts(payload_bytes: usize) -> Vec<u8> {
    let generated = v5_audio_block(1);
    assert_eq!(generated[1] & 1, 1, "no CRC in generated ADTS header");
    let length = 7 + payload_bytes;
    assert!(length <= 8191);
    let mut wire = generated[..7].to_vec();
    wire[3] = (wire[3] & 0xfc) | ((length >> 11) as u8 & 3);
    wire[4] = (length >> 3) as u8;
    wire[5] = (wire[5] & 0x1f) | ((length as u8 & 7) << 5);
    wire.resize(length, 0x5a);
    let parsed = codec::adts_frames(&wire);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].1.len(), payload_bytes);
    wire
}

fn v6_audio_refusal_keeps_input_open(ts: &mut TsIngest) {
    assert!(ts.pending.media_loss && ts.waiting_keyframe);
    assert!(!ts.startup_replaying && ts.startup_error.is_none());
    assert!(ts.startup.is_empty() && ts.muxer().is_none());
    assert!(!ts.input.lock().expect("private input").limit_exceeded);
    ts.push(&[], NOW)
        .expect("optional audio refusal is not a transport failure");
}

#[test]
fn v6_hard_aac_byte_refusal_runs_with_count_and_span_below_limits() {
    let source = frames();
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("one generated IDR");
    let wire = v6_resource_adts(8184); // Maximum parse-valid ADTS frame payload.
    for count in 1..=8 {
        ts.on_audio(source[0].0 as u64, &wire, NOW)
            .expect("hard byte boundary prefix");
        let (bytes, frames, span) = ts
            .muxer()
            .expect("bounded prepared state")
            .staged_audio_for_test();
        assert_eq!(bytes, count * 8184);
        assert_eq!(frames, count);
        assert_eq!(span, Some(0), "repeated effective timestamp isolates bytes");
    }
    let projection = ts
        .muxer()
        .expect("eight retained frames")
        .startup_audio_projection(0, 8184);
    assert_eq!(projection, (9 * 8184, 9));
    assert!(projection.0 > 64 * 1024 && projection.1 < 64);
    ts.on_audio(source[0].0 as u64, &wire, NOW)
        .expect("byte overflow discards before copying");
    v6_audio_refusal_keeps_input_open(&mut ts);
}

#[test]
fn v6_hard_aac_frame_refusal_runs_with_bytes_and_span_below_limits() {
    let source = frames();
    let mut ts = declared();
    frame(&mut ts, 0, NOW).expect("one generated IDR");
    let wire = v6_resource_adts(1);
    for count in 1..=64 {
        ts.on_audio(source[0].0 as u64, &wire, NOW)
            .expect("hard frame-count boundary prefix");
        let (bytes, frames, span) = ts
            .muxer()
            .expect("bounded prepared state")
            .staged_audio_for_test();
        assert_eq!((bytes, frames, span), (count, count, Some(0)));
    }
    let projection = ts
        .muxer()
        .expect("64 retained frames")
        .startup_audio_projection(0, 1);
    assert_eq!(projection, (65, 65));
    assert!(projection.0 < 64 * 1024 && projection.1 > 64);
    ts.on_audio(source[0].0 as u64, &wire, NOW)
        .expect("frame overflow discards before copying");
    v6_audio_refusal_keeps_input_open(&mut ts);
}
