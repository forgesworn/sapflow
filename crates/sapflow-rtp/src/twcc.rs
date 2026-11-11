//! Transport-wide congestion control feedback, receive side, from
//! draft-holmer-rmcat-transport-wide-cc-extensions-01 (sections 2.2 and
//! 3.1) with RFC 4585's RTPFB framing.
//!
//! The sender puts a 16-bit transport-wide sequence number in every packet
//! it sends on the transport (an RFC 8285 header extension whose ID comes
//! from `a=extmap` in the SDP answer), RTX and padding-only probes
//! included. sapflow records when each one arrived and, at a fixed cadence
//! ([`TransportCcConfig::interval`]), sends back RTPFB FMT 15 packets
//! listing every sequence number from where the last one ended, received
//! or not, with arrival deltas in 250 us units. The sender runs the rate
//! estimate; sapflow does none of its own.
//!
//! The draft is internally inconsistent in two places, resolved here:
//!
//! - Section 3.1's prose says payload type 206, but its figure and field
//!   description say RTPFB, 205. 205 is used, as RFC 4585 defines RTPFB.
//! - Section 3.1.4's prose says one-bit symbols are "packet received (0)
//!   and packet not received (1)", but both examples read 0 as not
//!   received and 1 as received, the low bit of the two-bit symbols. The
//!   examples are followed.
//!
//! Bounds, whatever the sender's sequence numbers:
//!
//! - A packet is recorded at most once: a window of the last 4096 sequence
//!   numbers remembers which arrived. Older packets, and copies, are
//!   counted and dropped.
//! - At most [`TransportCcConfig::max_pending`] packets wait for feedback;
//!   reaching it sends feedback at once.
//! - Every feedback packet is at most [`TransportCcConfig::max_packet`]
//!   bytes, split where needed: on size, on a status count over 16 bits,
//!   and where a receive delta does not fit 16 signed bits (the draft's
//!   "a new feedback message must be used").
//! - Encoding walks runs of statuses, not single statuses, so a gap of
//!   thousands of sequence numbers costs a few run-length chunks.
//!
//! A feedback starts where the previous one ended, so a loss across a
//! feedback boundary is reported as not received, unless that is more than
//! [`MAX_RUN`] sequence numbers back (one run-length chunk). A late packet
//! below that point starts the next feedback at itself: the base goes
//! back, and packets already reported as received are then reported as not
//! received, as section 3.1.1's note requires.

use crate::stats::{SeqWindow, WINDOW};
use std::time::Duration;

/// Longest run in one run-length chunk (13 bits).
pub const MAX_RUN: u32 = 8191;
/// Receive delta unit, microseconds.
pub const DELTA_US: u64 = 250;
/// Reference time unit, microseconds (64 ms, 256 delta units).
pub const REFERENCE_US: u64 = 64_000;
/// Bytes before the first packet chunk: RTCP header, both SSRCs, base
/// sequence number, status count, reference time and feedback count.
pub const HEADER_LEN: usize = 20;
/// The smallest [`TransportCcConfig::max_packet`] accepted.
pub const MIN_PACKET: usize = 64;

/// Where extended sequence numbers start, as in the reorder buffer.
const BASE: u64 = 1 << 32;

/// Transport-cc settings, from the SDP answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCcConfig {
    /// The `a=extmap` ID of the transport-wide sequence number extension:
    /// 1 to 14 in the one-byte form, up to 255 in the two-byte form.
    pub extension_id: u8,
    /// Feedback cadence. Default 50 ms (`docs/whip-ingest.md`, proposed).
    pub interval: Duration,
    /// Largest feedback packet in bytes, RTCP header included; longer
    /// feedback is split. At least [`MIN_PACKET`]. Default 1000, so that a
    /// compound with receiver reports and an SDES CNAME still fits a
    /// 1200-byte datagram.
    pub max_packet: usize,
    /// Most packets awaiting feedback. Reaching it sends feedback at once.
    /// Default 1024 (at 50 ms, about 160 Mbit/s of 1200-byte packets).
    pub max_pending: usize,
}
impl TransportCcConfig {
    /// The defaults, with the negotiated extension ID.
    pub fn new(extension_id: u8) -> Self {
        Self {
            extension_id,
            interval: Duration::from_millis(50),
            max_packet: 1000,
            max_pending: 1024,
        }
    }
}

/// Transport-cc counters for a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportCcStats {
    /// Packets recorded.
    pub packets: u64,
    /// Copies of a sequence number already recorded, dropped.
    pub duplicates: u64,
    /// Packets more than 4096 behind the highest, dropped.
    pub too_old: u64,
    /// Extensions with the configured ID that are not two bytes.
    pub malformed: u64,
    /// Feedback packets built.
    pub feedback: u64,
    /// Feedback packets dropped because the feedback queue was full.
    pub dropped: u64,
}

/// One feedback packet, ready for [`crate::rtcp::build::transport_cc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Built {
    /// Everything after the two SSRCs, padded to whole words.
    pub fci: Vec<u8>,
}

/// The receive side's record of arrivals.
#[derive(Debug)]
pub(crate) struct Recorder {
    cfg: TransportCcConfig,
    interval: u64,
    highest: Option<u64>,
    seen: SeqWindow,
    /// `(extended sequence number, arrival in microseconds)`, unsorted.
    pending: Vec<(u64, u64)>,
    /// One after the highest sequence number reported.
    next_base: Option<u64>,
    fb_count: u8,
    last_flush: u64,
    stats: TransportCcStats,
}

impl Recorder {
    pub fn new(cfg: TransportCcConfig) -> Self {
        let cfg = TransportCcConfig {
            max_packet: cfg.max_packet.max(MIN_PACKET),
            max_pending: cfg.max_pending.max(1),
            ..cfg
        };
        Self {
            cfg,
            interval: u64::try_from(cfg.interval.as_micros()).unwrap_or(u64::MAX),
            highest: None,
            seen: SeqWindow::new(),
            pending: Vec::new(),
            next_base: None,
            fb_count: 0,
            last_flush: 0,
            stats: TransportCcStats::default(),
        }
    }

    pub fn extension_id(&self) -> u8 {
        self.cfg.extension_id
    }

    pub fn stats(&self) -> TransportCcStats {
        self.stats
    }

    pub fn note_dropped(&mut self, n: u64) {
        self.stats.dropped += n;
    }

    /// Packets awaiting feedback.
    #[cfg(any(test, feature = "testing"))]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// The configuration as normalised.
    #[cfg(any(test, feature = "testing"))]
    pub fn config(&self) -> TransportCcConfig {
        self.cfg
    }

    /// Records the extension element of a packet that arrived at `now`
    /// (microseconds). Returns true when the pending list is full and
    /// feedback should be built at once.
    pub fn record(&mut self, now: u64, element: &[u8]) -> bool {
        let Ok(bytes) = <[u8; 2]>::try_from(element) else {
            self.stats.malformed += 1;
            return false;
        };
        let seq = u16::from_be_bytes(bytes);
        let ext = match self.highest {
            None => {
                let ext = BASE + u64::from(seq);
                self.highest = Some(ext);
                ext
            }
            Some(h) => {
                let ext = h.saturating_add_signed(i64::from(seq.wrapping_sub(h as u16) as i16));
                if ext > h {
                    self.seen.advance(h, ext);
                    self.highest = Some(ext);
                } else if h - ext >= WINDOW {
                    self.stats.too_old += 1;
                    return false;
                } else if self.seen.is_seen(ext) {
                    self.stats.duplicates += 1;
                    return false;
                }
                ext
            }
        };
        self.seen.mark(ext);
        self.pending.push((ext, now));
        self.stats.packets += 1;
        self.pending.len() >= self.cfg.max_pending
    }

    /// When feedback is next due, if anything is waiting.
    pub fn due(&self) -> Option<u64> {
        (!self.pending.is_empty()).then(|| self.last_flush.saturating_add(self.interval))
    }

    /// Builds feedback for everything pending, at `now`, in packets of at
    /// most `room` bytes as well as [`TransportCcConfig::max_packet`] (and
    /// at least [`MIN_PACKET`]).
    pub fn flush(&mut self, now: u64, room: usize) -> Vec<Built> {
        let max_packet = self.cfg.max_packet.min(room).max(MIN_PACKET);
        self.last_flush = now;
        let mut pending = std::mem::take(&mut self.pending);
        pending.sort_unstable_by_key(|p| p.0);
        let mut out = Vec::new();
        let mut rest = &pending[..];
        while let Some(&(next, _)) = rest.first() {
            let base = match self.next_base {
                Some(nb) if nb <= next && next - nb <= u64::from(MAX_RUN) => nb,
                _ => next,
            };
            let (fci, used, last) = encode(base, rest, self.fb_count, max_packet);
            out.push(Built { fci });
            self.stats.feedback += 1;
            self.fb_count = self.fb_count.wrapping_add(1);
            self.next_base = Some(self.next_base.unwrap_or(0).max(last + 1));
            rest = rest.get(used..).unwrap_or_default();
        }
        // Keep the allocation for the next interval.
        pending.clear();
        self.pending = pending;
        out
    }
}

/// Appends `n` of `symbol` to a run list, merging with the last run.
fn push_run(runs: &mut Vec<(u8, u32)>, symbol: u8, n: u64) {
    let mut n = n;
    while n > 0 {
        if let Some((s, c)) = runs.last_mut()
            && *s == symbol
            && *c < u32::MAX
        {
            let add = n.min(u64::from(u32::MAX - *c));
            *c += add as u32;
            n -= add;
        } else {
            let add = n.min(u64::from(u32::MAX));
            runs.push((symbol, add as u32));
            n -= add;
        }
    }
}

/// Encodes one feedback packet from `base`, taking packets from the front
/// of `packets` (sorted, distinct, the first at most [`MAX_RUN`] after
/// `base`) while they fit. Returns the FCI, how many packets it took (at
/// least one) and the last one's extended sequence number.
fn encode(
    base: u64,
    packets: &[(u64, u64)],
    fb_count: u8,
    max_packet: usize,
) -> (Vec<u8>, usize, u64) {
    let first_q = packets.first().map_or(0, |p| p.1 / DELTA_US);
    let reference = first_q / 256;
    let mut prev_q = reference * 256;
    let mut runs: Vec<(u8, u32)> = Vec::new();
    let mut deltas: Vec<u8> = Vec::new();
    // An upper bound on the size: every received packet costs its delta
    // and at most one chunk, every gap at most one run-length chunk per
    // MAX_RUN, plus up to three bytes of padding (see `encode_chunks`).
    let mut size = HEADER_LEN + 3;
    let mut covered = base;
    let mut used = 0usize;
    let mut last = base;
    for &(ext, arrival) in packets {
        let q = arrival / DELTA_US;
        let delta = i128::from(q) - i128::from(prev_q);
        let small = (0..=255).contains(&delta);
        let fits = small || i16::try_from(delta).is_ok();
        let span = ext + 1 - base;
        let gap = ext - covered;
        let cost = 2 * (1 + gap.div_ceil(u64::from(MAX_RUN))) as usize + if small { 1 } else { 2 };
        if used > 0 && (!fits || span > u64::from(u16::MAX) || size + cost > max_packet) {
            break;
        }
        size += cost;
        push_run(&mut runs, 0, gap);
        push_run(&mut runs, if small { 1 } else { 2 }, 1);
        if small {
            deltas.push(delta as u8);
        } else {
            deltas.extend_from_slice(&(delta as i16).to_be_bytes());
        }
        prev_q = q;
        covered = ext + 1;
        last = ext;
        used += 1;
    }
    let mut fci = Vec::with_capacity(size - 12);
    fci.extend_from_slice(&(base as u16).to_be_bytes());
    fci.extend_from_slice(&((last + 1 - base) as u16).to_be_bytes());
    // 24 bits, signed in the draft, in a time base of the receiver's
    // choosing. sapflow's starts at zero when the receiver is made and
    // counts up, written as its low 24 bits: two's complement, so a signed
    // reader sees it pass from 2^23 - 1 to -2^23 after about 6.2 days and
    // keep counting up from there, one step per 64 ms, as a wrapping
    // counter does. Only differences between reference times mean anything
    // to the sender.
    let reference = (reference & 0x00ff_ffff) as u32;
    fci.extend_from_slice(&(reference << 8 | u32::from(fb_count)).to_be_bytes());
    encode_chunks(&runs, &mut fci);
    fci.extend_from_slice(&deltas);
    fci.resize(fci.len().next_multiple_of(4), 0);
    (fci, used, last)
}

/// A position in a run list.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    run: usize,
    offset: u32,
}

fn symbol_at(runs: &[(u8, u32)], at: Cursor) -> Option<(u8, u32)> {
    runs.get(at.run).map(|&(s, c)| (s, c - at.offset))
}

fn advance(runs: &[(u8, u32)], mut at: Cursor, mut n: u32) -> Cursor {
    while n > 0 {
        let Some(&(_, c)) = runs.get(at.run) else {
            break;
        };
        let left = c - at.offset;
        if n < left {
            at.offset += n;
            return at;
        }
        n -= left;
        at = Cursor {
            run: at.run + 1,
            offset: 0,
        };
    }
    at
}

/// The next up to 14 symbols from `at`.
fn lookahead(runs: &[(u8, u32)], at: Cursor) -> ([u8; 14], usize) {
    let mut out = [0u8; 14];
    let mut n = 0;
    let mut pos = at;
    while n < 14 {
        let Some((s, left)) = symbol_at(runs, pos) else {
            break;
        };
        let take = (left as usize).min(14 - n);
        for slot in out.iter_mut().skip(n).take(take) {
            *slot = s;
        }
        n += take;
        pos = advance(runs, pos, take as u32);
    }
    (out, n)
}

/// Encodes a run list of statuses (0 not received, 1 small delta, 2 large
/// delta) as packet status chunks (section 3.1.2), appending big-endian
/// 16-bit chunks to `out`. Greedy, one chunk per step:
///
/// - 14 or more of one symbol: a run-length chunk of up to [`MAX_RUN`];
/// - otherwise, if the next 14 symbols (or all that are left) are only 0
///   and 1: a one-bit status vector;
/// - otherwise 7 or more of one symbol: a run-length chunk;
/// - otherwise a two-bit status vector of the next 7.
///
/// A vector at the end may run past the last status; its spare symbols are
/// 0, and the status count says where the list ends. Every vector holds at
/// least one received status, so the chunks number at most the received
/// packets plus one run-length chunk per [`MAX_RUN`] of each gap.
pub(crate) fn encode_chunks(runs: &[(u8, u32)], out: &mut Vec<u8>) {
    let mut at = Cursor { run: 0, offset: 0 };
    while let Some((symbol, left)) = symbol_at(runs, at) {
        let (next, n) = lookahead(runs, at);
        let (chunk, took) = if left >= 14 {
            let take = left.min(MAX_RUN);
            (u16::from(symbol) << 13 | take as u16, take)
        } else if next.iter().take(n).all(|&s| s <= 1) {
            let mut bits = 0u16;
            for (i, &s) in next.iter().take(n).enumerate() {
                bits |= u16::from(s & 1) << (13 - i);
            }
            (0x8000 | bits, n as u32)
        } else if left >= 7 {
            (u16::from(symbol) << 13 | left as u16, left)
        } else {
            let k = n.min(7);
            let mut bits = 0u16;
            for (i, &s) in next.iter().take(k).enumerate() {
                bits |= u16::from(s & 3) << (12 - 2 * i);
            }
            (0xc000 | bits, k as u32)
        };
        out.extend_from_slice(&chunk.to_be_bytes());
        at = advance(runs, at, took);
    }
}

/// A reader for the feedback sapflow builds, for tests and fuzzing only:
/// it accepts exactly what [`encode`] can produce, and so is stricter than
/// the draft.
#[cfg(any(test, feature = "testing"))]
pub mod parse {
    use crate::rtp::{be16, be32};

    /// One status, in sequence order from the base.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        /// Not received.
        NotReceived,
        /// Received, with an 8-bit delta in 250 us units.
        Small(u8),
        /// Received, with a 16-bit signed delta.
        Large(i16),
    }

    /// One feedback packet's FCI, read back.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Feedback {
        /// Base sequence number.
        pub base: u16,
        /// Reference time, 24 bits, in 64 ms units.
        pub reference: u32,
        /// Feedback packet count.
        pub fb_count: u8,
        /// The chunks as they were on the wire.
        pub chunks: Vec<u16>,
        /// One status per sequence number from the base.
        pub statuses: Vec<Status>,
    }

    /// Why FCI did not read back.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ParseError {
        /// Shorter than the fixed fields, chunks or deltas need.
        Truncated,
        /// A status count of zero, a run of zero, or the reserved symbol.
        Chunk,
        /// Spare vector symbols that are not 0, bytes after the deltas
        /// beyond the padding, padding that is not zero, or FCI that is
        /// not whole words.
        Trailing,
    }

    /// The symbols a chunk holds: a run-length chunk's `(symbol, length)`,
    /// or a vector's symbols in order.
    pub fn chunk_symbols(chunk: u16) -> Result<(u8, u32), Vec<u8>> {
        if chunk & 0x8000 == 0 {
            return Ok((((chunk >> 13) & 3) as u8, u32::from(chunk & 0x1fff)));
        }
        Err(if chunk & 0x4000 == 0 {
            (0..14).map(|i| ((chunk >> (13 - i)) & 1) as u8).collect()
        } else {
            (0..7)
                .map(|i| ((chunk >> (12 - 2 * i)) & 3) as u8)
                .collect()
        })
    }

    /// Reads FCI as [`crate::rtcp::build::transport_cc`] carries it.
    pub fn parse_fci(fci: &[u8]) -> Result<Feedback, ParseError> {
        let base = be16(fci, 0).ok_or(ParseError::Truncated)?;
        let count = usize::from(be16(fci, 2).ok_or(ParseError::Truncated)?);
        let word = be32(fci, 4).ok_or(ParseError::Truncated)?;
        if count == 0 {
            return Err(ParseError::Chunk);
        }
        let mut symbols: Vec<u8> = Vec::with_capacity(count);
        let mut chunks = Vec::new();
        let mut pos = 8;
        while symbols.len() < count {
            let chunk = be16(fci, pos).ok_or(ParseError::Truncated)?;
            pos += 2;
            chunks.push(chunk);
            match chunk_symbols(chunk) {
                Ok((_, 0)) => return Err(ParseError::Chunk),
                Ok((s, n)) => {
                    let n = (n as usize).min(count - symbols.len());
                    symbols.extend(std::iter::repeat_n(s, n));
                }
                Err(list) => {
                    let room = count - symbols.len();
                    if list.iter().skip(room).any(|&s| s != 0) {
                        return Err(ParseError::Trailing);
                    }
                    symbols.extend(list.into_iter().take(room));
                }
            }
        }
        let mut statuses = Vec::with_capacity(count);
        for s in symbols {
            statuses.push(match s {
                0 => Status::NotReceived,
                1 => {
                    let d = *fci.get(pos).ok_or(ParseError::Truncated)?;
                    pos += 1;
                    Status::Small(d)
                }
                2 => {
                    let d = be16(fci, pos).ok_or(ParseError::Truncated)? as i16;
                    pos += 2;
                    Status::Large(d)
                }
                _ => return Err(ParseError::Chunk),
            });
        }
        let rest = fci.get(pos..).unwrap_or_default();
        if !fci.len().is_multiple_of(4) || rest.len() > 3 || rest.iter().any(|&b| b != 0) {
            return Err(ParseError::Trailing);
        }
        Ok(Feedback {
            base,
            reference: word >> 8,
            fb_count: word as u8,
            chunks,
            statuses,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse::{Status, chunk_symbols, parse_fci};
    use super::*;

    const MS: u64 = 1000;

    fn element(seq: u16) -> [u8; 2] {
        seq.to_be_bytes()
    }

    /// Feeds `(seq, arrival us)` and flushes at `now`.
    fn feed(r: &mut Recorder, packets: &[(u16, u64)], now: u64) -> Vec<super::parse::Feedback> {
        for &(seq, at) in packets {
            r.record(at, &element(seq));
        }
        r.flush(now, usize::MAX)
            .into_iter()
            .map(|b| {
                assert!(b.fci.len() + 12 <= r.config().max_packet);
                parse_fci(&b.fci).expect("reads back")
            })
            .collect()
    }

    /// Arrival times reconstructed from a feedback, in 250 us units, by
    /// sequence number; `None` where not received.
    fn arrivals(f: &super::parse::Feedback, reference: u64) -> Vec<(u16, Option<i64>)> {
        let mut t = (reference * 256) as i64;
        f.statuses
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let seq = f.base.wrapping_add(i as u16);
                match *s {
                    Status::NotReceived => (seq, None),
                    Status::Small(d) => {
                        t += i64::from(d);
                        (seq, Some(t))
                    }
                    Status::Large(d) => {
                        t += i64::from(d);
                        (seq, Some(t))
                    }
                }
            })
            .collect()
    }

    /// The draft's chunk examples (sections 3.1.3 and 3.1.4).
    #[test]
    fn draft_chunk_examples() {
        // Run-length example 1: "not received", length 221.
        assert_eq!(chunk_symbols(0b0000_0000_1101_1101), Ok((0, 221)));
        let mut out = Vec::new();
        encode_chunks(&[(0, 221)], &mut out);
        assert_eq!(out, [0x00, 0xdd]);
        // Run-length example 2: symbol 11, length 24 (decoded only; the
        // symbol is reserved and sapflow never sends it).
        assert_eq!(chunk_symbols(0b0110_0000_0001_1000), Ok((3, 24)));
        // Vector example 1: 1 not received, 5 received, 3 not, 3
        // received, 2 not, in one-bit symbols.
        let ex1 = 0b1001_1111_0001_1100u16;
        let symbols = [0, 1, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 0, 0];
        assert_eq!(chunk_symbols(ex1), Err(symbols.to_vec()));
        let mut runs = Vec::new();
        for &s in &symbols {
            push_run(&mut runs, s, 1);
        }
        let mut out = Vec::new();
        encode_chunks(&runs, &mut out);
        assert_eq!(out, ex1.to_be_bytes());
        // Vector example 2, two-bit symbols: not received, the reserved
        // 11, three received, two not received.
        assert_eq!(
            chunk_symbols(0b1100_1101_0101_0000),
            Err(vec![0, 3, 1, 1, 1, 0, 0])
        );
    }

    /// One packet assembled by hand from the section 3.1 figure, byte for
    /// byte: 100 received 0.25 ms after the 64 ms reference, 101 not
    /// received, 102 received 100 ms after 100 (a large delta).
    #[test]
    fn a_whole_packet_matches_the_figure() {
        let mut r = Recorder::new(TransportCcConfig::new(3));
        r.record(64_250, &element(100));
        r.record(164_250, &element(102));
        let built = r.flush(200 * MS, usize::MAX);
        let mut out = Vec::new();
        crate::rtcp::build::transport_cc(&mut out, 0x0102_0304, 0x0a0b_0c0d, &built[0].fci)
            .expect("builds");
        #[rustfmt::skip]
        let want = [
            // V=2, P=0, FMT=15; PT=205; length 6 (28 bytes).
            0x8f, 0xcd, 0x00, 0x06,
            // SSRC of packet sender, SSRC of media source.
            0x01, 0x02, 0x03, 0x04,
            0x0a, 0x0b, 0x0c, 0x0d,
            // Base sequence number 100, packet status count 3.
            0x00, 0x64, 0x00, 0x03,
            // Reference time 1 (64 ms), feedback packet count 0.
            0x00, 0x00, 0x01, 0x00,
            // A two-bit status vector: 01 (small), 00 (not received), 10
            // (large), four spare 00; then the deltas 1 and 400.
            0xd2, 0x00, 0x01, 0x01,
            0x90, 0x00, 0x00, 0x00,
        ];
        assert_eq!(out, want);
    }

    #[test]
    fn run_length_versus_vectors() {
        let chunks = |runs: &[(u8, u32)]| {
            let mut out = Vec::new();
            encode_chunks(runs, &mut out);
            out.chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect::<Vec<_>>()
        };
        // 20 received with small deltas: one run.
        assert_eq!(chunks(&[(1, 20)]), [0x2000 | 20]);
        // 10000 not received then one received: a full run, a run of the
        // rest, then a one-bit vector for the tail.
        assert_eq!(
            chunks(&[(1, 1), (0, 10_000), (1, 1)]),
            [0x8000 | 1 << 13, 8191, 10_000 - 8191 - 13, 0x8000 | 1 << 13]
        );
        // Mixed small and large: two-bit vectors of seven.
        assert_eq!(
            chunks(&[
                (1, 1),
                (2, 1),
                (1, 1),
                (0, 1),
                (2, 1),
                (1, 1),
                (1, 1),
                (2, 1)
            ]),
            [0xc000 | 0b01_10_01_00_10_01_01, 0xc000 | 0b10 << 12]
        );
        // Eight large: a run beats two vectors.
        assert_eq!(chunks(&[(2, 8)]), [0x4000 | 8]);
        // Five large, then small: a two-bit vector, then a run.
        assert_eq!(
            chunks(&[(2, 5), (1, 20)]),
            [0xc000 | 0b10_10_10_10_10_01_01, 0x2000 | 18]
        );
    }

    #[test]
    fn a_loss_across_two_feedbacks_is_reported() {
        let mut r = Recorder::new(TransportCcConfig::new(3));
        let one: Vec<_> = (1u16..=10).map(|s| (s, u64::from(s) * MS)).collect();
        let f = feed(&mut r, &one, 20 * MS);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].base, f[0].statuses.len(), f[0].fb_count), (1, 10, 0));
        let two: Vec<_> = (12u16..=20).map(|s| (s, u64::from(s) * MS)).collect();
        let f = feed(&mut r, &two, 40 * MS);
        assert_eq!((f[0].base, f[0].statuses.len(), f[0].fb_count), (11, 10, 1));
        assert_eq!(
            f[0].statuses[0],
            Status::NotReceived,
            "11 was never received"
        );
        let got = arrivals(&f[0], 12 * MS / REFERENCE_US);
        assert_eq!(got[1], (12, Some(12 * MS as i64 / 250)));
        assert_eq!(got[9], (20, Some(20 * MS as i64 / 250)));
    }

    #[test]
    fn deltas_are_exact_large_and_negative() {
        let mut r = Recorder::new(TransportCcConfig::new(3));
        // 100 ms apart (400 units, large); then one that arrived before its
        // predecessor in sequence order (negative, large); then 1 ms.
        let packets = [
            (1u16, 1_000_123),
            (2, 1_100_123),
            (3, 1_050_000),
            (4, 1_051_000),
        ];
        let f = feed(&mut r, &packets, 2_000 * MS);
        assert_eq!(f.len(), 1);
        let reference = 1_000_123 / REFERENCE_US;
        assert_eq!(u64::from(f[0].reference), reference);
        let q: Vec<_> = packets.iter().map(|p| Some((p.1 / 250) as i64)).collect();
        let got: Vec<_> = arrivals(&f[0], reference)
            .into_iter()
            .map(|a| a.1)
            .collect();
        assert_eq!(got, q, "no rounding drift");
        assert!(matches!(f[0].statuses[0], Status::Small(_)));
        assert_eq!(f[0].statuses[1], Status::Large(400));
        assert_eq!(f[0].statuses[2], Status::Large(-200));
        assert_eq!(f[0].statuses[3], Status::Small(4));
    }

    #[test]
    fn a_delta_past_16_bits_splits() {
        let mut r = Recorder::new(TransportCcConfig::new(3));
        // 9 s between two packets in sequence order: past 8191.75 ms.
        let f = feed(
            &mut r,
            &[(1, 0), (2, 9_000 * MS), (3, 9_001 * MS)],
            9_100 * MS,
        );
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].base, f[0].statuses.len()), (1, 1));
        assert_eq!((f[1].base, f[1].statuses.len()), (2, 2));
        assert_eq!((f[0].fb_count, f[1].fb_count), (0, 1));
        assert_eq!(u64::from(f[1].reference), 9_000 * MS / REFERENCE_US);
    }

    #[test]
    fn the_reference_time_wraps_at_24_bits() {
        let mut r = Recorder::new(TransportCcConfig::new(3));
        let wrap = (1u64 << 24) * REFERENCE_US;
        let f = feed(&mut r, &[(1, wrap - 10 * MS)], wrap);
        assert_eq!(f[0].reference, 0xff_ffff);
        let f = feed(&mut r, &[(2, wrap + 70 * MS)], wrap + 100 * MS);
        assert_eq!(f[0].reference, 1, "64 ms after the wrap");
        assert_eq!(f[0].statuses, [Status::Small(24)], "70 - 64 = 6 ms");
    }

    /// Read as the draft's signed 24-bit field, the reference time steps by
    /// one 64 ms unit across the signed limit (about 6.2 days in) as
    /// anywhere else.
    #[test]
    fn the_reference_time_reads_as_signed_across_its_limit() {
        let signed = |r: u32| ((r << 8) as i32) >> 8;
        let mut r = Recorder::new(TransportCcConfig::new(3));
        let edge = (1u64 << 23) * REFERENCE_US;
        let a = feed(&mut r, &[(1, edge - REFERENCE_US)], edge);
        let b = feed(&mut r, &[(2, edge)], edge + 10 * MS);
        assert_eq!(
            (signed(a[0].reference), signed(b[0].reference)),
            ((1 << 23) - 1, -(1 << 23))
        );
        // The sender's difference, modulo 2^24, is one unit.
        assert_eq!(b[0].reference.wrapping_sub(a[0].reference) & 0xff_ffff, 1);
    }

    #[test]
    fn sequence_wrap_reordering_and_duplicates() {
        let mut r = Recorder::new(TransportCcConfig::new(3));
        let f = feed(
            &mut r,
            &[
                (65_534, 0),
                (0, MS),
                (65_535, 2 * MS),
                (0, 3 * MS),
                (1, 4 * MS),
            ],
            10 * MS,
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].base, 65_534);
        assert_eq!(f[0].statuses.len(), 4, "the copy of 0 is not reported");
        assert_eq!(r.stats().duplicates, 1);
        assert_eq!(f[0].statuses[1], Status::Small(8));
        assert_eq!(
            f[0].statuses[2],
            Status::Large(-4),
            "0 arrived before 65535"
        );
        // A late packet behind what was reported: the base goes back.
        let f = feed(&mut r, &[(2, 11 * MS), (65_533, 12 * MS)], 20 * MS);
        assert_eq!(f[0].base, 65_533);
        assert_eq!(f[0].statuses.len(), 6);
        assert!(matches!(f[0].statuses[0], Status::Small(_)));
        // Already reported as received: now not received.
        assert!(
            f[0].statuses[1..5]
                .iter()
                .all(|s| *s == Status::NotReceived)
        );
        assert!(matches!(f[0].statuses[5], Status::Large(_)), "2 came first");
        // The same late packet again, and one older than the window.
        let f = feed(
            &mut r,
            &[(65_533, 21 * MS), (2u16.wrapping_sub(5000), 21 * MS)],
            30 * MS,
        );
        assert!(f.is_empty());
        assert_eq!((r.stats().duplicates, r.stats().too_old), (2, 1));
    }

    #[test]
    fn far_jumps_keep_feedback_flowing() {
        let mut r = Recorder::new(TransportCcConfig::new(3));
        feed(&mut r, &[(100, 0)], 10 * MS);
        // 30000 ahead: further than one run back, so the feedback starts
        // at the new packet rather than with a long run.
        let f = feed(&mut r, &[(30_100, 20 * MS), (30_101, 21 * MS)], 30 * MS);
        assert_eq!((f[0].base, f[0].statuses.len()), (30_100, 2));
        // 8000 ahead: one run of "not received" from where it ended.
        let f = feed(&mut r, &[(38_102, 40 * MS)], 50 * MS);
        assert_eq!((f[0].base, f[0].statuses.len()), (30_102, 8001));
        assert_eq!(f[0].chunks.len(), 2);
        // Alternate packets 30000 apart: every packet is still reported.
        let mut seq = 38_102u16;
        let mut reported = 0;
        for n in 0..200u64 {
            seq = seq.wrapping_add(30_000);
            let f = feed(&mut r, &[(seq, (60 + n) * MS)], (60 + n) * MS);
            reported += f.len();
            assert!(r.pending() == 0);
        }
        assert_eq!(reported, 200);
    }

    #[test]
    fn feedback_splits_at_the_size_cap() {
        let cfg = TransportCcConfig {
            max_packet: 64,
            ..TransportCcConfig::new(3)
        };
        let mut r = Recorder::new(cfg);
        // Alternating received and lost, deltas large: worst case.
        let packets: Vec<_> = (0..300u16)
            .map(|n| (n * 2, u64::from(n) * 100 * MS))
            .collect();
        for &(seq, at) in &packets {
            r.record(at, &element(seq));
        }
        let built = r.flush(30_000 * MS, usize::MAX);
        assert!(built.len() > 1);
        let mut next = 0u16;
        let mut seen = 0;
        for (n, b) in built.iter().enumerate() {
            assert!(b.fci.len() + 12 <= 64, "{} bytes", b.fci.len() + 12);
            let f = parse_fci(&b.fci).expect("reads back");
            assert_eq!(f.fb_count, n as u8);
            assert_eq!(f.base, next, "each starts where the last ended");
            next = f.base.wrapping_add(f.statuses.len() as u16);
            seen += f
                .statuses
                .iter()
                .filter(|s| **s != Status::NotReceived)
                .count();
        }
        assert_eq!(seen, 300);
    }

    #[test]
    fn pending_is_bounded_and_extensions_checked() {
        let cfg = TransportCcConfig {
            max_pending: 4,
            ..TransportCcConfig::new(3)
        };
        let mut r = Recorder::new(cfg);
        assert!(!r.record(0, &element(1)));
        assert!(!r.record(0, &[1, 2, 3]));
        assert!(!r.record(0, &[]));
        assert_eq!(r.stats().malformed, 2);
        assert!(!r.record(0, &element(2)));
        assert!(!r.record(0, &element(3)));
        assert!(r.record(0, &element(4)), "full");
        assert_eq!(r.due(), Some(50 * MS));
        assert_eq!(r.flush(1, usize::MAX).len(), 1);
        assert_eq!(r.due(), None);
    }
}
