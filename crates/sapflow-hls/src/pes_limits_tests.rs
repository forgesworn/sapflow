//! Original generated TS exercises retention before a PES reaches the muxer.
use super::*;
use mpeg2ts::es::{StreamId, StreamType};
use mpeg2ts::time::Timestamp;
use mpeg2ts::ts::payload::{Bytes as TsBytes, Pes};
use mpeg2ts::ts::{
    ContinuityCounter, TransportScramblingControl, TsHeader, TsPacketWriter, WriteTsPacket,
};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;
const PES_CAP: usize = 8 * 1024 * 1024;

fn write(packet: &TsPacket) -> Vec<u8> {
    let mut output = Vec::new();
    TsPacketWriter::new(&mut output)
        .write_ts_packet(packet)
        .expect("write original generated TS packet");
    assert_eq!(output.len(), TS_SIZE);
    output
}

fn tables(pid: u16, video: bool) -> Vec<u8> {
    let fixture = include_bytes!("../tests/fixtures/audio-48k-mono.ts");
    let mut reader = TsPacketReader::new(&fixture[..]);
    let mut wire = Vec::new();
    while let Some(mut packet) = reader.read_ts_packet().expect("generated AAC tables") {
        if let Some(TsPayload::Pmt(pmt)) = &mut packet.payload {
            pmt.es_info[0].elementary_pid = Pid::new(pid).expect("test PID");
            pmt.es_info[0].stream_type = if video {
                StreamType::H264
            } else {
                StreamType::AdtsAac
            };
            wire.extend(write(&packet));
            return wire;
        }
        if matches!(packet.payload, Some(TsPayload::Pat(_))) {
            wire.extend(write(&packet));
        }
    }
    panic!("generated fixture must contain PAT/PMT");
}

fn packet(pid: u16, counter: u8, payload: TsPayload) -> Vec<u8> {
    write(&TsPacket {
        header: TsHeader {
            transport_error_indicator: false,
            transport_priority: false,
            pid: Pid::new(pid).expect("test PID"),
            transport_scrambling_control: TransportScramblingControl::NotScrambled,
            continuity_counter: ContinuityCounter::from_u8(counter).expect("test CC"),
        },
        adaptation_field: None,
        payload: Some(payload),
    })
}

fn start(pid: u16, counter: u8, declared: u16, data: &[u8]) -> Vec<u8> {
    packet(
        pid,
        counter,
        TsPayload::PesStart(Pes {
            header: PesHeader {
                stream_id: StreamId::new(0xe0),
                priority: false,
                data_alignment_indicator: true,
                copyright: false,
                original_or_copy: false,
                pts: Some(Timestamp::new(90_000).expect("test PTS")),
                dts: None,
                escr: None,
            },
            pes_packet_len: declared,
            data: TsBytes::new(data).expect("small START payload"),
        }),
    )
}

fn grow(ingest: &mut TsIngest, pid: u16, bytes: usize) -> Result<Update, IngestError> {
    let initial = bytes.min(170);
    let mut result = ingest.push(&start(pid, 0, 0, &vec![0xa5; initial]), NOW)?;
    let mut remaining = bytes - initial;
    let mut counter = 1;
    while remaining != 0 {
        let len = remaining.min(184);
        let wire = packet(
            pid,
            counter,
            TsPayload::PesContinuation(TsBytes::new(&vec![0xa5; len]).expect("small continuation")),
        );
        result = ingest.push(&wire, NOW)?;
        remaining -= len;
        counter = (counter + 1) & 15;
    }
    Ok(result)
}

#[test]
fn valid_continuity_unbounded_video_cannot_retain_beyond_one_pes_limit() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    assert!(
        grow(&mut ingest, 256, PES_CAP + 1).is_err(),
        "reject before the upstream unbounded Vec crosses 8 MiB"
    );
}

#[test]
fn rotated_pmt_pids_cannot_preallocate_beyond_aggregate_limit() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    let mut refused = false;
    for pid in 256..=512 {
        ingest.push(&tables(pid, true), NOW).expect("rotated PMT");
        if ingest.push(&start(pid, 0, u16::MAX, &[0xa5]), NOW).is_err() {
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "257 starts reserve over 16 MiB despite only 257 data bytes"
    );
}

#[test]
fn oversized_raw_push_is_refused_before_copying_or_parsing() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    assert!(
        ingest.push(&vec![0; MAX_BUFFER + 1], NOW).is_err(),
        "the raw unread-input cap must reject a single oversized slice"
    );
}

fn retained(ingest: &TsIngest) -> usize {
    ingest
        .pes
        .as_ref()
        .expect("healthy parser")
        .ts_packet_reader()
        .pending_bytes
}

fn assert_failed_and_released(ingest: &mut TsIngest) {
    assert!(
        ingest.pes.is_none(),
        "all upstream payload assemblies were dropped"
    );
    {
        let input = ingest.input.lock().expect("private input lock");
        assert_eq!(input.buf.capacity(), 0, "unread allocation released too");
        assert!(input.limit_exceeded);
    }
    assert_eq!(
        ingest.push(&tables(256, false), NOW),
        Err(IngestError::InputLimitExceeded)
    );
    assert!(ingest.finish(NOW).expect("seal accepted prefix").media_loss);
    assert_eq!(
        ingest.finish(NOW).expect("idempotent finish"),
        Update::default()
    );
    assert_eq!(
        ingest.push(&[], NOW),
        Err(IngestError::InputFinished),
        "existing post-finish contract takes priority"
    );
}

#[test]
fn exact_pes_boundary_is_allowed_then_one_extra_byte_releases_every_assembly() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    grow(&mut ingest, 256, PES_CAP).expect("exact 8 MiB payload is allowed");
    assert_eq!(retained(&ingest), PES_CAP);
    let previous = ingest
        .pes
        .as_ref()
        .expect("parser")
        .ts_packet_reader()
        .continuity
        .get(&Pid::new(256).expect("PID"))
        .expect("last CC")
        .0;
    let extra = packet(
        256,
        (previous + 1) & 15,
        TsPayload::PesContinuation(TsBytes::new(&[1]).expect("one byte")),
    );
    assert_eq!(
        ingest.push(&extra, NOW),
        Err(IngestError::InputLimitExceeded)
    );
    assert_failed_and_released(&mut ingest);
}

#[test]
fn same_pid_start_must_fit_old_and_new_reservations_simultaneously() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    for pid in 256..512 {
        ingest.push(&tables(pid, true), NOW).expect("rotated PMT");
        ingest
            .push(&start(pid, 0, u16::MAX, &[1]), NOW)
            .expect("bounded reservation");
    }
    assert_eq!(retained(&ingest), 256 * (usize::from(u16::MAX) - 8));
    // Replacing the first incomplete PES does not first free it upstream:
    // the new 65527-byte Vec exists before insert returns the old owner.
    assert_eq!(
        ingest.push(&start(256, 1, u16::MAX, &[1]), NOW),
        Err(IngestError::InputLimitExceeded)
    );
    assert_failed_and_released(&mut ingest);
}

#[test]
fn exact_aggregate_boundary_is_allowed_but_transient_same_pid_growth_is_not() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    for pid in [256, 257] {
        ingest.push(&tables(pid, true), NOW).expect("video tables");
        grow(&mut ingest, pid, PES_CAP).expect("two exact 8 MiB payloads");
    }
    assert_eq!(retained(&ingest), 16 * 1024 * 1024);
    let previous = ingest
        .pes
        .as_ref()
        .expect("parser")
        .ts_packet_reader()
        .continuity
        .get(&Pid::new(256).expect("PID"))
        .expect("last CC")
        .0;
    assert_eq!(
        ingest.push(&start(256, (previous + 1) & 15, 0, &[1]), NOW),
        Err(IngestError::InputLimitExceeded)
    );
    assert_failed_and_released(&mut ingest);
}

#[test]
fn raw_input_overflow_releases_existing_pes_and_keeps_failure_sticky() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    ingest
        .push(&start(256, 0, u16::MAX, &[1]), NOW)
        .expect("allocated bounded PES");
    ingest
        .push(&[0x47], NOW)
        .expect("one unread partial TS byte");
    assert_eq!(
        ingest.push(&vec![0; MAX_BUFFER], NOW),
        Err(IngestError::InputLimitExceeded)
    );
    assert_failed_and_released(&mut ingest);
}

#[test]
fn continuity_reset_releases_reservations_before_clean_packets_resume() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    ingest
        .push(&start(256, 0, u16::MAX, &[1]), NOW)
        .expect("bounded reservation");
    assert_eq!(retained(&ingest), usize::from(u16::MAX) - 8);
    ingest
        .push(&start(256, 2, 0, &[1]), NOW)
        .expect("existing continuity-loss recovery");
    assert_eq!(retained(&ingest), 1, "old advertised reserve is gone");
    assert_eq!(ingest.dropped_packets(), 1);
}

#[test]
fn successful_bounded_completion_releases_its_reservation() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    ingest
        .push(&start(256, 0, 10, &[0]), NOW)
        .expect("two-byte bounded PES");
    assert_eq!(retained(&ingest), 2);
    let tail = packet(
        256,
        1,
        TsPayload::PesContinuation(TsBytes::new(&[0]).expect("tail byte")),
    );
    ingest
        .push(&tail, NOW)
        .expect("complete bounded placeholder video");
    assert_eq!(retained(&ingest), 0);
    assert_eq!(ingest.dropped_packets(), 0);
}

#[test]
fn malformed_bounded_replacement_keeps_only_the_new_reservation() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    ingest
        .push(&start(256, 0, 100, &[1]), NOW)
        .expect("old reservation");
    assert_eq!(retained(&ingest), 92);
    ingest
        .push(&start(256, 1, 50, &[1]), NOW)
        .expect("existing malformed-PES recovery");
    assert_eq!(
        retained(&ingest),
        42,
        "upstream retains the replacement before rejecting its incomplete predecessor"
    );
    assert_eq!(ingest.dropped_packets(), 1);
    let end = packet(
        256,
        2,
        TsPayload::PesContinuation(TsBytes::new(&[1; 41]).expect("bounded tail")),
    );
    ingest.push(&end, NOW).expect("replacement completes");
    assert_eq!(retained(&ingest), 0);
}

#[test]
fn exact_limit_unbounded_video_eof_remains_unknown_tail_not_definite_loss() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    grow(&mut ingest, 256, PES_CAP).expect("exact limit allowed");
    assert!(
        !ingest
            .finish(NOW)
            .expect("existing unbounded-tail policy")
            .media_loss
    );
    assert_eq!(ingest.dropped_packets(), 0);
    assert!(
        ingest.muxer().is_none(),
        "unknown tail never becomes accepted video"
    );
}

#[test]
fn a_new_connection_resumes_accepted_audio_after_a_resource_failed_parser() {
    let cfg = MuxerConfig::default();
    let mut ingest = TsIngest::new(cfg);
    let fixture = include_bytes!("../tests/fixtures/audio-48k-mono.ts");
    ingest.push(fixture, NOW).expect("generated healthy AAC");
    assert!(ingest.muxer().expect("AAC muxer").has_content());
    assert_eq!(
        ingest.push(&vec![0; MAX_BUFFER + 1], NOW),
        Err(IngestError::InputLimitExceeded)
    );
    assert!(
        ingest
            .finish(NOW)
            .expect("failed accepted prefix seals")
            .media_loss
    );
    let muxer = ingest.take_muxer().expect("accepted presentation survives");
    let before = muxer.next_part_id();
    let mut next = TsIngest::resume(cfg, muxer);
    next.push(fixture, NOW)
        .expect("fresh connection gets fresh parser budget");
    assert!(!next.finish(NOW).expect("healthy fresh EOF").media_loss);
    assert!(next.muxer().expect("continued muxer").next_part_id() > before);
    assert_eq!(next.dropped_packets(), 0);
}

fn special_start(pid: u16, counter: u8, stream_id: u8, declared: u16) -> Vec<u8> {
    // These stream IDs have no optional PES header. Build their exact wire
    // form with adaptation stuffing, leaving one actual payload byte.
    let mut wire = vec![0xff; TS_SIZE];
    wire[..6].copy_from_slice(&[
        0x47,
        0x40 | ((pid >> 8) as u8),
        pid as u8,
        0x30 | counter,
        176,
        0,
    ]);
    wire[181..].copy_from_slice(&[
        0,
        0,
        1,
        stream_id,
        (declared >> 8) as u8,
        declared as u8,
        0xa5,
    ]);
    wire
}

fn next_counter(ingest: &TsIngest, pid: u16) -> u8 {
    let previous = ingest
        .pes
        .as_ref()
        .expect("live reader")
        .ts_packet_reader()
        .continuity
        .get(&Pid::new(pid).expect("test PID"))
        .expect("last payload CC")
        .0;
    (previous + 1) & 15
}

#[test]
fn special_header_rejection_must_not_hide_the_actual_upstream_old_payload() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&tables(256, true), NOW).expect("video tables");
    grow(&mut ingest, 256, PES_CAP).expect("old exact-limit PES");
    let malformed = special_start(256, next_counter(&ingest, 256), 0xbe, 1);
    ingest
        .push(&malformed, NOW)
        .expect("existing malformed-header recovery");
    assert_eq!(
        ingest.dropped_packets(),
        1,
        "actual maintained reader rejects length below its three-byte minimum"
    );
    let reported = retained(&ingest);
    ingest.input.lock().expect("private input").terminal = true;
    let old = ingest
        .pes
        .as_mut()
        .expect("actual maintained PES reader")
        .read_pes_packet()
        .expect("old unbounded PES at EOF")
        .expect("old owner is still present");
    assert_eq!(
        old.data.len(),
        PES_CAP,
        "actual upstream allocation survives the rejected replacement"
    );
    assert_eq!(
        reported,
        old.data.len(),
        "reservation metadata must still account for this old owner"
    );
}

#[test]
fn special_header_rejection_cannot_hide_three_full_pes_allocations() {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    let mut refused = false;
    for pid in [256, 257, 258] {
        ingest.push(&tables(pid, true), NOW).expect("rotated PMT");
        if grow(&mut ingest, pid, PES_CAP).is_err() {
            refused = true;
            break;
        }
        let malformed = special_start(pid, next_counter(&ingest, pid), 0xbf, 2);
        ingest
            .push(&malformed, NOW)
            .expect("existing malformed-header recovery");
    }
    assert!(
        refused,
        "special headers must not hide 24 MiB behind three tiny accounting entries"
    );
    assert_failed_and_released(&mut ingest);
}

#[test]
fn every_special_short_header_preserves_the_previous_reservation() {
    for id in [0xbc, 0xbe, 0xbf, 0xf0, 0xf1, 0xf2, 0xf8, 0xff] {
        for length in [1, 2] {
            let mut ingest = TsIngest::new(MuxerConfig::default());
            ingest.push(&tables(256, true), NOW).expect("video tables");
            ingest
                .push(&start(256, 0, u16::MAX, &[1]), NOW)
                .expect("old reserved bounded PES");
            ingest
                .push(&special_start(256, 1, id, length), NOW)
                .expect("malformed header is discarded");
            assert_eq!(
                retained(&ingest),
                usize::from(u16::MAX) - 8,
                "stream ID {id:x} length {length}: old reservation remains upstream"
            );
            assert_eq!(ingest.dropped_packets(), 1);
        }
    }
}
