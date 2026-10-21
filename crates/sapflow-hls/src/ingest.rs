//! MPEG-TS in, muxer out.
//!
//! TS and PES parsing come from `mpeg2ts`. Its reader pulls from a
//! `std::io::Read` and treats some stream errors as fatal, so it is fed one
//! 188-byte packet at a time from a buffer we own: a bad packet is dropped
//! and the next one parsed, and running out of input is reported as an error
//! that leaves the reader's state intact for the next push.

use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bytes::Bytes;
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::{ReadTsPacket, TsPacket, TsPacketReader, TsPayload};

use crate::codec::{self, AAC_FRAME_SAMPLES, AacConfig, H264Config};
use crate::muxer::{MuxError, Muxer, MuxerConfig, Update};

use crate::time::{Unwrapper, signed_delta};

const TS_SIZE: usize = 188;
const SYNC: u8 = 0x47;
/// Cap on unconsumed input (a few seconds at stage bitrates).
const MAX_BUFFER: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    /// The program carries a video codec other than H.264.
    UnsupportedVideo,
    /// The held presentation expired before new media recovered it.
    PresentationExpired,
    /// No keyframe for too long, or a segment beyond the size limit.
    SegmentTooLarge,
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PresentationExpired => f.write_str("presentation reconnect deadline expired"),
            Self::UnsupportedVideo => f.write_str("video is not H.264"),
            Self::SegmentTooLarge => f.write_str("segment too large (keyframe interval?)"),
        }
    }
}

impl std::error::Error for IngestError {}

#[derive(Debug, Default, Clone, Copy)]
struct Program {
    h264: bool,
    other_video: bool,
    aac: bool,
}

#[derive(Default)]
struct Slot {
    buf: Vec<u8>,
    pos: usize,
}

#[derive(Clone, Default)]
struct SlotReader(Arc<Mutex<Slot>>);

impl Read for SlotReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut slot = self.0.lock().map_err(|_| io::ErrorKind::Other)?;
        let n = out.len().min(slot.buf.len() - slot.pos);
        let start = slot.pos;
        out[..n].copy_from_slice(&slot.buf[start..start + n]);
        slot.pos += n;
        Ok(n)
    }
}

#[derive(Default)]
struct Input {
    buf: Vec<u8>,
    start: usize,
    starved: bool,
    program: Program,
    dropped: u64,
}

struct Feed {
    input: Arc<Mutex<Input>>,
    slot: SlotReader,
    inner: TsPacketReader<SlotReader>,
}

impl ReadTsPacket for Feed {
    fn read_ts_packet(&mut self) -> mpeg2ts::Result<Option<TsPacket>> {
        loop {
            let packet = {
                let mut input = self.input.lock().map_err(|_| starved())?;
                let Input { buf, start, .. } = &mut *input;
                // Resynchronise on the sync byte if the stream slipped.
                match buf[*start..].iter().position(|&b| b == SYNC) {
                    Some(skip) => *start += skip,
                    None => *start = buf.len(),
                }
                if buf.len() - *start < TS_SIZE {
                    input.starved = true;
                    return Err(starved());
                }
                let p = input.buf[input.start..input.start + TS_SIZE].to_vec();
                input.start += TS_SIZE;
                p
            };
            {
                let mut slot = self.slot.0.lock().map_err(|_| starved())?;
                slot.buf = packet;
                slot.pos = 0;
            }
            match self.inner.read_ts_packet() {
                Ok(Some(p)) => {
                    if let Some(TsPayload::Pmt(pmt)) = &p.payload
                        && let Ok(mut input) = self.input.lock()
                    {
                        let mut program = Program::default();
                        for es in &pmt.es_info {
                            match es.stream_type {
                                StreamType::H264 => program.h264 = true,
                                StreamType::AdtsAac => program.aac = true,
                                StreamType::H265
                                | StreamType::Mpeg1Video
                                | StreamType::Mpeg2Video
                                | StreamType::Mpeg4H263BasedVideo => program.other_video = true,
                                _ => {}
                            }
                        }
                        input.program = program;
                    }
                    return Ok(Some(p));
                }
                Ok(None) => {}
                Err(_) => {
                    if let Ok(mut input) = self.input.lock() {
                        input.dropped += 1;
                    }
                }
            }
        }
    }
}

fn starved() -> mpeg2ts::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "need more input").into()
}

/// Elementary streams in, muxer out: shared by the TS (SRT) and FLV (RTMP)
/// front ends. Timestamps are 90 kHz on one monotonic timeline.
pub struct EsIngest {
    cfg: MuxerConfig,
    origin: Option<i64>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    aac: Option<AacConfig>,
    /// Whether to hold the first keyframe until the audio config is known.
    expect_audio: bool,
    muxer: Option<Muxer>,
}

impl EsIngest {
    pub fn new(cfg: MuxerConfig) -> Self {
        Self {
            cfg,
            origin: None,
            sps: None,
            pps: None,
            aac: None,
            expect_audio: true,
            muxer: None,
        }
    }

    /// Carries on another connection's stream (see [`Muxer::interrupt`]).
    /// The first keyframe decides: with the same parameter sets and audio
    /// config, its media continues the stream after a discontinuity;
    /// otherwise it starts a new initialization epoch on the retained muxer
    /// and the update says `restarted` (for recording rollover).
    pub fn resume(cfg: MuxerConfig, muxer: Muxer) -> Self {
        Self {
            muxer: Some(muxer),
            ..Self::new(cfg)
        }
    }

    pub fn muxer(&self) -> Option<&Muxer> {
        self.muxer.as_ref()
    }

    pub fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        self.muxer.as_mut()
    }

    pub fn take_muxer(&mut self) -> Option<Muxer> {
        self.origin = None;
        self.muxer.take()
    }

    pub fn set_expect_audio(&mut self, expect: bool) {
        self.expect_audio = expect;
    }

    /// Parameter sets from out of band (RTMP's sequence header) or in band.
    pub fn parameter_sets(&mut self, sps: Option<Vec<u8>>, pps: Option<Vec<u8>>) {
        if let Some(sps) = sps {
            if let Some(m) = &self.muxer
                && self.origin.is_some()
                && m.video().sps != sps
                && self.sps.as_ref() != Some(&sps)
            {
                // The init segment is fixed at the first keyframe.
                tracing::warn!(
                    "encoder changed its SPS mid-stream (resolution or profile?); players keep the first one"
                );
            }
            self.sps = Some(sps);
        }
        if let Some(pps) = pps {
            self.pps = Some(pps);
        }
    }

    pub fn known_audio(&self) -> Option<AacConfig> {
        self.aac
    }

    pub fn audio_config(&mut self, config: AacConfig) {
        if self.aac.is_none() {
            if let Some(muxer) = &self.muxer
                && muxer.audio().is_none()
            {
                if muxer.has_content() {
                    // A served init segment and the recorder's track identity
                    // cannot acquire a new track in place.
                    tracing::warn!(
                        "AAC configuration arrived after video-only content became servable; audio cannot be added to this init segment"
                    );
                } else {
                    // Separate hardware encoders may deliver AVC + first IDR
                    // before AAC's sequence header. No content was published,
                    // so discard the provisional video-only muxer and start
                    // with both tracks at the next IDR.
                    self.muxer = None;
                    self.origin = None;
                }
            }
            self.aac = Some(config);
        }
    }

    /// One access unit, already length-prefixed (4 bytes).
    pub fn video(
        &mut self,
        dts: i64,
        pts: i64,
        au: codec::AccessUnit,
        now: SystemTime,
    ) -> Result<Update, IngestError> {
        self.parameter_sets(au.sps, au.pps);
        if au.data.is_empty() {
            return Ok(Update::default());
        }
        let mut restarted = false;
        if self.origin.is_none() {
            if !au.idr {
                return Ok(Update::default());
            }
            let (Some(sps), Some(pps)) = (&self.sps, &self.pps) else {
                return Ok(Update::default());
            };
            let Some(video) = H264Config::new(sps, pps) else {
                return Ok(Update::default());
            };
            // If audio is expected, wait for its config before the init
            // segment is fixed (or compared); the next keyframe will do.
            if self.expect_audio && self.aac.is_none() {
                return Ok(Update::default());
            }
            let audio = if self.expect_audio { self.aac } else { None };
            if let Some(m) = &mut self.muxer
                && (m.video().sps != video.sps
                    || m.video().pps != video.pps
                    || m.audio().copied() != audio)
            {
                tracing::warn!(
                    "reconnected encoder changed configuration; starting a new codec epoch on the existing timeline"
                );
                m.reconfigure(video.clone(), audio, now);
                restarted = true;
            }
            if self.muxer.is_none() {
                self.muxer = Some(Muxer::new(self.cfg, video, audio));
            }
            self.origin = Some(dts);
        }
        let (Some(muxer), Some(origin)) = (self.muxer.as_mut(), self.origin) else {
            return Ok(Update::default());
        };
        let mut update = muxer
            .push_video(
                dts - origin,
                pts - origin,
                au.idr,
                Bytes::from(au.data),
                now,
            )
            .map_err(|e| match e {
                MuxError::SegmentTooLarge => IngestError::SegmentTooLarge,
            })?;
        update.restarted |= restarted;
        Ok(update)
    }

    /// One raw AAC frame.
    pub fn audio(&mut self, pts: i64, config: AacConfig, payload: &[u8]) {
        self.audio_config(config);
        let (Some(muxer), Some(origin)) = (self.muxer.as_mut(), self.origin) else {
            return;
        };
        let t = pts - origin;
        if t >= 0 {
            muxer.push_audio(t, Bytes::copy_from_slice(payload));
        }
    }
}

/// One publisher's stream: TS bytes in, LL-HLS muxer state out.
pub struct TsIngest {
    input: Arc<Mutex<Input>>,
    pes: PesPacketReader<Feed>,
    clock: Unwrapper,
    es: EsIngest,
    pending: Update,
}

impl TsIngest {
    /// Carries on another connection's stream ([`EsIngest::resume`]).
    pub fn resume(cfg: MuxerConfig, muxer: Muxer) -> Self {
        let mut ts = Self::new(cfg);
        ts.es = EsIngest::resume(cfg, muxer);
        ts
    }

    pub fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        self.es.muxer_mut()
    }

    pub fn take_muxer(&mut self) -> Option<Muxer> {
        self.es.take_muxer()
    }

    pub fn new(cfg: MuxerConfig) -> Self {
        let input = Arc::new(Mutex::new(Input::default()));
        let slot = SlotReader::default();
        let feed = Feed {
            input: input.clone(),
            slot: slot.clone(),
            inner: TsPacketReader::new(slot),
        };
        Self {
            input,
            pes: PesPacketReader::new(feed),
            clock: Unwrapper::default(),
            es: EsIngest::new(cfg),
            pending: Update::default(),
        }
    }

    pub fn muxer(&self) -> Option<&Muxer> {
        self.es.muxer()
    }

    /// Packets the TS reader could not parse, for diagnostics.
    pub fn dropped_packets(&self) -> u64 {
        self.input.lock().map(|i| i.dropped).unwrap_or(0)
    }

    /// Feeds raw TS bytes (any alignment). `now` stamps new segments.
    pub fn push(&mut self, data: &[u8], now: SystemTime) -> Result<Update, IngestError> {
        {
            let Ok(mut input) = self.input.lock() else {
                return Ok(Update::default());
            };
            if input.start > 0 {
                let start = input.start;
                input.buf.drain(..start);
                input.start = 0;
            }
            if input.buf.len() + data.len() > MAX_BUFFER {
                input.buf.clear();
            }
            input.buf.extend_from_slice(data);
            input.starved = false;
        }
        self.pending = Update::default();
        loop {
            match self.pes.read_pes_packet() {
                Ok(Some(pes)) => self.on_pes(pes, now)?,
                Ok(None) => break,
                Err(_) => {
                    let starved = self.input.lock().map(|i| i.starved).unwrap_or(true);
                    if starved {
                        break;
                    }
                    // A malformed PES: its data is gone, the next one is fine.
                    if let Ok(mut input) = self.input.lock() {
                        input.dropped += 1;
                    }
                }
            }
        }
        Ok(self.pending)
    }

    fn program(&self) -> Program {
        self.input.lock().map(|i| i.program).unwrap_or_default()
    }

    fn on_pes(
        &mut self,
        pes: mpeg2ts::pes::PesPacket<Vec<u8>>,
        now: SystemTime,
    ) -> Result<(), IngestError> {
        let id = pes.header.stream_id.as_u8();
        let Some(pts) = pes.header.pts.map(|t| t.as_u64()) else {
            return Ok(());
        };
        match id {
            0xE0..=0xEF => self.on_video(pts, pes.header.dts.map(|t| t.as_u64()), &pes.data, now),
            0xC0..=0xDF => {
                self.on_audio(pts, &pes.data);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn on_video(
        &mut self,
        pts_raw: u64,
        dts_raw: Option<u64>,
        data: &[u8],
        now: SystemTime,
    ) -> Result<(), IngestError> {
        let program = self.program();
        if program.other_video && !program.h264 {
            return Err(IngestError::UnsupportedVideo);
        }
        self.es.set_expect_audio(program.aac);
        let dts_raw = dts_raw.unwrap_or(pts_raw);
        let dts = self.clock.unwrap(dts_raw);
        let pts = dts + signed_delta(pts_raw, dts_raw);
        let up = self.es.video(dts, pts, codec::access_unit(data), now)?;
        self.pending.merge(up);
        Ok(())
    }

    fn on_audio(&mut self, pts_raw: u64, data: &[u8]) {
        let pts = self.clock.unwrap(pts_raw);
        for (i, (config, payload)) in codec::adts_frames(data).into_iter().enumerate() {
            let step = i64::from(AAC_FRAME_SAMPLES) * 90_000 / i64::from(config.sample_rate.max(1));
            self.es.audio(pts + step * i as i64, config, payload);
        }
    }
}

/// Demuxes a clean TS buffer into elementary events, for other modules' tests.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    pub enum Es {
        Video {
            dts: i64,
            pts: i64,
            annexb: Vec<u8>,
        },
        Audio {
            pts: i64,
            config: AacConfig,
            payload: Vec<u8>,
        },
    }

    pub fn elementary(ts: &[u8], mut f: impl FnMut(Es)) {
        let mut reader = PesPacketReader::new(TsPacketReader::new(ts));
        let mut clock = Unwrapper::default();
        while let Ok(Some(pes)) = reader.read_pes_packet() {
            let Some(pts_raw) = pes.header.pts.map(|t| t.as_u64()) else {
                continue;
            };
            match pes.header.stream_id.as_u8() {
                0xE0..=0xEF => {
                    let dts_raw = pes.header.dts.map_or(pts_raw, |t| t.as_u64());
                    let dts = clock.unwrap(dts_raw);
                    let pts = dts + signed_delta(pts_raw, dts_raw);
                    f(Es::Video {
                        dts,
                        pts,
                        annexb: pes.data,
                    });
                }
                0xC0..=0xDF => {
                    let pts = clock.unwrap(pts_raw);
                    for (i, (config, payload)) in
                        codec::adts_frames(&pes.data).into_iter().enumerate()
                    {
                        let step =
                            i64::from(AAC_FRAME_SAMPLES) * 90_000 / i64::from(config.sample_rate);
                        f(Es::Audio {
                            pts: pts + step * i as i64,
                            config,
                            payload: payload.to_vec(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muxer::LEADING_GAPS;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/testsrc-3s.ts");

    fn ingest_in_chunks(chunk: usize) -> TsIngest {
        let mut ing = TsIngest::new(MuxerConfig::default());
        for c in FIXTURE.chunks(chunk) {
            ing.push(c, SystemTime::UNIX_EPOCH).expect("push");
        }
        ing
    }

    #[test]
    fn real_ffmpeg_stream_becomes_segments() {
        // SRT payloads are 1316 bytes (7 TS packets).
        let ing = ingest_in_chunks(1316);
        let m = ing.muxer().expect("muxer");
        assert!(m.has_content());
        assert_eq!(m.video().width, 320);
        assert_eq!(m.video().height, 180);
        assert_eq!(m.audio().map(|a| a.sample_rate), Some(48000));
        let seg = m.segment(LEADING_GAPS).expect("first segment");
        assert_eq!(seg.duration.as_millis(), 1000);
        assert_eq!(ing.dropped_packets(), 0);
    }

    #[test]
    fn odd_chunking_gives_the_same_result() {
        let a = ingest_in_chunks(1316);
        let b = ingest_in_chunks(1000);
        let (ma, mb) = (a.muxer().expect("a"), b.muxer().expect("b"));
        assert_eq!(ma.entries().len(), mb.entries().len());
        let (sa, sb) = (
            ma.segment(LEADING_GAPS).expect("a"),
            mb.segment(LEADING_GAPS).expect("b"),
        );
        assert_eq!(sa.data, sb.data);
    }

    #[test]
    fn garbage_and_truncation_do_not_derail_it() {
        let mut ing = TsIngest::new(MuxerConfig::default());
        ing.push(&[0x47, 1, 2, 3, 0xff, 0x00], SystemTime::UNIX_EPOCH)
            .expect("push");
        ing.push(&[0u8; 5000], SystemTime::UNIX_EPOCH)
            .expect("push");
        // Corrupt some bytes in the middle of the real stream.
        let mut data = FIXTURE.to_vec();
        for i in (20_000..data.len()).step_by(9_973) {
            data[i] ^= 0x5a;
        }
        for c in data.chunks(1316) {
            ing.push(c, SystemTime::UNIX_EPOCH).expect("push");
        }
        assert!(ing.muxer().is_some_and(|m| m.has_content()));
    }
}

#[cfg(test)]
mod epoch_window_tests {
    use super::*;
    use crate::{muxer::Entry, playlist};
    #[test]
    fn codec_epochs_retain_old_maps_only_for_the_bounded_window_and_inflight_owners() {
        let portrait = include_bytes!("../tests/fixtures/epoch-portrait.ts");
        let landscape = include_bytes!("../tests/fixtures/epoch-landscape.ts");
        let cfg = MuxerConfig {
            segment_count: 8,
            ..MuxerConfig::default()
        };
        let mut ingest = TsIngest::new(cfg);
        let mut checker = playlist::RuleCheck::default();
        let mut old = None;
        let mut video_end = 0;
        let mut last_part = None;
        for epoch in 0..16 {
            let bytes = if epoch % 2 == 0 {
                portrait.as_slice()
            } else {
                landscape.as_slice()
            };
            for chunk in bytes.chunks(1316) {
                ingest
                    .push(chunk, SystemTime::UNIX_EPOCH)
                    .expect("valid epoch regression fixture");
                let Some(muxer) = ingest.muxer() else {
                    continue;
                };
                if muxer.has_content() {
                    checker
                        .check(&playlist::media(muxer, "fixed"))
                        .expect("valid epoch regression fixture");
                }
            }
            let muxer = ingest.muxer().expect("valid epoch regression fixture");
            assert_eq!(muxer.current_initialization_id(), epoch);
            assert_eq!(muxer.target_duration(), 2);
            assert!(muxer.entries().len() <= cfg.segment_count);
            for id in last_part.map_or(0, |id| id + 1)..muxer.next_part_id() {
                if let crate::PartLookup::Found(part) = muxer.part(id) {
                    use mp4_atom::Decode;
                    let mut input = part.data.as_ref();
                    let moof =
                        mp4_atom::Moof::decode(&mut input).expect("valid epoch regression fixture");
                    let video = moof
                        .traf
                        .iter()
                        .find(|track| track.tfhd.track_id == 1)
                        .expect("valid epoch regression fixture");
                    let at = video
                        .tfdt
                        .as_ref()
                        .expect("valid epoch regression fixture")
                        .base_media_decode_time;
                    assert!(
                        at >= video_end,
                        "HLS decode time must not reset at a new epoch"
                    );
                    video_end = at
                        + video
                            .trun
                            .iter()
                            .flat_map(|run| &run.entries)
                            .map(|sample| {
                                u64::from(sample.duration.expect("valid epoch regression fixture"))
                            })
                            .sum::<u64>();
                    last_part = Some(id);
                }
            }
            if epoch == 0 {
                old = Some(muxer.segment(7).expect("valid epoch regression fixture"));
            }
            if epoch > 5 {
                assert!(muxer.initialization(0).is_none());
            }
            let epochs: std::collections::BTreeSet<_> = muxer
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Segment(segment) => Some(segment.initialization.id),
                    _ => None,
                })
                .chain(muxer.open_parts().iter().map(|part| part.initialization.id))
                .collect();
            assert!(epochs.len() <= cfg.segment_count + 1);
            let mut held = ingest.take_muxer().expect("valid epoch regression fixture");
            held.interrupt(SystemTime::UNIX_EPOCH);
            ingest = TsIngest::resume(cfg, held);
        }
        let old = old.expect("valid epoch regression fixture"); // a response already holding this Arc can finish
        assert_eq!(old.initialization.id, 0);
        assert!(!old.initialization.data.is_empty() && !old.data.is_empty());
        let latest = playlist::media(
            ingest.muxer().expect("valid epoch regression fixture"),
            "fixed",
        );
        assert!(
            checker
                .check(&latest.replace("fixed_seg", "changed_seg"))
                .is_err(),
            "same MSN must never be relabelled"
        );
    }
}
