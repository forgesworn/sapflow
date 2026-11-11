//! Entry points for the feedback fuzz targets (`rr_stats`,
//! `twcc_feedback`, `sr_timeline`) and their seed regression test. Inputs
//! use the `[u16 length, bytes]` records of [`super::records`], after a
//! mode byte.
//!
//! - `rr_stats`: a media SSRC's reception statistics, against a model that
//!   keeps every counted sequence number in a set. The counts, highest
//!   sequence number, cumulative and fractional loss and DLSR must match
//!   the model exactly, and every report block must survive the RR
//!   builder and parser.
//! - `twcc_feedback`: the transport-cc recorder, with sequence numbers and
//!   arrival times from the input. Every feedback packet is read back with
//!   [`crate::twcc::parse`]: each packet recorded is reported received
//!   exactly once, at its exact arrival time in 250 us units, and nothing
//!   else is; feedback stays within its size cap, counts on, and leaves
//!   nothing pending.
//! - `sr_timeline`: the receiver with video, audio, RTX and transport-cc,
//!   fed media, sender reports (honest, skewed or arbitrary) and clock
//!   steps. Frames come only after [`Event::Aligned`], each stream's `pts`
//!   never goes back and never runs ahead of arrival time beyond the
//!   timeline's stated bound, and every RTCP datagram parses, leads with a
//!   receiver report (unless reduced-size) and stays within its cap.
//!
//! Each also bounds state (pending packets, queued events) and output (a
//! constant per input record, plus what elapsed time accounts for).

use super::records;
use crate::opus::parse_packet;
use crate::receive::{
    Codec, Event, Receiver, ReceiverConfig, ReportConfig, RtxConfig, StreamConfig, StreamId,
};
use crate::rtcp::build::{self, MAX_FCI};
use crate::rtcp::{Compound, FMT_TRANSPORT_CC, PT_RTPFB, Packet};
use crate::rtp::{RtpBuilder, RtpPacket};
use crate::stats::{Arrival, Reception, WINDOW};
use crate::timeline::Alignment;
use crate::twcc::parse::{Status, parse_fci};
use crate::twcc::{DELTA_US, Recorder, TransportCcConfig};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const BASE: i64 = 1 << 32;

/// The model of [`Reception`]: every counted extended sequence number.
#[derive(Default)]
struct RrModel {
    started: bool,
    max: i64,
    base: i64,
    counted: BTreeSet<i64>,
    received: u64,
    expected_prior: i64,
    received_prior: u64,
}
impl RrModel {
    fn count(&mut self, seq: u16) {
        if !self.started {
            *self = Self {
                started: true,
                max: BASE + i64::from(seq),
                base: BASE + i64::from(seq),
                ..Self::default()
            };
            self.counted.insert(self.max);
            self.received = 1;
            return;
        }
        let ext = self.max + i64::from(seq.wrapping_sub(self.max as u16) as i16);
        if ext < self.max && self.max - ext >= WINDOW as i64 {
            return;
        }
        if !self.counted.insert(ext) {
            return;
        }
        self.max = self.max.max(ext);
        self.base = self.base.min(ext);
        self.received += 1;
        // Only the window matters for what comes next.
        let floor = self.max - WINDOW as i64;
        self.counted = self.counted.split_off(&floor);
    }
    fn expected(&self) -> i64 {
        self.max - self.base + 1
    }
}

/// `rr_stats`: a mode byte (bit 0: 48 kHz instead of 90 kHz), then records
/// whose first byte picks an operation (modulo 4):
///
/// - 0: a packet `[op, seq high, seq low, how, step high, step low, ms]`:
///   the timestamp moves by the step and the clock by the milliseconds.
///   `how` modulo 4: a first arrival, a repeat (counted if new, not
///   sampled), a probe (not counted), or a confirmed restart.
/// - 1: a sender report `[op, NTP timestamp (8 bytes)]`.
/// - 2: a report: the block is checked, built, parsed and a new interval
///   starts.
/// - 3: the clock moves on `[op, ms high, ms low]` milliseconds.
///
/// Returns the number of report blocks checked.
pub fn rr_stats(data: &[u8]) -> usize {
    let Some((&mode, rest)) = data.split_first() else {
        return 0;
    };
    let rate = if mode & 1 != 0 { 48_000 } else { 90_000 };
    let mut r = Reception::new(rate);
    let mut m = RrModel::default();
    let (mut now, mut ts) = (0u64, 0u32);
    let mut last_sr: Option<(u32, u64)> = None;
    let mut reports = 0usize;
    for record in records(rest) {
        let Some((&op, args)) = record.split_first() else {
            continue;
        };
        match op % 4 {
            0 => {
                let Some(&[hi, lo, how, sh, sl, ms]) = args.first_chunk::<6>() else {
                    continue;
                };
                let seq = u16::from_be_bytes([hi, lo]);
                ts = ts.wrapping_add(u32::from(u16::from_be_bytes([sh, sl])));
                now += u64::from(ms) * 1000;
                match how % 4 {
                    0 | 1 => {
                        let how = if how % 4 == 0 {
                            Arrival::Fresh
                        } else {
                            Arrival::Repeat
                        };
                        let before = m.received;
                        m.count(seq);
                        assert_eq!(r.count(seq, ts, now, how), m.received > before);
                    }
                    2 => {}
                    _ => {
                        r.restart();
                        m = RrModel::default();
                        let _ = r.count(seq.wrapping_sub(1), 0, now, Arrival::Repeat);
                        m.count(seq.wrapping_sub(1));
                        let _ = r.count(seq, ts, now, Arrival::Fresh);
                        m.count(seq);
                    }
                }
                assert_eq!(r.received(), m.received);
            }
            1 => {
                let Some(ntp) = args.first_chunk::<8>() else {
                    continue;
                };
                let ntp = u64::from_be_bytes(*ntp);
                r.sender_report(ntp, now);
                last_sr = Some(((ntp >> 16) as u32, now));
            }
            2 => {
                if !r.started() {
                    continue;
                }
                let b = r.block(0x5eed, now);
                assert!(m.started);
                assert_eq!(i64::from(b.highest_sequence), (m.max - BASE) & 0xffff_ffff);
                let lost = m.expected() - m.received as i64;
                assert!(lost >= 0, "each packet counted once");
                assert_eq!(i64::from(b.cumulative_lost), lost.min(i64::from(i32::MAX)));
                let expected_interval = m.expected() - m.expected_prior;
                let lost_interval = expected_interval - (m.received - m.received_prior) as i64;
                let fraction = if expected_interval <= 0 || lost_interval <= 0 {
                    0
                } else {
                    (lost_interval * 256 / expected_interval).min(255)
                };
                assert_eq!(i64::from(b.fraction_lost), fraction);
                match last_sr {
                    Some((middle, at)) => {
                        assert_eq!(b.last_sr, middle);
                        let delay = u128::from(now - at) * 65_536 / 1_000_000;
                        assert_eq!(
                            u128::from(b.delay_since_last_sr),
                            delay.min(u128::from(u32::MAX))
                        );
                    }
                    None => assert_eq!((b.last_sr, b.delay_since_last_sr), (0, 0)),
                }
                let mut out = Vec::new();
                build::receiver_report(&mut out, 1, &[b]);
                let c = Compound::parse(&out).expect("an RR parses");
                let Some(Ok(Packet::ReceiverReport(rr))) = c.packets().next() else {
                    panic!("an RR");
                };
                let mut read = rr.reports().next().expect("one block");
                assert_eq!(
                    read.cumulative_lost,
                    b.cumulative_lost.clamp(-0x80_0000, 0x7f_ffff)
                );
                read.cumulative_lost = b.cumulative_lost;
                assert_eq!(read, b);
                r.reported();
                m.expected_prior = m.expected();
                m.received_prior = m.received;
                reports += 1;
            }
            _ => {
                let Some(&[hi, lo]) = args.first_chunk::<2>() else {
                    continue;
                };
                now += u64::from(u16::from_be_bytes([hi, lo])) * 1000;
            }
        }
    }
    reports
}

/// `twcc_feedback`: a mode byte (bit 0: 64-byte feedback packets instead
/// of 1000; bit 1: 8 pending packets instead of 1024), then records whose
/// first byte picks an operation (modulo 5):
///
/// - 0: a packet `[op, seq high, seq low]` at the current time.
/// - 1: an extension element of any other length `[op, bytes]`.
/// - 2: the clock moves on `[op, high, low]` times 100 us.
/// - 3: the clock moves on `[op, s]` seconds, for large deltas and the
///   reference time.
/// - 4: the feedback timer: feedback if due.
///
/// Returns the number of feedback packets read back.
pub fn twcc_feedback(data: &[u8]) -> usize {
    let Some((&mode, rest)) = data.split_first() else {
        return 0;
    };
    let mut cfg = TransportCcConfig::new(1);
    if mode & 1 != 0 {
        cfg.max_packet = 64;
    }
    if mode & 2 != 0 {
        cfg.max_pending = 8;
    }
    let mut r = Recorder::new(cfg);
    let max_packet = r.config().max_packet;
    // Recorded and not yet reported, by extended sequence number (the
    // recorder's rule: the nearest to the highest recorded).
    let mut pending: BTreeMap<i64, u64> = BTreeMap::new();
    let mut highest: Option<i64> = None;
    let extend = |highest: Option<i64>, seq: u16| match highest {
        None => BASE + i64::from(seq),
        Some(h) => h + i64::from(seq.wrapping_sub(h as u16) as i16),
    };
    let mut now = 0u64;
    let mut fb_count = 0u8;
    let mut read = 0usize;
    let mut accepted = 0u64;
    // Feedback lists packets in ascending extended order, one flush after
    // another, so each received status must be the next pending packet:
    // none skipped, none twice, none invented, and gaps only where nothing
    // was recorded.
    let mut check = |r: &mut Recorder, now: u64, pending: &mut BTreeMap<i64, u64>| {
        let built = r.flush(now);
        assert!(
            built.len() <= pending.len(),
            "each feedback reports a packet"
        );
        for b in built {
            assert!(b.fci.len() + 12 <= max_packet, "{} bytes", b.fci.len() + 12);
            assert!(b.fci.len() <= 4 * MAX_FCI);
            let f = parse_fci(&b.fci).expect("our feedback reads back");
            assert_eq!(f.fb_count, fb_count);
            fb_count = fb_count.wrapping_add(1);
            let mut t: Option<i64> = None;
            for (i, s) in f.statuses.iter().enumerate() {
                let seq = f.base.wrapping_add(i as u16);
                let delta = match *s {
                    Status::NotReceived => continue,
                    Status::Small(d) => i64::from(d),
                    Status::Large(d) => {
                        assert!(!(0..=255).contains(&d), "a large delta that fits small");
                        i64::from(d)
                    }
                };
                let (ext, arrival) = pending.pop_first().expect("only what was recorded");
                assert_eq!(ext as u16, seq, "the next packet recorded, in order");
                let q = (arrival / DELTA_US) as i64;
                let at = match t {
                    None => {
                        assert_eq!(i64::from(f.reference), (q / 256) & 0xff_ffff);
                        (q / 256) * 256 + delta
                    }
                    Some(prev) => prev + delta,
                };
                assert_eq!(at, q, "arrival of {seq} exact");
                t = Some(at);
            }
            assert!(t.is_some(), "a feedback reports something received");
            assert!(
                matches!(f.statuses.last(), Some(s) if *s != Status::NotReceived),
                "ends at a received packet"
            );
            read += 1;
        }
        assert!(pending.is_empty(), "everything pending was reported");
        assert_eq!(r.pending(), 0);
    };
    for record in records(rest) {
        let Some((&op, args)) = record.split_first() else {
            continue;
        };
        match op % 5 {
            0 => {
                let Some(&[hi, lo]) = args.first_chunk::<2>() else {
                    continue;
                };
                let before = r.stats().packets;
                let full = r.record(now, &[hi, lo]);
                if r.stats().packets > before {
                    accepted += 1;
                    let ext = extend(highest, u16::from_be_bytes([hi, lo]));
                    highest = Some(highest.map_or(ext, |h| h.max(ext)));
                    assert!(
                        pending.insert(ext, now).is_none(),
                        "a copy was recorded twice"
                    );
                }
                assert!(r.pending() <= max_pending(&r));
                if full {
                    check(&mut r, now, &mut pending);
                }
            }
            1 => {
                if args.len() != 2 {
                    let before = r.stats();
                    assert!(!r.record(now, args));
                    assert_eq!(r.stats().malformed, before.malformed + 1);
                }
            }
            2 => {
                let Some(&[hi, lo]) = args.first_chunk::<2>() else {
                    continue;
                };
                now += u64::from(u16::from_be_bytes([hi, lo])) * 100;
            }
            3 => now += u64::from(args.first().copied().unwrap_or(0)) * 1_000_000,
            _ => {
                if r.due().is_some_and(|d| d <= now) {
                    check(&mut r, now, &mut pending);
                }
            }
        }
    }
    check(&mut r, now, &mut pending);
    assert!(read as u64 <= accepted);
    let stats = r.stats();
    assert_eq!(stats.packets, accepted);
    read
}

fn max_pending(r: &Recorder) -> usize {
    r.config().max_pending
}

const VIDEO_SSRC: u32 = 0x1111_1111;
const RTX_SSRC: u32 = 0x2222_2222;
const AUDIO_SSRC: u32 = 0x3333_3333;
const TWCC_ID: u8 = 3;
/// An NTP time for the sender's clock at receiver time zero.
const NTP_ZERO: u64 = 3_971_808_000 << 32;

fn ntp_after(us: i64) -> u64 {
    let frac = (i128::from(us) << 32) / 1_000_000;
    (i128::from(NTP_ZERO) + frac).clamp(0, i128::from(u64::MAX)) as u64
}

/// `sr_timeline`: a mode byte (bit 0: no start hold; bit 1: reduced-size
/// RTCP; bit 2: a 64-event queue, a 4000-byte hold and 300-byte
/// datagrams), then records whose first byte picks an operation (modulo
/// 6):
///
/// - 0 video and 1 audio: `[op, seq high, seq low, flags, step high, step
///   low, payload]`. Flag bit 0 is the marker; bit 1 advances the
///   timestamp a frame (3000 or 960); bit 2 adds the step; bit 3
///   subtracts it (a sender's timestamps going back); bit 4 sends it over
///   RTX instead (video only). Every packet carries a transport-wide
///   sequence number, which flag bit 5 holds still (a copy).
/// - 2: a sender report `[op, which, skew high, skew low]`: bit 0 of
///   `which` picks audio, and the report is honest (the stream's current
///   timestamp at the current time) but for a signed skew in ms. With bit
///   1, `[op, which, NTP (8 bytes), RTP (4 bytes)]` is sent as it is.
/// - 3: the clock moves on `[op, ms high, ms low]` milliseconds, then
///   timers run.
/// - 4: a datagram that is not a sender report, for no stream.
/// - 5: a packet for no stream, with the transport-wide number.
///
/// Returns the number of events and datagrams produced.
pub fn sr_timeline(data: &[u8]) -> usize {
    run_sr_timeline(data).map_or(0, |s| s.events + s.datagrams)
}

fn run_sr_timeline(data: &[u8]) -> Option<Session> {
    let (&mode, rest) = data.split_first()?;
    let mut cfg = ReceiverConfig {
        transport_cc: Some(TransportCcConfig::new(TWCC_ID)),
        ..ReceiverConfig::default()
    };
    if mode & 1 != 0 {
        cfg.timeline.start_hold = Duration::ZERO;
    }
    if mode & 2 != 0 {
        cfg.reports.reduced_size = true;
    }
    if mode & 4 != 0 {
        cfg.max_events = 64;
        cfg.timeline.max_hold_bytes = 4000;
        cfg.reports.max_datagram = 300;
    }
    let start = Instant::now();
    let mut r = Receiver::new(cfg, start);
    let mut video = StreamConfig::new(Codec::H264, 96);
    video.ssrc = Some(VIDEO_SSRC);
    video.rtx = Some(RtxConfig {
        payload_type: 97,
        ssrc: Some(RTX_SSRC),
    });
    video.nack = true;
    video.pli = true;
    let mut audio = StreamConfig::new(Codec::Opus, 111);
    audio.ssrc = Some(AUDIO_SSRC);
    audio.nack = true;
    let v = r.add_stream(video).expect("video");
    let a = r.add_stream(audio).expect("audio");
    let mut s = Session {
        cfg,
        start,
        now: 0,
        first_media: None,
        aligned: None,
        last_pts: [None, None],
        frames: 0,
        events: 0,
        datagrams: 0,
        records: 0,
    };
    let (mut ts, mut tseq, mut rtx_seq) = ([0u32; 2], 0u16, 0u16);
    for record in records(rest) {
        s.records += 1;
        let Some((&op, args)) = record.split_first() else {
            continue;
        };
        let at = s.start + Duration::from_millis(s.now);
        match op % 6 {
            kind @ (0 | 1 | 5) => {
                let Some((&[hi, lo, flags, sh, sl], payload)) = args.split_first_chunk::<5>()
                else {
                    continue;
                };
                let k = usize::from(kind == 1);
                let step = u32::from(u16::from_be_bytes([sh, sl]));
                if flags & 2 != 0 {
                    ts[k] = ts[k].wrapping_add(if k == 0 { 3000 } else { 960 });
                }
                if flags & 4 != 0 {
                    ts[k] = ts[k].wrapping_add(step);
                }
                if flags & 8 != 0 {
                    ts[k] = ts[k].wrapping_sub(step);
                }
                let seq = u16::from_be_bytes([hi, lo]);
                let rtx = kind == 0 && flags & 16 != 0;
                let (pt, ssrc, seq, body) = match (kind, rtx) {
                    (0, true) => {
                        rtx_seq = rtx_seq.wrapping_add(1);
                        let body = if payload.is_empty() {
                            Vec::new()
                        } else {
                            [&seq.to_be_bytes()[..], payload].concat()
                        };
                        (97, RTX_SSRC, rtx_seq, body)
                    }
                    (0, false) => (96, VIDEO_SSRC, seq, payload.to_vec()),
                    (1, _) => (111, AUDIO_SSRC, seq, payload.to_vec()),
                    _ => (100, 0x4444_4444, seq, payload.to_vec()),
                };
                if flags & 32 == 0 {
                    tseq = tseq.wrapping_add(1);
                }
                let built = RtpBuilder {
                    marker: flags & 1 != 0,
                    payload_type: pt,
                    sequence: seq,
                    timestamp: ts[k],
                    ssrc,
                    payload: body,
                    extension: Some(RtpBuilder::one_byte_elements(&[(
                        TWCC_ID,
                        &tseq.to_be_bytes(),
                    )])),
                    ..RtpBuilder::default()
                }
                .build();
                let parsed = RtpPacket::parse(&built).expect("built packets parse");
                let taken = r.handle_rtp(at, &parsed).is_ok();
                if taken && kind != 5 && !rtx {
                    s.first_media.get_or_insert(s.now);
                }
            }
            2 => {
                let Some((&which, args)) = args.split_first() else {
                    continue;
                };
                let k = usize::from(which & 1);
                let ssrc = [VIDEO_SSRC, AUDIO_SSRC][k];
                let (ntp, rtp) = if which & 2 != 0 {
                    let Some((ntp, rtp)) = args.split_first_chunk::<8>() else {
                        continue;
                    };
                    let Some(rtp) = rtp.first_chunk::<4>() else {
                        continue;
                    };
                    (u64::from_be_bytes(*ntp), u32::from_be_bytes(*rtp))
                } else {
                    let Some(&[hi, lo]) = args.first_chunk::<2>() else {
                        continue;
                    };
                    let skew = i64::from(i16::from_be_bytes([hi, lo]));
                    (ntp_after((s.now as i64 + skew) * 1000), ts[k])
                };
                let mut d = vec![0x80, 200, 0, 6];
                d.extend_from_slice(&ssrc.to_be_bytes());
                d.extend_from_slice(&ntp.to_be_bytes());
                d.extend_from_slice(&rtp.to_be_bytes());
                d.extend_from_slice(&[0; 8]);
                let c = Compound::parse(&d).expect("built SR parses");
                assert_eq!(r.handle_rtcp(at, &c), 1);
            }
            3 => {
                let Some(&[hi, lo]) = args.first_chunk::<2>() else {
                    continue;
                };
                s.now += u64::from(u16::from_be_bytes([hi, lo]));
                r.handle_timeout(s.start + Duration::from_millis(s.now));
                for _ in 0..64 {
                    let Some(next) = r.poll_timeout() else {
                        break;
                    };
                    if next > s.start + Duration::from_millis(s.now) {
                        break;
                    }
                    r.handle_timeout(next);
                    if let Some(after) = r.poll_timeout() {
                        assert!(after > next, "the next timer is later");
                    }
                    s.drain(&mut r, [v, a]);
                }
            }
            _ => {
                let mut d = Vec::new();
                build::pli(&mut d, 0x4444_4444, VIDEO_SSRC);
                let c = Compound::parse(&d).expect("built PLI parses");
                assert_eq!(r.handle_rtcp(at, &c), 0);
            }
        }
        s.drain(&mut r, [v, a]);
    }
    Some(s)
}

struct Session {
    cfg: ReceiverConfig,
    start: Instant,
    /// Milliseconds.
    now: u64,
    first_media: Option<u64>,
    aligned: Option<Alignment>,
    last_pts: [Option<u64>; 2],
    frames: u64,
    events: usize,
    datagrams: usize,
    records: usize,
}

impl Session {
    fn drain(&mut self, r: &mut Receiver, streams: [StreamId; 2]) {
        let cfg = self.cfg;
        loop {
            let mut d = Vec::new();
            if !r.poll_feedback(1, b"sapflow", &mut d) {
                break;
            }
            self.datagrams += 1;
            let c = Compound::parse(&d).expect("datagram parses");
            let packets: Vec<_> = c.packets().map(|p| p.expect("valid")).collect();
            assert!(!packets.is_empty());
            assert!(
                d.len() <= cfg.reports.max_datagram || packets.len() <= 3,
                "{} bytes",
                d.len()
            );
            if !cfg.reports.reduced_size {
                assert!(c.starts_with_report(), "RR first");
            }
            for p in packets {
                match p {
                    Packet::ReceiverReport(rr) => {
                        for b in rr.reports() {
                            assert!([VIDEO_SSRC, AUDIO_SSRC].contains(&b.ssrc));
                            assert!(b.cumulative_lost >= 0, "each packet counted once");
                        }
                    }
                    Packet::Other(o)
                        if o.packet_type == PT_RTPFB && o.count == FMT_TRANSPORT_CC =>
                    {
                        let fci = o.body.get(8..).expect("fci");
                        parse_fci(fci).expect("transport-cc reads back");
                    }
                    _ => {}
                }
            }
        }
        let lead = 90 * u64::try_from(cfg.timeline.max_lead.as_millis()).unwrap_or(0);
        while let Some(e) = r.poll_event() {
            self.events += 1;
            let (stream, pts) = match e {
                Event::Aligned { alignment } => {
                    assert!(self.aligned.is_none(), "aligned once");
                    self.aligned = Some(alignment);
                    continue;
                }
                Event::AccessUnit { stream, pts, .. } => (stream, pts),
                Event::Audio { stream, frame, pts } => {
                    let info = parse_packet(&frame.data).expect("audio parses");
                    assert_eq!(info.duration(), frame.duration);
                    (stream, pts)
                }
                _ => continue,
            };
            assert!(self.aligned.is_some(), "no frame before the alignment");
            self.frames += 1;
            let k = stream.index();
            assert!(k < 2);
            if let Some(prev) = self.last_pts[k] {
                assert!(pts >= prev, "pts {pts} after {prev}");
            }
            self.last_pts[k] = Some(pts);
            // Bounded by arrival time: 90 ticks a millisecond since the
            // first media packet, 0.1% for clock rates, the lead twice
            // (once for the alignment), and a tick a frame.
            let elapsed = 90 * (self.now - self.first_media.expect("media came first"));
            let bound = elapsed + elapsed / 1000 + 2 * lead + self.frames;
            assert!(pts <= bound, "pts {pts} beyond {bound}");
        }
        // The start hold is bounded in time.
        let hold = u64::try_from(cfg.timeline.start_hold.as_millis()).unwrap_or(u64::MAX);
        if self
            .first_media
            .is_some_and(|f| self.now >= f.saturating_add(hold))
        {
            assert!(self.aligned.is_some(), "still holding after the hold");
        }
        // State: the event queue and transport-cc pending list.
        assert!(r.events_queued() <= cfg.max_events + 1);
        let max_pending = cfg.transport_cc.map_or(0, |t| t.max_pending);
        assert!(r.transport_cc_pending() <= max_pending);
        for s in streams {
            let (packets, bytes) = r.buffered(s).expect("stream");
            assert!(packets <= cfg.max_packets && bytes <= cfg.max_bytes);
        }
        // Output: a constant per record, plus Opus fill that arrival time
        // accounts for (2.5 ms frames), plus reports and transport-cc
        // cadence.
        let fill =
            (u64::from(cfg.opus.max_fill) + u64::from(cfg.opus.allowance) + 48 * self.now) / 120;
        let dropped: u64 = streams
            .iter()
            .filter_map(|&s| r.stats(s))
            .map(|s| s.events_dropped)
            .sum();
        let events = self.events as u64 + dropped;
        assert!(events <= 16 * self.records as u64 + fill, "{events} events");
        let cadence =
            self.now / ReportConfig::default().interval.as_millis() as u64 + self.now / 50;
        assert!(
            (self.datagrams as u64) <= 200 * self.records as u64 + cadence + 2,
            "{} datagrams",
            self.datagrams
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::push_record;
    use super::*;
    use crate::timeline::Fallback;
    use std::path::{Path, PathBuf};

    fn seeds(target: &str) -> Vec<(PathBuf, Vec<u8>)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/seeds")
            .join(target);
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|entry| entry.expect("entry").path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        assert!(!files.is_empty(), "seeds for {target}");
        files
            .into_iter()
            .map(|p| {
                let bytes = std::fs::read(&p).expect("seed");
                (p, bytes)
            })
            .collect()
    }

    #[test]
    fn rr_seeds_report() {
        for (path, bytes) in seeds("rr_stats") {
            assert!(rr_stats(&bytes) > 0, "{}", path.display());
        }
    }

    #[test]
    fn twcc_seeds_give_feedback() {
        for (path, bytes) in seeds("twcc_feedback") {
            assert!(twcc_feedback(&bytes) > 0, "{}", path.display());
        }
    }

    /// Every seed makes frames, aligned as its name says.
    #[test]
    fn sr_timeline_seeds_make_frames() {
        for (path, bytes) in seeds("sr_timeline") {
            let s = run_sr_timeline(&bytes).expect("a mode byte");
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            assert!(
                s.frames > 20 && s.datagrams >= 5,
                "{name}: {} frames, {} datagrams, {} ms",
                s.frames,
                s.datagrams,
                s.now
            );
            let want = match name {
                "ffmpeg-honest-reports" | "ffmpeg-skewed-reports" => Alignment::SenderReports,
                "ffmpeg-no-reports-small" => Alignment::Arrival(Fallback::HoldFull),
                _ => Alignment::Arrival(Fallback::Timeout),
            };
            assert_eq!(s.aligned, Some(want), "{name}");
        }
    }

    #[test]
    fn empty_and_junk_inputs() {
        for f in [rr_stats, twcc_feedback, sr_timeline] {
            assert_eq!(f(&[]), 0);
            let _ = f(&[0xff; 64]);
            let _ = f(&[0, 0, 3, 2, 0x80, 0x80]);
        }
    }

    /// Mutated seeds through the three targets: not coverage-guided, a
    /// smoke run of their invariants where cargo-fuzz is not installed.
    /// `SAPFLOW_FUZZ_SMOKE=<inputs per target>` (default 2000), and
    /// `SAPFLOW_FUZZ_SEED` for the generator.
    #[test]
    #[ignore = "a long mutation run"]
    fn mutation_smoke() {
        let var = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
        let n = var("SAPFLOW_FUZZ_SMOKE").unwrap_or(2000);
        let mut x = var("SAPFLOW_FUZZ_SEED").unwrap_or(0x9e37_79b9_7f4a_7c15) | 1;
        let mut rand = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        type Target = (&'static str, fn(&[u8]) -> usize);
        let targets: [Target; 3] = [
            ("rr_stats", rr_stats),
            ("twcc_feedback", twcc_feedback),
            ("sr_timeline", sr_timeline),
        ];
        for (target, f) in targets {
            let seeds = seeds(target);
            let (mut produced, started) = (0usize, std::time::Instant::now());
            for i in 0..n {
                let mut d = seeds[i as usize % seeds.len()].1.clone();
                for _ in 0..1 + rand() % 8 {
                    if d.is_empty() {
                        break;
                    }
                    let at = rand() as usize % d.len();
                    let len = (rand() as usize % 64).min(d.len() - at);
                    match rand() % 20 {
                        0..=7 => d[at] ^= 1 << (rand() % 8),
                        8..=13 => d[at] = rand() as u8,
                        14..=16 => {
                            let chunk = d[at..at + len].to_vec();
                            let to = rand() as usize % d.len();
                            d.splice(to..to, chunk);
                        }
                        17 | 18 => {
                            d.drain(at..at + len);
                        }
                        _ => d.truncate(at.max(1)),
                    }
                }
                let result = std::panic::catch_unwind(|| f(&d));
                produced += result.as_ref().map_or(0, |n| *n);
                if let Err(e) = result {
                    // Kept for a regression: `target/smoke-<target>`.
                    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../target")
                        .join(format!("smoke-{target}"));
                    std::fs::write(&out, &d).expect("write the failing input");
                    std::panic::resume_unwind(e);
                }
            }
            eprintln!(
                "{target}: {n} inputs, {produced} out, {:?}",
                started.elapsed()
            );
        }
    }

    /// Reads a fixture: RTP datagrams in `[channel, u16 length, bytes]`.
    fn fixture(name: &str) -> Vec<Vec<u8>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let bytes = std::fs::read(path).expect("fixture");
        let mut out = Vec::new();
        let mut rest = &bytes[..];
        while let Some((_, tail)) = rest.split_first() {
            let len = usize::from(u16::from_be_bytes([tail[0], tail[1]]));
            out.push(tail[2..2 + len].to_vec());
            rest = &tail[2 + len..];
        }
        out
    }

    /// Writes the committed feedback seeds from the committed fixtures:
    /// `cargo test -p sapflow-rtp --lib -- --ignored write_feedback_seeds`.
    #[test]
    #[ignore = "writes fuzz/seeds from tests/fixtures"]
    fn write_feedback_seeds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
        let write = |target: &str, name: &str, bytes: &[u8]| {
            let d = root.join(target);
            std::fs::create_dir_all(&d).expect("dir");
            assert!(bytes.len() <= 65_536, "{name} within CI's -max_len");
            let used = records(bytes.get(1..).unwrap_or_default()).count();
            assert!(
                used < super::super::MAX_RECORDS,
                "{name}: every record is read"
            );
            std::fs::write(d.join(name), bytes).expect("write");
        };
        let video = fixture("ffmpeg-h264-fu.rec");
        let audio = fixture("ffmpeg-opus-stereo.rec");
        let parsed = |d: &[u8]| RtpPacket::parse(d).expect("rtp").sequence();

        // rr_stats: the video capture's sequence numbers (across the wrap)
        // with drops, a reordering, copies, a probe, a restart and reports.
        let mut ops = vec![0u8];
        let packet = |ops: &mut Vec<u8>, seq: u16, how: u8, step: u16, ms: u8| {
            let mut rec = vec![0u8];
            rec.extend_from_slice(&seq.to_be_bytes());
            rec.push(how);
            rec.extend_from_slice(&step.to_be_bytes());
            rec.push(ms);
            push_record(ops, &rec);
        };
        let mut ntp = vec![1u8];
        ntp.extend_from_slice(&(NTP_ZERO + (5 << 30)).to_be_bytes());
        push_record(&mut ops, &ntp);
        for (n, d) in video.iter().enumerate() {
            let seq = parsed(d);
            if n % 17 == 5 {
                continue;
            }
            packet(
                &mut ops,
                seq,
                if n % 23 == 7 { 1 } else { 0 },
                if n % 3 == 0 { 6000 } else { 0 },
                7,
            );
            if n % 31 == 3 {
                packet(&mut ops, seq, 1, 0, 0);
            }
            if n % 50 == 49 {
                push_record(&mut ops, &[2]);
            }
        }
        packet(&mut ops, parsed(&video[5]), 1, 0, 30);
        push_record(&mut ops, &[2]);
        packet(&mut ops, 30_000, 2, 0, 1);
        packet(&mut ops, 30_001, 3, 0, 1);
        packet(&mut ops, 30_002, 0, 3000, 33);
        push_record(&mut ops, &[3, 0x10, 0]);
        push_record(&mut ops, &[2]);
        write("rr_stats", "ffmpeg-h264-drops-copies-restart", &ops);
        let mut ops = vec![1u8];
        for (n, d) in audio.iter().enumerate() {
            if n % 11 != 4 {
                packet(&mut ops, parsed(d), 0, 960, 20 + (n % 3) as u8);
            }
        }
        push_record(&mut ops, &[2]);
        write("rr_stats", "ffmpeg-opus-jitter", &ops);

        // twcc_feedback: the two captures interleaved as one transport,
        // at their send times, with losses, reordering, a copy, a long
        // pause and a far jump.
        let interleaved = |mode: u8| {
            let mut ops = vec![mode];
            let mut tseq = 100u16;
            let (mut vi, mut ai) = (video.iter(), audio.iter());
            // 110 packets, so the far jump and pause at the end are within
            // the 256 records read.
            for n in 0..110u16 {
                let d = if n % 3 == 2 { ai.next() } else { vi.next() };
                if d.is_none() {
                    break;
                }
                tseq = tseq.wrapping_add(1);
                if n % 29 == 11 {
                    continue;
                }
                if n % 37 == 5 {
                    push_record(
                        &mut ops,
                        &[
                            0,
                            (tseq.wrapping_add(1) >> 8) as u8,
                            tseq.wrapping_add(1) as u8,
                        ],
                    );
                    push_record(&mut ops, &[0, (tseq >> 8) as u8, tseq as u8]);
                    tseq = tseq.wrapping_add(1);
                } else {
                    push_record(&mut ops, &[0, (tseq >> 8) as u8, tseq as u8]);
                }
                if n % 41 == 0 {
                    push_record(&mut ops, &[0, (tseq >> 8) as u8, tseq as u8]);
                }
                push_record(&mut ops, &[2, 0, 30 + (n % 7) as u8]);
                if n % 12 == 0 {
                    push_record(&mut ops, &[4]);
                }
            }
            push_record(&mut ops, &[3, 9]);
            push_record(&mut ops, &[0, 0x80, 0x00]);
            push_record(&mut ops, &[1, 1, 2, 3]);
            push_record(&mut ops, &[4]);
            ops
        };
        write("twcc_feedback", "ffmpeg-interleaved", &interleaved(0));
        write("twcc_feedback", "ffmpeg-interleaved-small", &interleaved(3));

        // sr_timeline: the start of the captures with honest sender
        // reports, with skewed ones, and with none.
        let session = |mode: u8, reports: &[(usize, u8, i16)]| {
            let mut ops = vec![mode];
            let media = |op: u8, d: &[u8], advance: bool| {
                let p = RtpPacket::parse(d).expect("rtp");
                let mut rec = vec![op];
                rec.extend_from_slice(&p.sequence().to_be_bytes());
                rec.push(u8::from(p.marker()) | if advance { 2 } else { 0 });
                rec.extend_from_slice(&[0, 0]);
                rec.extend_from_slice(p.payload());
                rec
            };
            // Video frames 3000 ticks (33 ms) apart, audio 20 ms, both
            // on the clock.
            let mut last_ts = None;
            let mut a = audio.iter();
            let (mut video_ms, mut audio_ms) = (0u64, 0u64);
            // 130 packets, about half a second, to stay within 64 KiB.
            for (n, d) in video.iter().take(130).enumerate() {
                let p = RtpPacket::parse(d).expect("rtp");
                let advance = last_ts.is_some_and(|t| t != p.timestamp());
                if advance {
                    push_record(&mut ops, &[3, 0, 33]);
                    video_ms += 33;
                    while audio_ms + 20 <= video_ms {
                        audio_ms += 20;
                        if let Some(ad) = a.next() {
                            push_record(&mut ops, &media(1, ad, true));
                        }
                    }
                }
                last_ts = Some(p.timestamp());
                push_record(&mut ops, &media(0, d, advance));
                for &(at, which, skew) in reports {
                    if at == n {
                        let mut rec = vec![2, which];
                        rec.extend_from_slice(&skew.to_be_bytes());
                        push_record(&mut ops, &rec);
                    }
                }
            }
            push_record(&mut ops, &[3, 0x0b, 0xb8]);
            ops
        };
        write(
            "sr_timeline",
            "ffmpeg-honest-reports",
            &session(0, &[(3, 0, 0), (8, 1, 0), (100, 0, 0), (120, 1, 0)]),
        );
        write(
            "sr_timeline",
            "ffmpeg-skewed-reports",
            &session(2, &[(3, 0, 0), (8, 1, 0), (90, 0, 400), (110, 1, -300)]),
        );
        write("sr_timeline", "ffmpeg-no-reports-small", &session(4, &[]));
        let mut hostile = session(1, &[(2, 0, 0)]);
        let mut rec = vec![2u8, 3];
        rec.extend_from_slice(&u64::MAX.to_be_bytes());
        rec.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        push_record(&mut hostile, &rec);
        let mut rec = vec![2u8, 2];
        rec.extend_from_slice(&0u64.to_be_bytes());
        rec.extend_from_slice(&[0; 4]);
        push_record(&mut hostile, &rec);
        push_record(&mut hostile, &[0, 0, 9, 0x09, 0xff, 0xff, 0x41, 0x9a]);
        push_record(&mut hostile, &[0, 0, 10, 0x11, 0, 0, 0x41, 0x9a]);
        push_record(&mut hostile, &[4]);
        push_record(&mut hostile, &[5, 0, 1, 0, 0, 0, 0x41]);
        push_record(&mut hostile, &[3, 0, 200]);
        write("sr_timeline", "built-hostile-reports", &hostile);
    }
}
