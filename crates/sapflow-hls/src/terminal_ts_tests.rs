//! Generated TS boundary tests; no sockets, camera media or sender timing.
use std::time::SystemTime;

use mpeg2ts::es::StreamId;
use mpeg2ts::pes::PesHeader;
use mpeg2ts::time::Timestamp;
use mpeg2ts::ts::payload::{Bytes as TsBytes, Pes};
use mpeg2ts::ts::{
    ContinuityCounter, Pid, ReadTsPacket, TransportScramblingControl, TsHeader, TsPacket,
    TsPacketReader, TsPacketWriter, TsPayload, WriteTsPacket,
};

use crate::muxer::Entry;
use crate::{IngestError, MuxerConfig, TsIngest, Update};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;
// AAC-LC 48 kHz mono ADTS with three generated payload bytes. The test checks
// packaging and exact sample counts, not whether these bytes decode to PCM.
const ADTS: &[u8] = &[0xff, 0xf1, 0x4c, 0x40, 0x01, 0x5f, 0xfc, 1, 2, 3];

fn tables() -> (Vec<u8>, Pid) {
    let fixture = include_bytes!("../tests/fixtures/audio-48k-mono.ts");
    let mut reader = TsPacketReader::new(&fixture[..]);
    let mut output = Vec::new();
    while let Some(packet) = reader.read_ts_packet().expect("generated fixture TS") {
        let audio = match &packet.payload {
            Some(TsPayload::Pmt(pmt)) => Some(pmt.es_info[0].elementary_pid),
            _ => None,
        };
        if matches!(packet.payload, Some(TsPayload::Pat(_) | TsPayload::Pmt(_))) {
            TsPacketWriter::new(&mut output)
                .write_ts_packet(&packet)
                .expect("write generated program tables");
        }
        if let Some(pid) = audio {
            return (output, pid);
        }
    }
    panic!("fixture contains an AAC PMT");
}

fn tables_with_extra_audio_pid() -> (Vec<u8>, Pid, Pid) {
    let (wire, pid) = tables();
    let other = Pid::new(pid.as_u16() + 1).expect("adjacent audio PID");
    let mut reader = TsPacketReader::new(&wire[..]);
    let mut output = Vec::new();
    while let Some(mut packet) = reader.read_ts_packet().expect("program tables") {
        if let Some(TsPayload::Pmt(pmt)) = &mut packet.payload {
            let mut audio = pmt.es_info[0].clone();
            audio.elementary_pid = other;
            pmt.es_info.push(audio);
        }
        TsPacketWriter::new(&mut output)
            .write_ts_packet(&packet)
            .expect("write two-PID table");
    }
    (output, pid, other)
}

fn start(pid: Pid, stream_id: u8, packet_len: u16, payload: &[u8]) -> Vec<u8> {
    start_at(pid, stream_id, packet_len, payload, 90_000, 0)
}

fn start_at(
    pid: Pid,
    stream_id: u8,
    packet_len: u16,
    payload: &[u8],
    pts: u64,
    counter: u8,
) -> Vec<u8> {
    start_pts(pid, stream_id, packet_len, payload, Some(pts), counter)
}

fn start_pts(
    pid: Pid,
    stream_id: u8,
    packet_len: u16,
    payload: &[u8],
    pts: Option<u64>,
    counter: u8,
) -> Vec<u8> {
    let packet = TsPacket {
        header: TsHeader {
            transport_error_indicator: false,
            transport_priority: false,
            pid,
            transport_scrambling_control: TransportScramblingControl::NotScrambled,
            continuity_counter: ContinuityCounter::from_u8(counter).expect("four-bit counter"),
        },
        adaptation_field: None,
        payload: Some(TsPayload::PesStart(Pes {
            header: PesHeader {
                stream_id: StreamId::new(stream_id),
                priority: false,
                data_alignment_indicator: true,
                copyright: false,
                original_or_copy: false,
                pts: pts.map(|pts| Timestamp::new(pts).expect("valid PTS")),
                dts: None,
                escr: None,
            },
            pes_packet_len: packet_len,
            data: TsBytes::new(payload).expect("payload fits one TS packet"),
        })),
    };
    let mut output = Vec::new();
    TsPacketWriter::new(&mut output)
        .write_ts_packet(&packet)
        .expect("write generated START packet");
    assert_eq!(output.len(), 188);
    output
}

fn stuffed_start(pid: Pid, declared: u16, payload: &[u8]) -> Vec<u8> {
    let ordinary = start(pid, 0xc0, declared, payload);
    let old_at = usize::from(ordinary[4]) + 5;
    let at = old_at - 1;
    let mut packet = vec![0xff; 188];
    packet[..4].copy_from_slice(&ordinary[..4]);
    packet[4] = ordinary[4] - 1;
    packet[5] = 0;
    packet[at..at + 14].copy_from_slice(&ordinary[old_at..old_at + 14]);
    packet[at + 8] = 6; // five-byte PTS followed by one permitted FF stuffing byte.
    packet[at + 14] = 0xff;
    packet[at + 15..].copy_from_slice(payload);
    packet
}

fn continuation(pid: Pid, counter: u8, data: &[u8]) -> Vec<u8> {
    let packet = TsPacket {
        header: TsHeader {
            transport_error_indicator: false,
            transport_priority: false,
            pid,
            transport_scrambling_control: TransportScramblingControl::NotScrambled,
            continuity_counter: ContinuityCounter::from_u8(counter).expect("four-bit counter"),
        },
        adaptation_field: None,
        payload: Some(TsPayload::PesContinuation(
            TsBytes::new(data).expect("small continuation"),
        )),
    };
    let mut wire = Vec::new();
    TsPacketWriter::new(&mut wire)
        .write_ts_packet(&packet)
        .expect("write continuation");
    wire
}

#[test]
fn complete_start_only_aac_is_not_lost_at_terminal_input() {
    let (mut wire, pid) = tables();
    wire.extend(start(pid, 0xc0, 18, ADTS));
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&wire, NOW).expect("valid bounded AAC PES");
    assert!(ingest.muxer().is_none(), "starvation is not terminal input");
    assert_eq!(
        ingest.push(&[], NOW).expect("starved read"),
        Update::default()
    );
    assert!(ingest.muxer().is_none(), "empty pushes must not imply EOF");
    let update = ingest.finish(NOW).expect("clean bounded terminal AAC");
    assert!(update.part && update.segment && !update.media_loss);
    assert_eq!(samples(&ingest), 1);
    assert_eq!(ingest.dropped_packets(), 0);
    assert_eq!(
        ingest.finish(NOW).expect("repeat finish"),
        Update::default()
    );
    assert_eq!(
        samples(&ingest),
        1,
        "idempotent finish cannot duplicate AAC"
    );
    assert_eq!(ingest.push(&[], NOW), Err(IngestError::InputFinished));
    assert_eq!(ingest.push(&wire, NOW), Err(IngestError::InputFinished));
}

fn samples(ingest: &TsIngest) -> usize {
    use mp4_atom::Decode;
    ingest.muxer().map_or(0, |muxer| {
        muxer
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                Entry::Segment(s) => Some(s),
                _ => None,
            })
            .flat_map(|s| &s.parts)
            .map(|part| {
                let moof = mp4_atom::Moof::decode(&mut std::io::Cursor::new(&part.data[..]))
                    .expect("actual MP4 moof");
                moof.traf
                    .iter()
                    .flat_map(|t| &t.trun)
                    .map(|r| r.entries.len())
                    .sum::<usize>()
            })
            .sum()
    })
}

#[test]
fn terminal_partial_ts_and_truncated_bounded_pes_report_loss_without_media() {
    for tail in 1..188 {
        let (mut wire, pid) = tables();
        wire.extend(&start(pid, 0xc0, 18, ADTS)[..tail]);
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .push(&wire, NOW)
            .expect("partial TS waits for more bytes");
        let update = ingest.finish(NOW).expect("explicit truncated EOF");
        assert!(update.media_loss, "partial packet length {tail}");
        assert_eq!(samples(&ingest), 0);
        assert!(ingest.dropped_packets() > 0);
    }
    for advertised in [19, 65_535] {
        let (mut wire, pid) = tables();
        wire.extend(start(pid, 0xc0, advertised, ADTS));
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .push(&wire, NOW)
            .expect("incomplete bounded PES waits");
        assert!(ingest.finish(NOW).expect("reject truncated PES").media_loss);
        assert_eq!(samples(&ingest), 0, "partial PES cannot escape at EOF");
        assert_eq!(ingest.dropped_packets(), 1);
    }
}

#[test]
fn bounded_pes_with_truncated_adts_keeps_only_complete_frame_and_reports_loss() {
    let (mut wire, pid) = tables();
    let mut adts = ADTS.to_vec();
    adts.extend(&ADTS[..8]);
    wire.extend(start(pid, 0xc0, 26, &adts));
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest
        .push(&wire, NOW)
        .expect("bounded PES is syntactically complete");
    let update = ingest
        .finish(NOW)
        .expect("validated AAC prefix remains usable");
    assert!(update.media_loss && update.part);
    assert_eq!(samples(&ingest), 1);
}

#[test]
fn unbounded_tail_is_never_flushed_or_invented_as_definite_loss() {
    for stream_id in [0xc0, 0xe0] {
        let (mut wire, pid) = tables();
        wire.extend(start(pid, stream_id, 0, ADTS));
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .push(&wire, NOW)
            .expect("unbounded PES can await another START");
        assert_eq!(
            ingest.finish(NOW).expect("discard uncertain tail"),
            Update::default()
        );
        assert_eq!(samples(&ingest), 0);
        assert_eq!(ingest.dropped_packets(), 0);
    }
}

#[test]
fn real_audio_fixtures_finish_with_exact_source_frame_counts_at_every_alignment() {
    for (fixture, frames) in [
        (
            &include_bytes!("../tests/fixtures/audio-48k-mono.ts")[..],
            283,
        ),
        (
            &include_bytes!("../tests/fixtures/audio-44k1-stereo.ts")[..],
            260,
        ),
    ] {
        for chunk in [1, 187, 188, 1316, fixture.len()] {
            let mut ingest = TsIngest::new(MuxerConfig {
                segment_count: 100,
                ..MuxerConfig::default()
            });
            for bytes in fixture.chunks(chunk) {
                ingest.push(bytes, NOW).expect("generated fixture chunk");
            }
            assert!(
                !ingest
                    .finish(NOW)
                    .expect("complete generated TS EOF")
                    .media_loss
            );
            assert_eq!(samples(&ingest), frames, "source samples, chunk={chunk}");
            assert_eq!(ingest.dropped_packets(), 0);
        }
    }
}

#[test]
fn invalid_bytes_and_continuity_loss_cannot_become_clean_terminal_receipts() {
    let (mut wire, pid) = tables();
    wire.extend(start(pid, 0xc0, 18, ADTS));
    wire.extend([1, 2, 3]);
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&wire, NOW).expect("resync rejects bad bytes");
    assert!(
        ingest
            .finish(NOW)
            .expect("known bad terminal bytes")
            .media_loss
    );
    assert_eq!(
        samples(&ingest),
        0,
        "sync loss discards pending PES assembly"
    );
}

#[test]
fn normal_multi_packet_completion_is_not_replayed_and_missing_continuation_is_loss() {
    for counter in [1, 2] {
        let (mut wire, pid) = tables();
        wire.extend(start(pid, 0xc0, 18, &ADTS[..8]));
        wire.extend(continuation(pid, counter, &ADTS[8..]));
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .push(&wire, NOW)
            .expect("continuation is parsed or rejected");
        let update = ingest.finish(NOW).expect("finite PES tail");
        if counter == 1 {
            assert!(!update.media_loss);
            assert_eq!(
                samples(&ingest),
                1,
                "completed continuation emitted exactly once"
            );
        } else {
            assert!(update.media_loss);
            assert_eq!(
                samples(&ingest),
                0,
                "CC gap cannot concatenate a truncated PES"
            );
        }
    }
}

#[test]
fn terminal_bounded_and_unbounded_audio_are_isolated_by_pid() {
    let (mut wire, pid, other) = tables_with_extra_audio_pid();
    wire.extend(start(pid, 0xc0, 18, ADTS));
    wire.extend(start(other, 0xc1, 0, ADTS));
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&wire, NOW).expect("independent PES assembly");
    assert!(
        !ingest
            .finish(NOW)
            .expect("only proven AAC released")
            .media_loss
    );
    assert_eq!(samples(&ingest), 1);
    assert!(ingest.muxer().expect("AAC muxer").video().is_none());
}

#[test]
fn ambiguous_same_header_pid_tails_fail_closed_and_report_bounded_media_discard() {
    let (mut wire, pid, other) = tables_with_extra_audio_pid();
    wire.extend(start(pid, 0xc0, 18, ADTS));
    wire.extend(start(other, 0xc0, 18, ADTS));
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest
        .push(&wire, NOW)
        .expect("two pending PES with identical headers");
    assert!(
        ingest
            .finish(NOW)
            .expect("cannot infer a PID from an ambiguous header")
            .media_loss
    );
    assert_eq!(samples(&ingest), 0);
}

#[test]
fn finishing_tables_alone_does_not_invent_media_and_resume_uses_new_parser() {
    let (tables, pid) = tables();
    let mut empty = TsIngest::new(MuxerConfig::default());
    empty.push(&tables, NOW).expect("program only");
    assert_eq!(
        empty.finish(NOW).expect("header-only finish"),
        Update::default()
    );
    assert!(empty.muxer().is_none());

    let mut wire = tables.clone();
    wire.extend(start(pid, 0xc0, 18, ADTS));
    let mut first = TsIngest::new(MuxerConfig::default());
    first.push(&wire, NOW).expect("first connection");
    first.finish(NOW).expect("first connection closes");
    let old_part = first
        .muxer()
        .expect("first muxer")
        .segment(crate::muxer::LEADING_GAPS)
        .expect("completed one-frame segment")
        .parts[0]
        .clone();
    let muxer = first.take_muxer().expect("retain presentation");
    let mut next = TsIngest::resume(MuxerConfig::default(), muxer);
    next.push(&wire, NOW)
        .expect("fresh parser permits timestamp reset");
    assert!(
        !next
            .finish(NOW)
            .expect("second complete connection")
            .media_loss
    );
    assert_eq!(samples(&next), 2);
    let retained = next
        .muxer()
        .expect("continued muxer")
        .segment(crate::muxer::LEADING_GAPS)
        .expect("retained first segment")
        .parts[0]
        .clone();
    assert_eq!(old_part.data, retained.data);
    assert_eq!(old_part.initialization.id, retained.initialization.id);
    assert!(next.muxer().expect("muxer").next_part_id() > old_part.id + 1);
}

#[test]
fn legacy_av_finish_keeps_accepted_prefix_without_emitting_unknown_last_video() {
    let fixture = include_bytes!("../tests/fixtures/testsrc-3s.ts");
    let mut normal = TsIngest::new(MuxerConfig::default());
    normal.push(fixture, NOW).expect("legacy AV fixture");
    normal.muxer_mut().expect("AV muxer").interrupt(NOW);
    let expected = samples(&normal);
    let mut terminal = TsIngest::new(MuxerConfig::default());
    terminal.push(fixture, NOW).expect("same legacy fixture");
    assert!(
        !terminal
            .finish(NOW)
            .expect("normal AV termination")
            .media_loss
    );
    assert_eq!(
        samples(&terminal),
        expected,
        "no unbounded final video emitted"
    );
    assert_eq!(terminal.dropped_packets(), 0);
}

#[test]
fn terminal_semantic_error_still_seals_the_valid_accepted_prefix() {
    let (mut wire, pid) = tables();
    wire.extend(start_at(pid, 0xc0, 18, ADTS, 90_000, 0));
    let changed = [0xff, 0xf1, 0x50, 0x80, 0x01, 0x5f, 0xfc, 1, 2, 3];
    wire.extend(start_at(pid, 0xc0, 18, &changed, 91_920, 1));
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest
        .push(&wire, NOW)
        .expect("first 48 kHz AAC accepted before next START");
    assert_eq!(samples(&ingest), 0, "accepted frame is not sealed yet");
    assert_eq!(ingest.finish(NOW), Err(IngestError::InvalidAudio));
    assert_eq!(
        samples(&ingest),
        1,
        "error must not discard valid accepted prefix"
    );
    assert_eq!(
        ingest
            .muxer()
            .expect("old muxer remains available")
            .audio()
            .expect("accepted old audio config")
            .sample_rate,
        48_000
    );
    assert_eq!(
        ingest
            .finish(NOW)
            .expect("terminal error does not rearm drain"),
        Update::default()
    );
    assert_eq!(ingest.push(&wire, NOW), Err(IngestError::InputFinished));
}

#[test]
fn raw_stuffed_header_overrun_cannot_be_accepted_as_clean_terminal_aac() {
    let (mut wire, pid) = tables();
    // Packet length18 minus actual nine-byte optional header advertises nine
    // payload bytes, but this packet carries ten. Parsed fields omit stuffing.
    wire.extend(stuffed_start(pid, 18, ADTS));
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest
        .push(&wire, NOW)
        .expect("PES length checked at terminal boundary");
    assert!(ingest.finish(NOW).expect("reject raw overrun").media_loss);
    assert_eq!(samples(&ingest), 0);
}

#[test]
fn terminal_bounded_aac_without_pts_cannot_be_silently_discarded_as_clean() {
    let (mut wire, pid) = tables();
    wire.extend(start_pts(pid, 0xc0, 13, ADTS, None, 0));
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest
        .push(&wire, NOW)
        .expect("bounded PES without timestamp");
    assert!(
        ingest
            .finish(NOW)
            .expect("unusable AAC timestamp")
            .media_loss
    );
    assert_eq!(samples(&ingest), 0);
}

#[test]
fn raw_stuffed_header_exact_and_truncated_lengths_use_actual_header_extent() {
    for (declared, valid) in [(19, true), (20, false), (8, false)] {
        let (mut wire, pid) = tables();
        wire.extend(stuffed_start(pid, declared, ADTS));
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .push(&wire, NOW)
            .expect("stuffing validated before assembly");
        let update = ingest.finish(NOW).expect("terminal length result");
        assert_eq!(
            update.media_loss, !valid,
            "actual header length, PES {declared}"
        );
        assert_eq!(samples(&ingest), usize::from(valid));
    }
}

#[test]
fn stuffed_header_payload_completes_normally_on_continuation_or_next_start() {
    for continuation_case in [false, true] {
        let (mut wire, pid) = tables();
        if continuation_case {
            wire.extend(stuffed_start(pid, 19, &ADTS[..8]));
            wire.extend(continuation(pid, 1, &ADTS[8..]));
        } else {
            wire.extend(stuffed_start(pid, 19, ADTS));
            wire.extend(start_at(pid, 0xc0, 18, ADTS, 91_920, 1));
        }
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .push(&wire, NOW)
            .expect("normal stuffed-header streaming");
        assert!(!ingest.finish(NOW).expect("clean finish").media_loss);
        assert_eq!(samples(&ingest), if continuation_case { 1 } else { 2 });
        assert_eq!(ingest.dropped_packets(), 0);
    }
}
