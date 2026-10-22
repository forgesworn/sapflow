//! FLV tag bodies (as RTMP carries them) in, muxer out.
//!
//! The RTMP session itself is the server's business; this is the media part:
//! the AVC and AAC tag headers (a few bytes each) and their sequence headers.

use std::time::SystemTime;

use crate::codec::{self, AacConfig};
use crate::ingest::{EsIngest, IngestError};
use crate::muxer::{Muxer, MuxerConfig, Update};

const CODEC_AVC: u8 = 7;
const SOUND_AAC: u8 = 10;

/// One RTMP publisher's media.
pub struct FlvIngest {
    es: EsIngest,
    nal_length_size: usize,
    last_ms: Option<u32>,
    /// Milliseconds on a 64-bit timeline (RTMP timestamps wrap at 2^32).
    base: i64,
}

impl FlvIngest {
    /// An ingest with no muxer yet: the first keyframe, once its parameter sets
    /// are known, creates one from `cfg`. No audio is expected until
    /// [`FlvIngest::expect_audio`] or an AAC sequence header says there is
    /// some.
    pub fn new(cfg: MuxerConfig) -> Self {
        let mut es = EsIngest::new(cfg);
        // Explicit metadata can announce audio before either encoder supplies
        // media. Without metadata, the first IDR starts a provisional video
        // muxer; AAC arriving before servable content restarts it at an IDR.
        es.set_expect_audio(false);
        Self {
            es,
            nal_length_size: 4,
            last_ms: None,
            base: 0,
        }
    }

    /// The muxer, once a keyframe has started one.
    pub fn muxer(&self) -> Option<&Muxer> {
        self.es.muxer()
    }

    /// Carries on another connection's stream ([`EsIngest::resume`]).
    pub fn resume(cfg: MuxerConfig, muxer: Muxer) -> Self {
        let expect_audio = muxer.audio().is_some();
        let mut ingest = Self::new(cfg);
        ingest.es = EsIngest::resume(cfg, muxer);
        // An immutable served A/V initialization cannot provisionally become
        // video-only merely because this connection's AAC header comes later.
        // Explicit metadata may still announce that audio was disabled.
        ingest.es.set_expect_audio(expect_audio);
        ingest
    }

    /// The muxer, mutably, once a keyframe has started one.
    pub fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        self.es.muxer_mut()
    }

    /// Removes and returns the muxer, for [`FlvIngest::resume`] on a later
    /// connection.
    pub fn take_muxer(&mut self) -> Option<Muxer> {
        self.es.take_muxer()
    }

    /// From RTMP's `onMetaData`: whether to hold the first keyframe until
    /// the AAC sequence header arrives.
    pub fn expect_audio(&mut self, expect: bool) {
        self.es.set_expect_audio(expect);
    }

    fn timeline(&mut self, ms: u32) -> i64 {
        if let Some(last) = self.last_ms
            && ms < last
            && last - ms > u32::MAX / 2
        {
            self.base += 1 << 32;
        }
        self.last_ms = Some(ms);
        self.base + i64::from(ms)
    }

    /// A video tag body. Only AVC is accepted.
    pub fn video(&mut self, ms: u32, tag: &[u8], now: SystemTime) -> Result<Update, IngestError> {
        let [head, packet_type, c0, c1, c2, body @ ..] = tag else {
            return Ok(Update::default());
        };
        if head & 0x0f != CODEC_AVC {
            return Err(IngestError::UnsupportedVideo);
        }
        let t = self.timeline(ms) * 90;
        match packet_type {
            0 => {
                if let Some((sps, pps, len)) = codec::avc_decoder_config(body) {
                    self.es.parameter_sets(Some(sps), Some(pps));
                    self.nal_length_size = len;
                }
                Ok(Update::default())
            }
            1 => {
                // Composition time: signed 24-bit milliseconds.
                let raw = (u32::from(*c0) << 16) | (u32::from(*c1) << 8) | u32::from(*c2);
                let cts = i64::from(((raw << 8) as i32) >> 8) * 90;
                let au = codec::access_unit_avcc(body, self.nal_length_size);
                self.es.video(t, t + cts, au, now)
            }
            _ => Ok(Update::default()),
        }
    }

    /// An audio tag body. Only AAC is used; anything else is ignored.
    pub fn audio(&mut self, ms: u32, tag: &[u8]) {
        let [head, packet_type, body @ ..] = tag else {
            return;
        };
        if head >> 4 != SOUND_AAC {
            return;
        }
        match packet_type {
            0 => {
                if let Some(c) = AacConfig::from_audio_specific_config(body) {
                    self.es.audio_config(c);
                    self.es.set_expect_audio(true);
                }
            }
            1 => {
                let t = self.timeline(ms) * 90;
                if let Some(c) = self.audio_config() {
                    self.es.audio(t, c, body);
                }
            }
            _ => {}
        }
    }

    fn audio_config(&self) -> Option<AacConfig> {
        self.es.known_audio()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muxer::LEADING_GAPS;

    /// Converts the TS fixture's frames into FLV tags, as an RTMP encoder
    /// would send them, by demuxing it with `TsIngest`'s building blocks.
    fn flv_from_fixture() -> Vec<(bool, u32, Vec<u8>)> {
        flv_from_ts(include_bytes!("../tests/fixtures/testsrc-3s.ts"))
    }

    fn flv_from_ts(ts: &[u8]) -> Vec<(bool, u32, Vec<u8>)> {
        let mut tags = Vec::new();
        let mut seq_sent = false;
        let mut audio_seq_sent = false;
        crate::ingest::tests_support::elementary(ts, |ev| match ev {
            crate::ingest::tests_support::Es::Video { dts, pts, annexb } => {
                let au = codec::access_unit(&annexb);
                if !seq_sent {
                    let (Some(sps), Some(pps)) = (&au.sps, &au.pps) else {
                        return;
                    };
                    let mut rec = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
                    rec.extend((sps.len() as u16).to_be_bytes());
                    rec.extend(sps);
                    rec.push(1);
                    rec.extend((pps.len() as u16).to_be_bytes());
                    rec.extend(pps);
                    let mut tag = vec![0x17, 0, 0, 0, 0];
                    tag.extend(rec);
                    tags.push((true, (dts / 90) as u32, tag));
                    seq_sent = true;
                }
                let cts = ((pts - dts) / 90) as u32;
                let key = if au.idr { 0x17 } else { 0x27 };
                let mut tag = vec![key, 1, (cts >> 16) as u8, (cts >> 8) as u8, cts as u8];
                tag.extend(au.data);
                tags.push((true, (dts / 90) as u32, tag));
            }
            crate::ingest::tests_support::Es::Audio {
                pts,
                config,
                payload,
            } => {
                if !audio_seq_sent {
                    audio_seq_sent = true;
                    let mut tag = vec![0xaf, 0];
                    tag.extend(config.audio_specific_config());
                    tags.push((false, (pts / 90) as u32, tag));
                }
                let mut tag = vec![0xaf, 1];
                tag.extend(payload);
                tags.push((false, (pts / 90) as u32, tag));
            }
        });
        // Encoders send both sequence headers before any frame.
        if let Some(i) = tags.iter().position(|(v, _, t)| !*v && t[1] == 0) {
            let (_, _, seq) = tags.remove(i);
            tags.insert(0, (false, 0, seq));
        }
        tags
    }

    #[test]
    fn resumed_rtmp_aspect_epochs_keep_audio_when_its_header_follows_first_idr() {
        for metadata in [false, true] {
            let cfg = MuxerConfig::default();
            let mut ingest = FlvIngest::new(cfg);
            let mut check = crate::playlist::RuleCheck::default();
            for (epoch, bytes) in [
                include_bytes!("../tests/fixtures/epoch-portrait.ts").as_slice(),
                include_bytes!("../tests/fixtures/epoch-landscape.ts").as_slice(),
                include_bytes!("../tests/fixtures/epoch-portrait.ts").as_slice(),
            ]
            .into_iter()
            .enumerate()
            {
                if metadata {
                    ingest.expect_audio(true);
                }
                let mut tags = flv_from_ts(bytes);
                if epoch != 0 {
                    let audio_index = tags
                        .iter()
                        .position(|(video, _, tag)| !*video && tag[1] == 0)
                        .expect("valid RTMP epoch fixture");
                    let audio = tags.remove(audio_index);
                    let idr_index = tags
                        .iter()
                        .position(|(video, _, tag)| *video && tag[1] == 1)
                        .expect("valid RTMP epoch fixture");
                    tags.insert(idr_index + 1, audio);
                }
                for (video, ms, tag) in tags {
                    if video {
                        ingest
                            .video(ms, &tag, SystemTime::UNIX_EPOCH)
                            .expect("valid RTMP epoch fixture");
                    } else {
                        ingest.audio(ms, &tag);
                    }
                    if let Some(muxer) = ingest.muxer().filter(|m| m.has_content()) {
                        check
                            .check(&crate::playlist::media(muxer, "rtmp"))
                            .expect("valid RTMP epoch fixture");
                    }
                }
                let muxer = ingest.muxer().expect("valid RTMP epoch fixture");
                assert_eq!(muxer.current_initialization_id(), epoch as u64);
                assert_eq!(
                    muxer.audio().expect("valid RTMP epoch fixture").sample_rate,
                    48_000
                );
                assert_eq!(muxer.video().width, if epoch == 1 { 1280 } else { 720 });
                assert!(
                    muxer
                        .open_parts()
                        .iter()
                        .chain(muxer.entries().iter().filter_map(|entry| match entry {
                            crate::muxer::Entry::Segment(segment)
                                if segment.initialization.id == epoch as u64 =>
                                segment.parts.first(),
                            _ => None,
                        }))
                        .any(|part| part.initialization.id == epoch as u64)
                );
                let mut held = ingest.take_muxer().expect("valid RTMP epoch fixture");
                held.interrupt(SystemTime::UNIX_EPOCH);
                ingest = FlvIngest::resume(cfg, held);
            }
        }
    }

    #[test]
    fn rtmp_style_tags_become_segments() {
        let mut ing = FlvIngest::new(MuxerConfig::default());
        for (video, ms, tag) in flv_from_fixture() {
            if video {
                ing.video(ms, &tag, SystemTime::UNIX_EPOCH).expect("video");
            } else {
                ing.audio(ms, &tag);
            }
        }
        let m = ing.muxer().expect("muxer");
        assert!(m.has_content());
        assert_eq!((m.video().width, m.video().height), (320, 180));
        assert_eq!(m.audio().map(|a| a.sample_rate), Some(48000));
        let seg = m.segment(LEADING_GAPS).expect("segment");
        assert_eq!(seg.duration.as_millis(), 1000);
    }

    fn feed(ing: &mut FlvIngest, tags: &[(bool, u32, Vec<u8>)]) -> Update {
        let mut up = Update::default();
        for (video, ms, tag) in tags {
            if *video {
                up.merge(ing.video(*ms, tag, SystemTime::UNIX_EPOCH).expect("video"));
            } else {
                ing.audio(*ms, tag);
            }
        }
        up
    }

    /// Preserve main's reconnect/audio-disable case while enforcing retained
    /// codec epochs: changed tracks require an explicit announcement, and old
    /// media keeps its original initialization instead of resetting the stream.
    #[test]
    fn a_reconnect_resumes_or_changes_codec_epoch_without_resetting_media() {
        let tags = flv_from_fixture();
        let mut first = FlvIngest::new(MuxerConfig::default());
        feed(&mut first, &tags);
        let mut held = first.take_muxer().expect("muxer");
        held.interrupt(SystemTime::UNIX_EPOCH);
        let segments = held.next_part_id();

        let mut same = FlvIngest::resume(MuxerConfig::default(), held);
        let up = feed(&mut same, &tags);
        assert!(!up.restarted);
        let m = same.muxer().expect("muxer");
        assert!(m.next_part_id() > segments + 10, "media carried on");
        assert!(m.entries().iter().any(|e| matches!(
            e,
            crate::muxer::Entry::Segment(s) if s.discontinuity
        )));

        let mut held = same.take_muxer().expect("muxer");
        held.interrupt(SystemTime::UNIX_EPOCH);
        let next_part = held.next_part_id();
        let targets = (held.target_duration(), held.part_target());
        let old = held.segment(LEADING_GAPS).expect("retained old segment");
        let video_only: Vec<_> = tags.iter().filter(|t| t.0).cloned().collect();
        let mut other = FlvIngest::resume(MuxerConfig::default(), held);
        other.expect_audio(false); // Explicit metadata disables audio for this epoch.
        let up = feed(&mut other, &video_only);
        assert!(up.restarted);
        let m = other.muxer().expect("muxer");
        assert!(m.audio().is_none());
        assert!(m.has_content());
        assert!(m.next_part_id() > next_part);
        assert_eq!(m.epoch_first_part_id(), next_part);
        assert_eq!((m.target_duration(), m.part_target()), targets);
        assert_eq!(m.current_initialization_id(), old.initialization.id + 1);
        assert_eq!(
            m.initialization(old.initialization.id),
            Some(&old.initialization.data)
        );
        assert!(m.entries().iter().any(|entry| matches!(
            entry,
            crate::muxer::Entry::Segment(segment)
                if segment.initialization.id == m.current_initialization_id() && segment.discontinuity
        )));
        let mut rules = crate::playlist::RuleCheck::default();
        rules
            .check(&crate::playlist::media(m, "reconnect"))
            .expect("valid retained epochs");
    }

    #[test]
    fn aac_after_first_idr_restarts_only_the_unpublished_provisional_muxer() {
        let mut tags = flv_from_fixture();
        let sequence_index = tags
            .iter()
            .position(|(video, _, tag)| !*video && tag[1] == 0)
            .expect("AAC config");
        let audio_sequence = tags.remove(sequence_index);
        let first_frame = tags
            .iter()
            .position(|(video, _, tag)| *video && tag[1] == 1)
            .expect("first IDR");
        tags.insert(first_frame + 1, audio_sequence);
        let mut ing = FlvIngest::new(MuxerConfig::default());
        let mut saw_provisional = false;
        let mut saw_audio = false;
        for (video, ms, tag) in tags {
            if video {
                ing.video(ms, &tag, SystemTime::UNIX_EPOCH).expect("video");
                if !saw_audio && tag[1] == 1 {
                    let muxer = ing.muxer().expect("first IDR starts provisional muxer");
                    assert!(muxer.audio().is_none());
                    assert!(!muxer.has_content());
                    saw_provisional = true;
                }
            } else {
                ing.audio(ms, &tag);
                if tag[1] == 0 {
                    assert!(saw_provisional);
                    assert!(ing.muxer().is_none(), "unpublished init may be discarded");
                    saw_audio = true;
                }
            }
        }
        let muxer = ing.muxer().expect("next IDR starts A/V muxer");
        assert!(muxer.has_content());
        assert_eq!(muxer.audio().map(|config| config.sample_rate), Some(48_000));
    }

    #[test]
    fn aac_after_servable_video_does_not_change_the_existing_track_identity() {
        let tags = flv_from_fixture();
        let audio_sequence = tags
            .iter()
            .find(|(video, _, tag)| !*video && tag[1] == 0)
            .expect("AAC config")
            .2
            .clone();
        let mut ing = FlvIngest::new(MuxerConfig::default());
        for (video, ms, tag) in tags {
            if video {
                ing.video(ms, &tag, SystemTime::UNIX_EPOCH).expect("video");
            }
        }
        let muxer = ing.muxer().expect("video-only muxer");
        assert!(muxer.has_content());
        let init = muxer.init().clone();
        ing.audio(3000, &audio_sequence);
        let muxer = ing.muxer().expect("existing muxer retained");
        assert!(muxer.has_content());
        assert!(muxer.audio().is_none());
        assert_eq!(muxer.init(), &init);
    }

    #[test]
    fn refuses_non_avc_video_and_survives_junk() {
        let mut ing = FlvIngest::new(MuxerConfig::default());
        assert_eq!(
            ing.video(0, &[0x1c, 1, 0, 0, 0, 1, 2], SystemTime::UNIX_EPOCH),
            Err(IngestError::UnsupportedVideo)
        );
        for junk in [
            &[][..],
            &[0x17],
            &[0x17, 0, 0, 0, 0, 1, 2, 3],
            &[0x17, 1, 0xff, 0xff, 0xff, 0, 0, 0, 200],
        ] {
            let _ = ing.video(0, junk, SystemTime::UNIX_EPOCH);
        }
        for junk in [&[][..], &[0xaf], &[0xaf, 0, 0xff], &[0xaf, 1, 1, 2, 3]] {
            ing.audio(0, junk);
        }
        assert!(ing.muxer().is_none());
    }

    #[test]
    fn timestamps_survive_the_32_bit_wrap() {
        let mut ing = FlvIngest::new(MuxerConfig::default());
        assert_eq!(ing.timeline(u32::MAX - 10), i64::from(u32::MAX - 10));
        assert_eq!(ing.timeline(5), (1i64 << 32) + 5);
        assert_eq!(ing.timeline(3), (1i64 << 32) + 3);
    }
}
