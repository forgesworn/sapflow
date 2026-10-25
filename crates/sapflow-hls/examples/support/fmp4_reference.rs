// Frozen encode_part_inner reference from d730c893c4fa138da0f23802963c299ae8524ff8.
// Used only by regression tests and the generated-input benchmark example.
use crate::fmp4::{AUDIO_TRACK, TrackRun};
use bytes::Bytes;
use mp4_atom::{Encode, Mdat, Mfhd, Moof, Tfdt, Tfhd, Traf, Trun, TrunEntry};

const FLAGS_SYNC: u32 = 0x0200_0000;
const FLAGS_NON_SYNC: u32 = 0x0101_0000;

// Frozen pre-optimisation writer: retain its separate owned mdat payload
// and complete box encoder as a byte oracle, independent of the new path.
pub(crate) fn before_direct_mdat(
    sequence_number: u32,
    runs: &[TrackRun<'_>],
    native_recording: bool,
    final_audio_duration: Option<u32>,
) -> Bytes {
    let runs: Vec<&TrackRun<'_>> = runs.iter().filter(|r| !r.samples.is_empty()).collect();
    let build = |offsets: &[i32]| Moof {
        mfhd: Mfhd { sequence_number },
        traf: runs
            .iter()
            .zip(offsets)
            .map(|(run, &offset)| Traf {
                tfhd: Tfhd {
                    track_id: run.track_id,
                    default_base_is_moof: true,
                    ..Default::default()
                },
                tfdt: Some(Tfdt {
                    base_media_decode_time: run.base_time,
                }),
                trun: vec![Trun {
                    data_offset: Some(offset),
                    entries: run
                        .samples
                        .iter()
                        .enumerate()
                        .map(|(index, s)| TrunEntry {
                            duration: Some(
                                if run.track_id == AUDIO_TRACK && index + 1 == run.samples.len() {
                                    final_audio_duration.unwrap_or(s.duration)
                                } else {
                                    s.duration
                                },
                            ),
                            size: Some(s.data.len() as u32),
                            flags: Some(if s.sync { FLAGS_SYNC } else { FLAGS_NON_SYNC }),
                            cts: Some(s.cts),
                        })
                        .collect(),
                }],
                sbgp: if native_recording && run.track_id == AUDIO_TRACK {
                    vec![mp4_atom::Sbgp {
                        grouping_type: b"roll".into(),
                        grouping_type_parameter: None,
                        entries: vec![mp4_atom::SbgpEntry {
                            sample_count: run.samples.len() as u32,
                            group_description_index: 1,
                        }],
                    }]
                } else {
                    Vec::new()
                },
                ..Default::default()
            })
            .collect(),
    };
    let mut probe = Vec::new();
    if build(&vec![0; runs.len()]).encode(&mut probe).is_err() {
        return Bytes::new();
    }
    let header = probe.len() + 8;
    let mut offsets = Vec::with_capacity(runs.len());
    let mut data = Vec::new();
    for run in &runs {
        offsets.push((header + data.len()) as i32);
        for s in run.samples {
            data.extend_from_slice(&s.data);
        }
    }
    let mut buf = Vec::with_capacity(header + data.len());
    if build(&offsets).encode(&mut buf).is_err() || (Mdat { data }).encode(&mut buf).is_err() {
        return Bytes::new();
    }
    Bytes::from(buf)
}
