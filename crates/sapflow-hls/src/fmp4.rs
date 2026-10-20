//! Fragmented MP4: the init segment and one `moof` + `mdat` per part.
//!
//! Box encoding comes from `mp4-atom`; this module only decides what goes in
//! them. Track 1 is H.264 video at 90 kHz, track 2 (optional) is AAC at its
//! sample rate.

use bytes::Bytes;
use mp4_atom::{
    Audio, Avc1, Avcc, Dinf, Dref, Encode, Esds, Ftyp, Hdlr, Mdat, Mdhd, Mdia, Mfhd, Minf, Moof,
    Moov, Mp4a, Mvex, Mvhd, Smhd, Stbl, Stco, Stsc, Stsd, Stsz, Stts, Tfdt, Tfhd, Tkhd, Traf, Trak,
    Trex, Trun, TrunEntry, Url, Visual, Vmhd, esds,
};

use crate::codec::{AacConfig, H264Config};

pub const VIDEO_TRACK: u32 = 1;
pub const AUDIO_TRACK: u32 = 2;
pub const VIDEO_TIMESCALE: u32 = 90_000;

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
    pub sync: bool,
    pub data: Bytes,
}

/// A track's samples within one part.
#[derive(Debug, Clone)]
pub struct TrackRun<'a> {
    pub track_id: u32,
    /// Decode time of the first sample, in the track's timescale.
    pub base_time: u64,
    pub samples: &'a [Sample],
}

/// Builds `ftyp` + `moov` for the given tracks.
pub fn init_segment(video: &H264Config, audio: Option<&AacConfig>) -> Bytes {
    init(video, audio, None)
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
    init(video, audio, Some(duration.as_millis() as u64))
}

fn init(video: &H264Config, audio: Option<&AacConfig>, duration_ms: Option<u64>) -> Bytes {
    let ftyp = Ftyp {
        major_brand: b"iso5".into(),
        minor_version: 1,
        compatible_brands: vec![b"iso5".into(), b"iso6".into(), b"mp41".into()],
    };

    let mut trak = vec![video_trak(video)];
    let mut trex = vec![Trex {
        track_id: VIDEO_TRACK,
        default_sample_description_index: 1,
        ..Default::default()
    }];
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
        return Bytes::new();
    }
    Bytes::from(buf)
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
pub fn encode_part(sequence_number: u32, runs: &[TrackRun<'_>]) -> Bytes {
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
                        .map(|s| TrunEntry {
                            duration: Some(s.duration),
                            size: Some(s.data.len() as u32),
                            flags: Some(if s.sync { FLAGS_SYNC } else { FLAGS_NON_SYNC }),
                            cts: Some(s.cts),
                        })
                        .collect(),
                }],
                ..Default::default()
            })
            .collect(),
    };

    // The data offsets depend on the moof's own size, which does not depend
    // on their values: encode once to measure, then again with real offsets.
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
}
