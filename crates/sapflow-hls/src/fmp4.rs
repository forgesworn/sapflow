//! Fragmented MP4: the init segment and one `moof` + `mdat` per part.
//!
//! Box encoding comes from `mp4-atom`; this module only decides what goes in
//! them. Track 1 is optional H.264 video at 90 kHz; track 2 is optional AAC at its
//! sample rate.

use bytes::Bytes;
use mp4_atom::{
    Audio, Avc1, Avcc, BufMut, Dinf, Dref, Encode, Esds, Ftyp, Hdlr, Mdhd, Mdia, Mfhd, Minf, Moof,
    Moov, Mp4a, Mvex, Mvhd, Smhd, Stbl, Stco, Stsc, Stsd, Stsz, Stts, Tfdt, Tfhd, Tkhd, Traf, Trak,
    Trex, Trun, TrunEntry, Url, Visual, Vmhd, esds,
};

use crate::codec::{AacConfig, H264Config};
use crate::recording_programme::ProgrammeRecordingPresentation;
use crate::recording_timing::{MOVIE_TIMESCALE, RecordingPresentation, TimingError};

/// `track_ID` of the video track.
pub const VIDEO_TRACK: u32 = 1;
/// `track_ID` of the audio track.
pub const AUDIO_TRACK: u32 = 2;
/// Ticks per second on the video track: 90 kHz, the MPEG-TS clock.
pub const VIDEO_TIMESCALE: u32 = 90_000;
/// Largest generated fragment accepted by [`rebase_part_for_file`]. Recording
/// admission can apply this same limit before retaining a fragment for writing.
pub const MAX_FILE_FRAGMENT_BYTES: usize = 64 * 1024 * 1024;

/// Sample flags (ISO/IEC 14496-12 8.8.3.1): a sync sample depends on
/// nothing; any other sample depends on others and is not a sync sample.
const FLAGS_SYNC: u32 = 0x0200_0000;
const FLAGS_NON_SYNC: u32 = 0x0101_0000;

/// One encoded frame, ready to place in a part.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Duration in the track's timescale.
    pub duration: u32,
    /// Composition offset (PTS - DTS) in the track's timescale.
    pub cts: i32,
    /// Whether this is a sync sample (a keyframe).
    pub sync: bool,
    /// The coded sample: length-prefixed NAL units for video, one raw AAC frame
    /// for audio.
    pub data: Bytes,
}

/// A track's samples within one part.
#[derive(Debug, Clone)]
pub struct TrackRun<'a> {
    /// The `track_ID` the samples belong to ([`VIDEO_TRACK`] or
    /// [`AUDIO_TRACK`]).
    pub track_id: u32,
    /// Decode time of the first sample, in the track's timescale.
    pub base_time: u64,
    /// The samples, in decode order.
    pub samples: &'a [Sample],
}

/// Builds `ftyp` + `moov` for the given tracks.
pub fn init_segment(video: &H264Config, audio: Option<&AacConfig>) -> Bytes {
    init(Some(video), audio, None)
}

/// The init segment for a recording file, carrying its total duration
/// (`mvhd` and `mehd`) so a browser shows the right length. The encoding is
/// the same size whatever the duration, so a recorder can write it with
/// zero first and overwrite it in place when the show ends.
pub fn init_segment_for_file(
    video: &H264Config,
    audio: Option<&AacConfig>,
    duration: std::time::Duration,
) -> Bytes {
    init(Some(video), audio, Some(duration.as_millis() as u64))
}

/// Recording init for video, audio or both. An empty track set is refused.
/// Initial and final duration headers have equal size and can be rewritten in place.
pub fn init_segment_for_tracks(
    video: Option<&H264Config>,
    audio: Option<&AacConfig>,
    duration: std::time::Duration,
) -> Option<Bytes> {
    if video.is_none() && audio.is_none() {
        return None;
    }
    let duration_ms = u64::try_from(duration.as_millis()).ok()?;
    let bytes = init(video, audio, Some(duration_ms));
    (!bytes.is_empty()).then_some(bytes)
}

/// Explicit native-AAC recording header. Streaming and legacy defaults are unchanged.
/// The layout has fixed-width duration/edit sections from its first output.
pub fn init_segment_for_native_recording(
    video: Option<&H264Config>,
    audio: &AacConfig,
    presentation: &RecordingPresentation,
) -> Result<Bytes, TimingError> {
    presentation.validate()?;
    if audio.audio_specific_config() != [0x11, 0x88]
        || video.is_some() != presentation.video_start.is_some()
    {
        return Err(TimingError::Unsupported);
    }
    native_recording_init(
        video,
        audio,
        NativeInitPresentation {
            movie_duration: presentation.movie_duration()?,
            audio: NativeAudioPresentation {
                delay: presentation
                    .audio_start
                    .checked_mul(15)
                    .ok_or(TimingError::Overflow)?,
                media_time: presentation
                    .audio_start
                    .checked_add(u64::from(presentation.leading_frames))
                    .ok_or(TimingError::Overflow)?,
                duration: presentation
                    .valid_audio_frames()
                    .checked_mul(15)
                    .ok_or(TimingError::Overflow)?,
            },
            video: presentation.video_start.zip(presentation.video_end),
        },
    )
}

/// Audio-only programme header with independent original media and movie origins.
/// Checked scalar window geometry is not native EOF, file or source authority.
/// Original fragment timestamps/payloads are never rewritten by this function.
/// Legacy native-recording headers retain their existing public semantics.
pub fn init_segment_for_programme_recording(
    audio: &AacConfig,
    presentation: &ProgrammeRecordingPresentation,
) -> Result<Bytes, TimingError> {
    if audio.audio_specific_config() != [0x11, 0x88] {
        return Err(TimingError::Unsupported);
    }
    native_recording_init(
        None,
        audio,
        NativeInitPresentation {
            movie_duration: presentation.movie_extent()?.duration_ticks,
            audio: NativeAudioPresentation {
                delay: presentation.movie_delay_ticks(),
                media_time: presentation.media_time(),
                duration: presentation
                    .presented_frames()
                    .checked_mul(15)
                    .ok_or(TimingError::Overflow)?,
            },
            video: None,
        },
    )
}

struct NativeAudioPresentation {
    delay: u64,
    media_time: u64,
    duration: u64,
}

struct NativeInitPresentation {
    movie_duration: u64,
    audio: NativeAudioPresentation,
    video: Option<(u64, u64)>,
}

fn native_recording_init(
    video: Option<&H264Config>,
    audio: &AacConfig,
    presentation: NativeInitPresentation,
) -> Result<Bytes, TimingError> {
    let encode = |result: mp4_atom::Result<()>| result.map_err(|_| TimingError::Unsupported);
    let mut output = Vec::new();
    encode(
        Ftyp {
            major_brand: b"iso5".into(),
            minor_version: 1,
            compatible_brands: vec![b"iso5".into(), b"iso6".into(), b"mp41".into()],
        }
        .encode(&mut output),
    )?;
    let mut movie = Vec::new();
    let duration = presentation.movie_duration;
    encode(
        Mvhd {
            timescale: MOVIE_TIMESCALE,
            duration,
            rate: 1.into(),
            volume: 1.into(),
            next_track_id: 3,
            ..Default::default()
        }
        .encode(&mut movie),
    )?;
    let mut trex = Vec::with_capacity(2);
    if let Some(config) = video {
        let mut track = video_trak(config);
        let (first, end) = presentation.video.ok_or(TimingError::Inconsistent)?;
        track.tkhd.duration = end.checked_mul(8).ok_or(TimingError::Overflow)?;
        track.mdia.mdhd.duration = 0; // Empty fragmented init: media duration comes from truns.
        native_track(
            &track,
            first.checked_mul(8).ok_or(TimingError::Overflow)?,
            first,
            end.checked_sub(first)
                .ok_or(TimingError::Inconsistent)?
                .checked_mul(8)
                .ok_or(TimingError::Overflow)?,
            &mut movie,
        )?;
        trex.push(Trex {
            track_id: VIDEO_TRACK,
            default_sample_description_index: 1,
            ..Default::default()
        });
    }
    let mut track = audio_trak(audio);
    let delay = presentation.audio.delay;
    let audio_duration = presentation.audio.duration;
    track.tkhd.duration = delay
        .checked_add(audio_duration)
        .ok_or(TimingError::Overflow)?;
    track.mdia.mdhd.duration = 0; // Do not add the fragment coded extent twice in Apple readers.
    track.mdia.minf.stbl.sgpd = vec![mp4_atom::Sgpd {
        grouping_type: b"roll".into(),
        default_length: Some(2),
        default_group_description_index: None,
        static_group_description: false,
        static_mapping: false,
        essential: false,
        entries: vec![mp4_atom::SgpdEntry {
            description_length: Some(2),
            entry: mp4_atom::AnySampleGroupEntry::UnknownGroupingType(
                b"roll".into(),
                vec![255, 255],
            ),
        }],
    }];
    native_track(
        &track,
        delay,
        presentation.audio.media_time,
        audio_duration,
        &mut movie,
    )?;
    trex.push(Trex {
        track_id: AUDIO_TRACK,
        default_sample_description_index: 1,
        ..Default::default()
    });
    encode(
        Mvex {
            trex,
            mehd: Some(mp4_atom::Mehd {
                fragment_duration: duration,
            }),
        }
        .encode(&mut movie),
    )?;
    native_box(b"moov", &movie, &mut output)?;
    Ok(Bytes::from(output))
}

// Only serialises metadata we construct, not a parser or another media muxer.
// mp4-atom chooses elst v0 for small values; explicit v1 avoids later layout growth.
fn native_track(
    track: &Trak,
    delay: u64,
    media_time: u64,
    duration: u64,
    output: &mut Vec<u8>,
) -> Result<(), TimingError> {
    if media_time > i64::MAX as u64 {
        return Err(TimingError::Overflow);
    }
    let mut body = Vec::new();
    track
        .tkhd
        .encode(&mut body)
        .map_err(|_| TimingError::Unsupported)?;
    track
        .mdia
        .encode(&mut body)
        .map_err(|_| TimingError::Unsupported)?;
    let mut edits = vec![1, 0, 0, 0];
    edits.extend_from_slice(&(if delay == 0 { 1_u32 } else { 2 }).to_be_bytes());
    if delay > 0 {
        edits.extend_from_slice(&delay.to_be_bytes());
        edits.extend_from_slice(&(-1_i64).to_be_bytes());
        edits.extend_from_slice(&65536_u32.to_be_bytes());
    }
    edits.extend_from_slice(&duration.to_be_bytes());
    edits.extend_from_slice(&media_time.to_be_bytes());
    edits.extend_from_slice(&65536_u32.to_be_bytes());
    let mut edts = Vec::new();
    native_box(b"elst", &edits, &mut edts)?;
    native_box(b"edts", &edts, &mut body)?;
    native_box(b"trak", &body, output)
}

fn native_box(kind: &[u8; 4], body: &[u8], output: &mut Vec<u8>) -> Result<(), TimingError> {
    let length = u32::try_from(body.len().checked_add(8).ok_or(TimingError::Overflow)?)
        .map_err(|_| TimingError::Overflow)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(body);
    Ok(())
}

fn init(video: Option<&H264Config>, audio: Option<&AacConfig>, duration_ms: Option<u64>) -> Bytes {
    Bytes::from(init_buffer(video, audio, duration_ms))
}

pub(crate) fn init_with_quota(
    video: Option<&H264Config>,
    audio: Option<&AacConfig>,
    duration_ms: Option<u64>,
    quota: Option<&std::sync::Arc<sapflow_core::fragment::FragmentQuota>>,
) -> Result<Bytes, sapflow_core::fragment::FragmentLimit> {
    crate::fragment::retain(init_buffer(video, audio, duration_ms), quota)
}

fn init_buffer(
    video: Option<&H264Config>,
    audio: Option<&AacConfig>,
    duration_ms: Option<u64>,
) -> Vec<u8> {
    let ftyp = Ftyp {
        major_brand: b"iso5".into(),
        minor_version: 1,
        compatible_brands: vec![b"iso5".into(), b"iso6".into(), b"mp41".into()],
    };

    let mut trak = Vec::with_capacity(2);
    let mut trex = Vec::with_capacity(2);
    if let Some(video) = video {
        trak.push(video_trak(video));
        trex.push(Trex {
            track_id: VIDEO_TRACK,
            default_sample_description_index: 1,
            ..Default::default()
        });
    }
    if let Some(a) = audio {
        trak.push(audio_trak(a));
        trex.push(Trex {
            track_id: AUDIO_TRACK,
            default_sample_description_index: 1,
            ..Default::default()
        });
    }

    let moov = Moov {
        mvhd: Mvhd {
            timescale: 1000,
            rate: 1.into(),
            volume: 1.into(),
            next_track_id: if audio.is_some() { 3 } else { 2 },
            duration: duration_ms.unwrap_or(0),
            ..Default::default()
        },
        mvex: Some(Mvex {
            trex,
            mehd: duration_ms.map(|d| mp4_atom::Mehd {
                fragment_duration: d,
            }),
        }),
        trak,
        ..Default::default()
    };

    let mut buf = Vec::new();
    // Encoding into a Vec cannot run out of space; the only failures are
    // values that do not fit their fields, which our own configs never hold.
    if ftyp.encode(&mut buf).is_err() || moov.encode(&mut buf).is_err() {
        return Vec::new();
    }
    buf
}

fn empty_stbl(stsd: Stsd) -> Stbl {
    Stbl {
        stsd,
        stts: Stts::default(),
        stsc: Stsc::default(),
        stsz: Stsz::default(),
        stco: Some(Stco::default()),
        ..Default::default()
    }
}

fn dinf() -> Dinf {
    Dinf {
        dref: Dref {
            urls: vec![Url {
                location: String::new(),
            }],
        },
    }
}

fn video_trak(v: &H264Config) -> Trak {
    let avc1 = Avc1 {
        visual: Visual {
            data_reference_index: 1,
            width: v.width,
            height: v.height,
            ..Default::default()
        },
        avcc: Avcc {
            configuration_version: 1,
            avc_profile_indication: v.profile(),
            profile_compatibility: v.compatibility(),
            avc_level_indication: v.level(),
            length_size: 4,
            sequence_parameter_sets: vec![v.sps.clone()],
            picture_parameter_sets: vec![v.pps.clone()],
            ext: None,
        },
        btrt: None,
        colr: None,
        pasp: None,
        taic: None,
        fiel: None,
    };
    Trak {
        tkhd: Tkhd {
            track_id: VIDEO_TRACK,
            enabled: true,
            in_movie: true,
            width: v.width.into(),
            height: v.height.into(),
            ..Default::default()
        },
        mdia: Mdia {
            mdhd: Mdhd {
                timescale: VIDEO_TIMESCALE,
                language: "und".into(),
                ..Default::default()
            },
            hdlr: Hdlr {
                handler: b"vide".into(),
                name: "VideoHandler".into(),
            },
            minf: Minf {
                vmhd: Some(Vmhd::default()),
                dinf: dinf(),
                stbl: empty_stbl(Stsd {
                    codecs: vec![avc1.into()],
                }),
                ..Default::default()
            },
        },
        ..Default::default()
    }
}

fn audio_trak(a: &AacConfig) -> Trak {
    let mp4a = Mp4a {
        audio: Audio {
            data_reference_index: 1,
            channel_count: u16::from(a.channels),
            sample_size: 16,
            sample_rate: (a.sample_rate.min(u32::from(u16::MAX)) as u16).into(),
        },
        esds: Esds {
            es_desc: esds::EsDescriptor {
                es_id: AUDIO_TRACK as u16,
                dec_config: esds::DecoderConfig {
                    object_type_indication: 0x40,
                    stream_type: 5,
                    dec_specific: Some(esds::DecoderSpecific {
                        profile: a.object_type,
                        freq_index: a.sample_rate_index,
                        chan_conf: a.channels,
                        raw: a.audio_specific_config().to_vec(),
                    }),
                    ..Default::default()
                },
                sl_config: esds::SLConfig {},
            },
        },
        btrt: None,
        taic: None,
    };
    Trak {
        tkhd: Tkhd {
            track_id: AUDIO_TRACK,
            alternate_group: 1,
            enabled: true,
            in_movie: true,
            volume: 1.into(),
            ..Default::default()
        },
        mdia: Mdia {
            mdhd: Mdhd {
                timescale: a.sample_rate,
                language: "und".into(),
                ..Default::default()
            },
            hdlr: Hdlr {
                handler: b"soun".into(),
                name: "SoundHandler".into(),
            },
            minf: Minf {
                smhd: Some(Smhd::default()),
                dinf: dinf(),
                stbl: empty_stbl(Stsd {
                    codecs: vec![mp4a.into()],
                }),
                ..Default::default()
            },
        },
        ..Default::default()
    }
}

/// Builds one `moof` + `mdat` holding every run, in order.
/// Returns empty bytes if the fragment cannot use 32-bit box sizes and signed
/// 32-bit track data offsets. Payloads are copied once into the final fragment.
pub fn encode_part(sequence_number: u32, runs: &[TrackRun<'_>]) -> Bytes {
    encode_part_inner(sequence_number, runs, false, None)
}

/// Same original media runs, adding only recording roll-group references.
/// The native and independent decoder proof for this grouping is a separate gate.
pub fn encode_native_recording_part(sequence_number: u32, runs: &[TrackRun<'_>]) -> Bytes {
    encode_part_inner(sequence_number, runs, true, None)
}

/// Native COMPLETE EOF only: shorten the final container entry, never the shared sample.
/// Unknown EOF and every interior generation retain their original 1024-frame cadence.
pub fn encode_native_recording_part_with_final_audio_duration(
    sequence_number: u32,
    runs: &[TrackRun<'_>],
    final_audio_duration: u32,
) -> Bytes {
    let mut audio = runs.iter().filter(|r| r.track_id == AUDIO_TRACK);
    let Some(track) = audio.next() else {
        return Bytes::new();
    };
    if !(1..=1024).contains(&final_audio_duration)
        || audio.next().is_some()
        || track.samples.last().is_none_or(|s| s.duration != 1024)
    {
        return Bytes::new();
    }
    encode_part_inner(sequence_number, runs, true, Some(final_audio_duration))
}

fn encode_part_inner(
    sequence_number: u32,
    runs: &[TrackRun<'_>],
    native_recording: bool,
    final_audio_duration: Option<u32>,
) -> Bytes {
    Bytes::from(encode_part_buffer(
        sequence_number,
        runs,
        native_recording,
        final_audio_duration,
    ))
}

pub(crate) fn encode_part_with_quota(
    sequence_number: u32,
    runs: &[TrackRun<'_>],
    quota: Option<&std::sync::Arc<sapflow_core::fragment::FragmentQuota>>,
) -> Result<Bytes, sapflow_core::fragment::FragmentLimit> {
    crate::fragment::retain(
        encode_part_buffer(sequence_number, runs, false, None),
        quota,
    )
}

fn encode_part_buffer(
    sequence_number: u32,
    runs: &[TrackRun<'_>],
    native_recording: bool,
    final_audio_duration: Option<u32>,
) -> Vec<u8> {
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

    // The data offsets depend on the moof's own size, which does not depend
    // on their values: encode once to measure, then again with real offsets.
    let mut probe = Vec::new();
    if build(&vec![0; runs.len()]).encode(&mut probe).is_err() {
        return Vec::new();
    }
    let lengths = runs.iter().map(|run| {
        run.samples.iter().try_fold(0usize, |length, sample| {
            length.checked_add(sample.data.len())
        })
    });
    let Some((offsets, capacity)) = checked_part_layout(probe.len(), lengths) else {
        return Vec::new();
    };
    let mut buf = Vec::with_capacity(capacity);
    if build(&offsets).encode(&mut buf).is_err() {
        return Vec::new();
    }
    // Write shared sample payloads directly into the final fragment. Keep the
    // same mp4-atom box framing without a temporary payload Vec, its growth
    // reallocations, or a second copy of every compressed sample.
    if buf
        .encode_atom(b"mdat", |output| {
            for run in &runs {
                for sample in run.samples {
                    output.extend_from_slice(&sample.data);
                }
            }
            Ok(())
        })
        .is_err()
    {
        return Vec::new();
    }
    buf
}

// mp4-atom's Atom encoder supports 32-bit sizes, not extended mdat boxes.
// Validate representability before allocating the final payload-sized buffer.
fn checked_part_layout(
    moof_len: usize,
    lengths: impl Iterator<Item = Option<usize>>,
) -> Option<(Vec<i32>, usize)> {
    let header = moof_len.checked_add(8)?;
    let mut offsets = Vec::with_capacity(lengths.size_hint().0);
    let mut data_len = 0usize;
    for length in lengths {
        offsets.push(i32::try_from(header.checked_add(data_len)?).ok()?);
        data_len = data_len.checked_add(length?)?;
    }
    u32::try_from(data_len.checked_add(8)?).ok()?;
    Some((offsets, header.checked_add(data_len)?))
}

/// Shared rational origin for a standalone recording. Each track subtracts
/// this same time, rounded down in its own timescale; A/V offsets survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTimeBase {
    ticks: u64,
    timescale: u32,
}

/// [`rebase_part_for_file`] refused its input: it is not one of our own `moof`
/// + `mdat` fragments, or it is too large.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidFileFragment;

/// Rebase only our generated moof+mdat on the recording worker. One bounded
/// output copy; the immutable HLS bytes remain untouched. Supports tfdt v0/v1.
/// This deliberately accepts our fixed box structure, not arbitrary MP4 files.
pub fn rebase_part_for_file(
    data: &Bytes,
    audio_rate: Option<u32>,
    origin: &mut Option<FileTimeBase>,
) -> Result<Bytes, InvalidFileFragment> {
    if data.len() > MAX_FILE_FRAGMENT_BYTES {
        return Err(InvalidFileFragment);
    }
    fn u32_at(data: &[u8], at: usize) -> Result<u32, InvalidFileFragment> {
        Ok(u32::from_be_bytes(
            data.get(at..at + 4)
                .ok_or(InvalidFileFragment)?
                .try_into()
                .map_err(|_| InvalidFileFragment)?,
        ))
    }
    fn box_at(data: &[u8], at: usize, limit: usize) -> Result<(usize, &[u8]), InvalidFileFragment> {
        let size = u32_at(data, at)? as usize;
        let end = at.checked_add(size).ok_or(InvalidFileFragment)?;
        if size < 8 || end > limit {
            return Err(InvalidFileFragment);
        }
        Ok((end, data.get(at + 4..at + 8).ok_or(InvalidFileFragment)?))
    }
    let (moof_end, kind) = box_at(data, 0, data.len())?;
    if kind != b"moof" || moof_end > 1024 * 1024 {
        return Err(InvalidFileFragment);
    }
    let (end, kind) = box_at(data, moof_end, data.len())?;
    if kind != b"mdat" || end != data.len() {
        return Err(InvalidFileFragment);
    }
    let mut fields = Vec::with_capacity(2);
    let mut at = 8;
    let mut seen = 0u8;
    let mut boxes = 0;
    while at < moof_end {
        boxes += 1;
        if boxes > 4 {
            return Err(InvalidFileFragment);
        }
        let (end, kind) = box_at(data, at, moof_end)?;
        if kind == b"traf" {
            let mut child = at + 8;
            let mut track = None;
            let mut field = None;
            let mut children = 0;
            while child < end {
                children += 1;
                if children > 4 {
                    return Err(InvalidFileFragment);
                }
                let (next, kind) = box_at(data, child, end)?;
                match kind {
                    b"tfhd" => {
                        if track.is_some()
                            || next - child != 16
                            || u32_at(data, child + 8)? != 0x020000
                        {
                            return Err(InvalidFileFragment);
                        }
                        track = Some(u32_at(data, child + 12)?);
                    }
                    b"tfdt" => {
                        if field.is_some() {
                            return Err(InvalidFileFragment);
                        }
                        let version = u32_at(data, child + 8)?;
                        let ticks = match (version, next - child) {
                            (0, 16) => u64::from(u32_at(data, child + 12)?),
                            (0x01000000, 20) => u64::from_be_bytes(
                                data[child + 12..next]
                                    .try_into()
                                    .map_err(|_| InvalidFileFragment)?,
                            ),
                            _ => return Err(InvalidFileFragment),
                        };
                        field = Some((child + 12, version != 0, ticks));
                    }
                    b"trun" => {}
                    _ => return Err(InvalidFileFragment),
                }
                child = next;
            }
            let track = track.ok_or(InvalidFileFragment)?;
            let rate = match track {
                VIDEO_TRACK => VIDEO_TIMESCALE,
                AUDIO_TRACK => audio_rate
                    .filter(|rate| *rate > 0)
                    .ok_or(InvalidFileFragment)?,
                _ => return Err(InvalidFileFragment),
            };
            let bit = 1u8 << track;
            if seen & bit != 0 {
                return Err(InvalidFileFragment);
            }
            seen |= bit;
            let (offset, wide, ticks) = field.ok_or(InvalidFileFragment)?;
            fields.push((offset, wide, ticks, rate));
        } else if kind != b"mfhd" {
            return Err(InvalidFileFragment);
        }
        at = end;
    }
    if fields.is_empty() {
        return Err(InvalidFileFragment);
    }
    let base = match *origin {
        Some(base) => base,
        None => {
            let (_, _, ticks, rate) = fields
                .iter()
                .min_by(|a, b| {
                    (u128::from(a.2) * u128::from(b.3)).cmp(&(u128::from(b.2) * u128::from(a.3)))
                })
                .ok_or(InvalidFileFragment)?;
            FileTimeBase {
                ticks: *ticks,
                timescale: *rate,
            }
        }
    };
    let mut output = data.to_vec();
    for (offset, wide, ticks, rate) in fields {
        let shift =
            u64::try_from(u128::from(base.ticks) * u128::from(rate) / u128::from(base.timescale))
                .map_err(|_| InvalidFileFragment)?;
        let value = ticks.checked_sub(shift).ok_or(InvalidFileFragment)?;
        if wide {
            output[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
        } else {
            output[offset..offset + 4].copy_from_slice(
                &u32::try_from(value)
                    .map_err(|_| InvalidFileFragment)?
                    .to_be_bytes(),
            );
        }
    }
    *origin = Some(base);
    Ok(Bytes::from(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp4_atom::{Decode, Moof as M};

    fn video() -> H264Config {
        H264Config::new(
            &[
                0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03,
                0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
            ],
            &[0x68, 0xef, 0xbc, 0x80],
        )
        .expect("valid sps")
    }

    #[test]
    fn bounded_encodings_charge_actual_vec_capacity_including_headers() {
        use sapflow_core::fragment::FragmentQuota;
        use std::sync::Arc;
        let config = video();
        let init_buffer = super::init_buffer(Some(&config), None, None);
        let capacity = init_buffer.capacity();
        let quota = Arc::new(FragmentQuota::new(capacity));
        let init = super::init_with_quota(Some(&config), None, None, Some(&quota))
            .expect("exact encoded init capacity");
        assert_eq!(init.as_ref(), init_buffer.as_slice());
        assert_eq!(quota.used(), capacity);
        assert!(capacity >= init.len());
        drop(init);
        assert_eq!(quota.used(), 0);

        let samples = [Sample {
            duration: 3000,
            cts: 0,
            sync: true,
            data: Bytes::from_static(b"generated sample"),
        }];
        let runs = [TrackRun {
            track_id: VIDEO_TRACK,
            base_time: 0,
            samples: &samples,
        }];
        let buffer = super::encode_part_buffer(1, &runs, false, None);
        let capacity = buffer.capacity();
        assert!(capacity >= buffer.len() && buffer.len() > samples[0].data.len());
        let quota = Arc::new(FragmentQuota::new(capacity));
        let fragment = super::encode_part_with_quota(1, &runs, Some(&quota))
            .expect("exact full encoded capacity");
        assert_eq!(fragment.as_ref(), buffer.as_slice());
        assert_eq!(
            quota.used(),
            capacity,
            "moof/mdat headers and all spare capacity are charged"
        );
        assert!(quota.try_acquire(1).is_err());
        let suffix = fragment.slice(8..);
        drop(fragment);
        assert_eq!(quota.used(), capacity);
        drop(suffix);
        assert_eq!(quota.used(), 0);
    }

    #[test]
    fn init_segment_round_trips() {
        let aac = AacConfig {
            object_type: 2,
            sample_rate_index: 3,
            sample_rate: 48000,
            channels: 2,
        };
        let init = init_segment(&video(), Some(&aac));
        let mut cur = std::io::Cursor::new(&init[..]);
        let ftyp = Ftyp::decode(&mut cur).expect("ftyp");
        assert_eq!(ftyp.major_brand, b"iso5".into());
        let moov = Moov::decode(&mut cur).expect("moov");
        assert_eq!(moov.trak.len(), 2);
        assert_eq!(moov.trak[0].mdia.mdhd.timescale, 90_000);
        assert_eq!(moov.trak[1].mdia.mdhd.timescale, 48_000);
        assert_eq!(moov.mvex.expect("mvex").trex.len(), 2);
    }

    #[test]
    fn file_init_is_the_same_size_whatever_the_duration() {
        use std::time::Duration;
        let a = init_segment_for_file(&video(), None, Duration::ZERO);
        let b = init_segment_for_file(&video(), None, Duration::from_secs(3 * 3600));
        assert_eq!(a.len(), b.len());
        let mut cur = std::io::Cursor::new(&b[..]);
        Ftyp::decode(&mut cur).expect("ftyp");
        let moov = Moov::decode(&mut cur).expect("moov");
        assert_eq!(moov.mvhd.duration, 3 * 3600 * 1000);
        assert_eq!(
            moov.mvex.and_then(|m| m.mehd).map(|m| m.fragment_duration),
            Some(3 * 3600 * 1000)
        );
    }

    #[test]
    fn part_offsets_point_at_each_tracks_data() {
        let v = [
            Sample {
                duration: 3000,
                cts: 0,
                sync: true,
                data: Bytes::from_static(b"VVVV"),
            },
            Sample {
                duration: 3000,
                cts: 3000,
                sync: false,
                data: Bytes::from_static(b"vv"),
            },
        ];
        let a = [Sample {
            duration: 1024,
            cts: 0,
            sync: true,
            data: Bytes::from_static(b"AAA"),
        }];
        let part = encode_part(
            7,
            &[
                TrackRun {
                    track_id: VIDEO_TRACK,
                    base_time: 900_000,
                    samples: &v,
                },
                TrackRun {
                    track_id: AUDIO_TRACK,
                    base_time: 480_000,
                    samples: &a,
                },
            ],
        );
        let mut cur = std::io::Cursor::new(&part[..]);
        let moof = M::decode(&mut cur).expect("moof");
        assert_eq!(moof.mfhd.sequence_number, 7);
        let vo = moof.traf[0].trun[0].data_offset.expect("offset") as usize;
        let ao = moof.traf[1].trun[0].data_offset.expect("offset") as usize;
        assert_eq!(&part[vo..vo + 6], b"VVVVvv");
        assert_eq!(&part[ao..ao + 3], b"AAA");
        assert_eq!(
            moof.traf[0].tfdt.as_ref().map(|t| t.base_media_decode_time),
            Some(900_000)
        );
    }
    #[test]
    fn audio_only_file_init_declares_only_audio_and_keeps_rewrite_size() {
        use std::time::Duration;
        let aac = AacConfig {
            object_type: 2,
            sample_rate_index: 3,
            sample_rate: 48_000,
            channels: 1,
        };
        let initial =
            init_segment_for_tracks(None, Some(&aac), Duration::ZERO).expect("audio init");
        let final_init = init_segment_for_tracks(None, Some(&aac), Duration::from_millis(12_345))
            .expect("audio final init");
        assert_eq!(initial.len(), final_init.len());
        let mut cur = std::io::Cursor::new(&final_init[..]);
        Ftyp::decode(&mut cur).expect("ftyp");
        let moov = Moov::decode(&mut cur).expect("moov");
        assert_eq!(moov.trak.len(), 1);
        assert_eq!(moov.trak[0].tkhd.track_id, AUDIO_TRACK);
        assert_eq!(moov.trak[0].mdia.mdhd.timescale, 48_000);
        assert_eq!(moov.mvhd.duration, 12_345);
        assert_eq!(moov.mvhd.next_track_id, 3);
        assert_eq!(moov.mvex.expect("mvex").trex[0].track_id, AUDIO_TRACK);
        assert!(init_segment_for_tracks(None, None, Duration::ZERO).is_none());
    }
}

#[cfg(test)]
#[path = "../examples/support/fmp4_reference.rs"]
mod fmp4_reference;

#[cfg(test)]
mod direct_mdat_tests {
    use super::*;
    use mp4_atom::{Decode, Mdat};

    use super::fmp4_reference::before_direct_mdat;

    fn assert_layout(
        bytes: &Bytes,
        runs: &[TrackRun<'_>],
        native_recording: bool,
        final_audio_duration: Option<u32>,
    ) {
        let mut input = bytes.as_ref();
        let moof = Moof::decode(&mut input).expect("moof");
        let moof_len = bytes.len() - input.len();
        let mdat = Mdat::decode(&mut input).expect("mdat");
        assert!(input.is_empty(), "exactly one moof and mdat");
        let nonempty: Vec<_> = runs.iter().filter(|r| !r.samples.is_empty()).collect();
        assert_eq!(moof.traf.len(), nonempty.len());
        let mut offset = moof_len + 8;
        let mut expected_payload = Vec::new();
        for (traf, run) in moof.traf.iter().zip(nonempty) {
            assert_eq!(traf.tfhd.track_id, run.track_id);
            assert_eq!(
                traf.tfdt
                    .as_ref()
                    .expect("decode time")
                    .base_media_decode_time,
                run.base_time
            );
            assert_eq!(traf.trun.len(), 1);
            assert_eq!(traf.trun[0].data_offset, Some(offset as i32));
            assert_eq!(traf.trun[0].entries.len(), run.samples.len());
            for (index, (entry, sample)) in traf.trun[0].entries.iter().zip(run.samples).enumerate()
            {
                let duration = if run.track_id == AUDIO_TRACK && index + 1 == run.samples.len() {
                    final_audio_duration.unwrap_or(sample.duration)
                } else {
                    sample.duration
                };
                assert_eq!(entry.duration, Some(duration));
                assert_eq!(entry.cts, Some(sample.cts));
                assert_eq!(entry.size, Some(sample.data.len() as u32));
                assert_eq!(&bytes[offset..offset + sample.data.len()], sample.data);
                expected_payload.extend_from_slice(&sample.data);
                offset += sample.data.len();
            }
            assert_eq!(
                traf.sbgp.len(),
                usize::from(native_recording && run.track_id == AUDIO_TRACK)
            );
        }
        assert_eq!(offset, bytes.len());
        assert_eq!(mdat.data, expected_payload);
    }

    #[test]
    fn fragment_layout_checks_box_offset_and_machine_integer_boundaries() {
        let layout =
            |moof, lengths: &[Option<usize>]| checked_part_layout(moof, lengths.iter().copied());
        let header = 32;
        let offset_limit = i32::MAX as usize - header;
        assert_eq!(
            layout(24, &[Some(offset_limit), Some(0)]),
            Some((vec![header as i32, i32::MAX], i32::MAX as usize))
        );
        assert!(layout(24, &[Some(offset_limit + 1), Some(0)]).is_none());
        let mdat_limit = u32::MAX as usize - 8;
        if usize::BITS > 32 {
            assert_eq!(
                layout(24, &[Some(mdat_limit)]),
                Some((vec![header as i32], header + mdat_limit)),
                "one run can extend beyond signed offsets if its start fits"
            );
        }
        assert!(layout(24, &[Some(mdat_limit + 1)]).is_none());
        assert!(layout(usize::MAX, &[]).is_none());
        assert!(layout(24, &[Some(usize::MAX)]).is_none());
        let overflowing_run = [usize::MAX, 1]
            .into_iter()
            .try_fold(0usize, usize::checked_add);
        assert!(layout(24, &[overflowing_run]).is_none());
    }

    #[test]
    fn unrepresentable_fragments_are_refused_without_allocating_their_payload() {
        // One actual MiB shared by every sample describes a 4 GiB payload.
        // Any attempted fragment allocation would make this test impractical.
        let sample = Sample {
            duration: 3000,
            cts: 0,
            sync: true,
            data: Bytes::from(vec![0x67; 1024 * 1024]),
        };
        let samples = vec![sample.clone(); 4096];
        let oversized_mdat = [TrackRun {
            track_id: VIDEO_TRACK,
            base_time: 0,
            samples: &samples,
        }];
        assert!(encode_part(1, &oversized_mdat).is_empty());
        assert!(encode_native_recording_part(1, &oversized_mdat).is_empty());
        let audio = [Sample {
            duration: 1024,
            ..sample
        }];
        let oversized_offset = [
            TrackRun {
                samples: &samples[..2048],
                ..oversized_mdat[0]
            },
            TrackRun {
                track_id: AUDIO_TRACK,
                base_time: 0,
                samples: &audio,
            },
        ];
        assert!(encode_part(1, &oversized_offset).is_empty());
        assert!(encode_native_recording_part(1, &oversized_offset).is_empty());
        assert!(
            encode_native_recording_part_with_final_audio_duration(1, &oversized_offset, 576)
                .is_empty()
        );
    }

    #[test]
    fn direct_mdat_matches_frozen_writer_for_streaming_and_native_layouts() {
        let video: Vec<_> = [0, 1, 17, 4096, 131_071, 13]
            .into_iter()
            .enumerate()
            .map(|(i, len)| Sample {
                duration: 3000 + i as u32,
                cts: [0, 3000, -3000][i % 3],
                sync: i % 2 == 0,
                data: Bytes::from(vec![i as u8 + 1; len]),
            })
            .collect();
        let audio: Vec<_> = [0, 1, 193, 4097]
            .into_iter()
            .enumerate()
            .map(|(i, len)| Sample {
                duration: 1024,
                cts: 0,
                sync: true,
                data: Bytes::from(vec![i as u8 + 17; len]),
            })
            .collect();
        let v = TrackRun {
            track_id: VIDEO_TRACK,
            base_time: u64::from(u32::MAX) + 90_000,
            samples: &video,
        };
        let a = TrackRun {
            track_id: AUDIO_TRACK,
            base_time: 480_000,
            samples: &audio,
        };
        let empty_v = TrackRun { samples: &[], ..v };
        let empty_a = TrackRun { samples: &[], ..a };
        let layouts = [
            vec![],
            vec![empty_v.clone(), empty_a.clone()],
            vec![v.clone()],
            vec![a.clone()],
            vec![v.clone(), a.clone()],
            vec![a.clone(), v.clone()],
            vec![empty_a.clone(), v.clone(), empty_v.clone()],
            vec![empty_v, a.clone(), empty_a],
            vec![
                TrackRun {
                    samples: &video[..1],
                    ..v
                },
                TrackRun {
                    samples: &audio[..1],
                    ..a
                },
            ],
        ];
        for runs in &layouts {
            for sequence in [0, 7, u32::MAX] {
                for native in [false, true] {
                    let actual = if native {
                        encode_native_recording_part(sequence, runs)
                    } else {
                        encode_part(sequence, runs)
                    };
                    let before = before_direct_mdat(sequence, runs, native, None);
                    assert_eq!(actual, before, "sequence {sequence}, native {native}");
                    assert_layout(&actual, runs, native, None);
                    if !native {
                        let mut old_origin = None;
                        let mut new_origin = None;
                        assert_eq!(
                            rebase_part_for_file(&actual, Some(48_000), &mut new_origin)
                                .map_err(|_| ()),
                            rebase_part_for_file(&before, Some(48_000), &mut old_origin)
                                .map_err(|_| ()),
                            "standalone recording rebase"
                        );
                        assert_eq!(new_origin, old_origin);
                    }
                }
                if runs.iter().filter(|r| r.track_id == AUDIO_TRACK).count() == 1
                    && runs
                        .iter()
                        .find(|r| r.track_id == AUDIO_TRACK)
                        .is_some_and(|r| !r.samples.is_empty())
                {
                    for duration in [1, 576, 1024] {
                        let actual = encode_native_recording_part_with_final_audio_duration(
                            sequence, runs, duration,
                        );
                        assert_eq!(
                            actual,
                            before_direct_mdat(sequence, runs, true, Some(duration)),
                            "final native duration {duration}"
                        );
                        assert_layout(&actual, runs, true, Some(duration));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod file_time_tests {
    use super::*;
    use mp4_atom::Decode;

    fn fragment(video: u64, audio: u64) -> Bytes {
        let samples = [Sample {
            duration: 3000,
            cts: 0,
            sync: true,
            data: Bytes::from_static(b"fixture"),
        }];
        encode_part(
            1,
            &[
                TrackRun {
                    track_id: VIDEO_TRACK,
                    base_time: video,
                    samples: &samples,
                },
                TrackRun {
                    track_id: AUDIO_TRACK,
                    base_time: audio,
                    samples: &samples,
                },
            ],
        )
    }
    fn times(bytes: &Bytes) -> Vec<u64> {
        let mut input = bytes.as_ref();
        Moof::decode(&mut input)
            .expect("valid epoch regression fixture")
            .traf
            .iter()
            .map(|t| {
                t.tfdt
                    .as_ref()
                    .expect("valid epoch regression fixture")
                    .base_media_decode_time
            })
            .collect()
    }
    #[test]
    fn file_origin_is_shared_and_audio_may_precede_first_video() {
        let bytes = fragment(900_000, 479_520); // audio 10ms before video
        let original = bytes.clone();
        let mut base = None;
        let output = rebase_part_for_file(&bytes, Some(48_000), &mut base)
            .expect("valid epoch regression fixture");
        assert_eq!(times(&output), [900, 0]);
        assert_eq!(bytes, original);
        assert_eq!(
            times(
                &rebase_part_for_file(&fragment(918_000, 489_120), Some(48_000), &mut base)
                    .expect("valid epoch regression fixture")
            ),
            [18_900, 9600]
        );
        let saved = base;
        assert!(rebase_part_for_file(&fragment(0, 0), Some(48_000), &mut base).is_err());
        assert_eq!(base, saved);
    }
    #[test]
    fn own_box_walk_accepts_v0_and_rejects_versions_sizes_tracks_and_overflow() {
        let bytes = fragment(90_000, 48_000);
        // Compact every v1 tfdt to v0 and adjust its traf and moof sizes.
        let mut small = bytes.to_vec();
        let mut moof = u32::from_be_bytes(
            small[..4]
                .try_into()
                .expect("valid epoch regression fixture"),
        );
        let mut at = 8;
        while at < moof as usize {
            let size = u32::from_be_bytes(
                small[at..at + 4]
                    .try_into()
                    .expect("valid epoch regression fixture"),
            ) as usize;
            if &small[at + 4..at + 8] == b"traf" {
                let mut child = at + 8;
                while child < at + size {
                    let n = u32::from_be_bytes(
                        small[child..child + 4]
                            .try_into()
                            .expect("valid epoch regression fixture"),
                    ) as usize;
                    if &small[child + 4..child + 8] == b"tfdt" {
                        small[child..child + 4].copy_from_slice(&16u32.to_be_bytes());
                        small[child + 8] = 0;
                        small.drain(child + 12..child + 16);
                        small[at..at + 4].copy_from_slice(&((size - 4) as u32).to_be_bytes());
                        moof -= 4;
                        break;
                    }
                    child += n;
                }
                at += size - 4;
            } else {
                at += size;
            }
        }
        small[..4].copy_from_slice(&moof.to_be_bytes());
        // The compacted moof is eight bytes shorter; keep payload offsets valid.
        let mut at = 0;
        while let Some(index) = small[at..moof as usize]
            .windows(4)
            .position(|box_type| box_type == b"trun")
        {
            let offset = at + index + 12;
            let value = i32::from_be_bytes(
                small[offset..offset + 4]
                    .try_into()
                    .expect("valid epoch regression fixture"),
            ) - 8;
            small[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
            at = offset + 4;
        }
        let rebased = rebase_part_for_file(&Bytes::from(small), Some(48_000), &mut None)
            .expect("valid epoch regression fixture");
        assert_eq!(times(&rebased), [0, 0]);
        for bad in [
            Bytes::new(),
            Bytes::from_static(b"invalid!"),
            bytes.slice(..bytes.len() - 1),
            {
                let mut b = bytes.to_vec();
                let at = b
                    .windows(4)
                    .position(|x| x == b"tfdt")
                    .expect("valid epoch regression fixture");
                b[at + 4] = 2;
                Bytes::from(b)
            },
            {
                let mut b = bytes.to_vec();
                let at = b
                    .windows(4)
                    .position(|x| x == b"tfhd")
                    .expect("valid epoch regression fixture");
                b[at + 8..at + 12].copy_from_slice(&3u32.to_be_bytes());
                Bytes::from(b)
            },
        ] {
            assert!(rebase_part_for_file(&bad, Some(48_000), &mut None).is_err());
        }
        let mut enormous = Some(FileTimeBase {
            ticks: u64::MAX,
            timescale: 1,
        });
        assert!(rebase_part_for_file(&bytes, Some(48_000), &mut enormous).is_err());
    }
}
