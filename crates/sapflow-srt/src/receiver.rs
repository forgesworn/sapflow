//! One connection's receiving side, after the handshake: the receive
//! buffer, timestamp-based delivery (TSBPD) with too-late packet drop, ACKs,
//! NAKs, RTT, keepalives and the idle timeout.
//!
//! A gap is reported in a NAK as soon as it is seen, as libsrt does, until
//! the path is seen to reorder: a missing packet that then arrives as an
//! original rather than a retransmission. From then on the first NAK of each
//! new gap is held for a little longer than the latest such packet has come
//! since the path last went a second without reordering (at most
//! [`Settings::reorder_wait_max`] and an eighth of the latency, never into
//! the last three round trips before the gap's deadline), so a packet that
//! is only late is not resent. The wait halves after every second with no
//! reordering, back to none. It adapts only for a caller that
//! flags its retransmissions (`REXMITFLG`), since otherwise a retransmission
//! that fills a gap looks like a late original.
//!
//! Times are microseconds on the listener's clock (`i64`, never negative in
//! practice); packet timestamps are extended to 64 bits so nothing here
//! wraps except the sequence numbers, which are compared circularly.

use std::collections::VecDeque;

use crate::crypto::Decryptor;
use crate::loss::{LossList, Timing};
use crate::packet::{
    Ack, AckStats, Control, ControlPacket, DataPacket, HEADER_LEN, MAX_LOSS_ENTRIES,
};
use crate::rtt::Rtt;
use crate::seq::{Seq, TimestampExtender};

/// Full ACKs go out at most this often (the specification's SYN interval).
pub const ACK_INTERVAL: i64 = 10_000;
/// A light ACK after this many packets with no ACK.
pub const LIGHT_ACK_PACKETS: u32 = 64;
/// A keepalive when nothing has been sent for this long.
pub const KEEPALIVE_INTERVAL: i64 = 1_000_000;
/// Packet arrivals a drift estimate is taken over.
const DRIFT_WINDOW: u32 = 1000;
/// The most one drift estimate may move delivery times.
const DRIFT_STEP: i64 = 5_000;
/// Drift is never corrected by more than this in all.
const DRIFT_MAX: i64 = 500_000;
/// A packet is never held longer than its latency plus this, whatever its
/// timestamp claims.
const MAX_HOLD_BEYOND_LATENCY: i64 = 1_000_000;
/// Unanswered full ACKs remembered for RTT measurement.
const ACKS_REMEMBERED: usize = 64;
/// Consecutive-original arrival gaps used for the link-capacity median.
const PACKET_PAIR_GAPS: usize = 16;
/// The reorder wait halves after this long with no reordered packet...
const REORDER_DECAY: i64 = 1_000_000;
/// ...down to none below this.
const REORDER_FLOOR: i64 = 1_000;
/// The wait covers the latest reordered packet's lateness, a quarter more,
/// and this.
const REORDER_MARGIN: i64 = 1_000;

/// IPv6 and UDP headers. IPv4 and UDP take 28, so a datagram that fits with
/// 48 fits either; the negotiated MTU counts both headers.
const IP_UDP_OVERHEAD: usize = 48;
/// A negotiated MTU is clamped to this range when sizing a NAK: the
/// largest the listener accepts, and the smallest datagram every IPv4 host
/// must take, so a bogus handshake value cannot split a NAK into a flood.
const NAK_MTU_RANGE: (u32, u32) = (576, 1500);

/// The most bytes of loss list one NAK may carry on a path of `mtu` bytes
/// (IP and UDP headers included): what is left of the datagram after the IP,
/// UDP and SRT headers. 1436 at 1500.
fn nak_list_budget(mtu: u32) -> usize {
    let mtu = mtu.clamp(NAK_MTU_RANGE.0, NAK_MTU_RANGE.1) as usize;
    mtu - IP_UDP_OVERHEAD - HEADER_LEN
}

/// Splits `ranges` into runs, in order, whose loss-list encodings (4 bytes
/// for one number, 8 for a range) each take at most `budget` bytes. A budget
/// below 8 still carries one range per run, so the split always ends.
fn split_loss_list(ranges: &[(Seq, Seq)], budget: usize) -> impl Iterator<Item = &[(Seq, Seq)]> {
    let mut rest = ranges;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut bytes = 0;
        let mut n = 0;
        for (a, b) in rest {
            let width = if a == b { 4 } else { 8 };
            if n > 0 && bytes + width > budget {
                break;
            }
            bytes += width;
            n += 1;
        }
        let (run, tail) = rest.split_at(n);
        rest = tail;
        Some(run)
    })
}

/// Per-connection receiver settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Microseconds.
    pub latency: i64,
    /// Packets the receive buffer holds.
    pub buffer: usize,
    /// Close after this long with nothing received (microseconds).
    pub peer_idle: i64,
    /// Largest data payload accepted, in bytes; larger packets are ignored.
    pub max_payload: usize,
    /// The negotiated MTU, IP and UDP headers included. A NAK is split so
    /// each datagram fits it, with room for IPv6 (see
    /// [`nak_list_budget`]); values outside 576 to 1500 are clamped.
    pub mtu: u32,
    /// The caller set the retransmission flag (`REXMITFLG`) in its
    /// handshake, so its retransmissions can be told from originals. The
    /// reorder wait adapts only then.
    pub retransmit_flag: bool,
    /// The longest the first NAK of a new gap may be held while the path
    /// reorders (microseconds); never more than an eighth of `latency`, and
    /// 0 turns the hold off.
    pub reorder_wait_max: i64,
}

/// Counts for one connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Data packets received, every copy.
    pub received: u64,
    /// Payload bytes in those packets.
    pub received_bytes: u64,
    /// Of those, retransmissions.
    pub retransmitted: u64,
    /// Copies of packets already held.
    pub duplicates: u64,
    /// Copies of packets already delivered or given up on.
    pub belated: u64,
    /// Missing packets that then arrived as originals, not retransmissions:
    /// the path reordered them. Those whose NAK had already gone out were
    /// resent for nothing; [`nak_held`](Self::nak_held) counts the rest. This
    /// relies on the caller setting the retransmission flag (`REXMITFLG`), as
    /// libsrt does; a caller that never sets it has its retransmissions
    /// counted here too.
    pub reordered: u64,
    /// Of those, packets whose first NAK was still being held when they came,
    /// so none was sent for them.
    pub nak_held: u64,
    /// The highest the reorder wait reached, in microseconds (0 if the path
    /// never reordered, the caller does not flag retransmissions or the hold
    /// is off). A first NAK is held for at most this, and less where the
    /// gap's deadline is too near.
    pub max_reorder_wait: u64,
    /// Packets found missing (each counted once).
    pub lost: u64,
    /// Missing packets skipped at their delivery time.
    pub dropped: u64,
    /// Packets handed over in order for delivery.
    pub delivered: u64,
    /// Data packets refused for a plaintext/key mismatch or expired key.
    pub encrypted: u64,
    /// Data packets decrypted.
    pub decrypted: u64,
    /// Key material refresh messages refused: encryption is not in use on the
    /// connection, or the message failed to unwrap or validate.
    pub km_rejected: u64,
    /// NAK packets sent.
    pub naks_sent: u64,
    /// ACK packets sent, light and full.
    pub acks_sent: u64,
}

/// Why a receiver closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The peer sent SHUTDOWN.
    Shutdown,
    /// Nothing arrived for the idle timeout.
    Timeout,
}

#[derive(Debug)]
struct Slot {
    deliver_at: i64,
    payload: Vec<u8>,
}

/// Packets and bytes a second, over the last whole second.
#[derive(Debug, Default)]
struct RateMeter {
    since: i64,
    packets: u32,
    bytes: u64,
    rate: (u32, u32),
}

impl RateMeter {
    fn add(&mut self, now: i64, bytes: usize) {
        let elapsed = now - self.since;
        if elapsed >= 1_000_000 {
            let per_s = |n: u64| u32::try_from(n * 1_000_000 / elapsed as u64).unwrap_or(u32::MAX);
            self.rate = (per_s(u64::from(self.packets)), per_s(self.bytes));
            self.since = now;
            self.packets = 0;
            self.bytes = 0;
        }
        self.packets = self.packets.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes as u64);
    }
}

/// Link capacity from the spacing of back-to-back packets: the median of
/// the last 16 gaps between consecutive originals.
#[derive(Debug, Default)]
struct PacketPairs {
    last: Option<(Seq, i64)>,
    gaps: [i64; PACKET_PAIR_GAPS],
    len: usize,
    next: usize,
}

impl PacketPairs {
    fn add(&mut self, seq: Seq, now: i64) {
        if let Some((s, t)) = self.last
            && s.next() == seq
        {
            if let Some(gap) = self.gaps.get_mut(self.next) {
                *gap = (now - t).max(1);
            }
            self.next = (self.next + 1) % PACKET_PAIR_GAPS;
            self.len = (self.len + 1).min(PACKET_PAIR_GAPS);
        }
        self.last = Some((seq, now));
    }

    /// Packets per second; 1000 before there is anything to go on (as
    /// libsrt reports at first, observed).
    fn capacity(&self) -> u32 {
        if self.len == 0 {
            return 1000;
        }
        // Full ACKs can run every 10 ms per receiver. Keep the rolling history
        // inline and median scratch on the stack, without a heap allocation
        // for every estimate. Ring order does not affect the median.
        let mut g = self.gaps;
        let Some(g) = g.get_mut(..self.len) else {
            return 1000;
        };
        g.sort_unstable();
        let median = g.get(g.len() / 2).copied().unwrap_or(1).max(1);
        u32::try_from(1_000_000 / median).unwrap_or(u32::MAX)
    }
}

/// Follows the sender's clock: the earliest arrival relative to the
/// timestamp, over a window of packets, so queueing delay does not count.
#[derive(Debug, Default)]
struct Drift {
    offset: i64,
    window_min: Option<i64>,
    count: u32,
}

impl Drift {
    fn sample(&mut self, lateness: i64) {
        self.window_min = Some(self.window_min.map_or(lateness, |m| m.min(lateness)));
        self.count += 1;
        if self.count >= DRIFT_WINDOW {
            let target = self.window_min.unwrap_or(self.offset);
            self.offset = (self.offset + (target - self.offset).clamp(-DRIFT_STEP, DRIFT_STEP))
                .clamp(-DRIFT_MAX, DRIFT_MAX);
            self.window_min = None;
            self.count = 0;
        }
    }
}

/// One connection's receiving side; see the module docs. Give it packets and
/// the time, then take its control packets and deliveries.
pub struct Receiver {
    settings: Settings,
    peer_socket: u32,
    /// When the connection opened; our packets' timestamps count from here.
    start: i64,
    /// Local time of sender timestamp 0.
    base: i64,
    ts: TimestampExtender,
    drift: Drift,
    /// The next packet to deliver; everything before it is delivered or
    /// given up on.
    head: Seq,
    buf: VecDeque<Option<Slot>>,
    /// First occupied slot, retained across missing heads and sequence wrap.
    /// Only removing this packet requires another scan of the buffer.
    first_present: Option<Seq>,
    /// The first sequence number not yet received (what an ACK reports).
    ack_pos: Seq,
    loss: LossList,
    rtt: Rtt,
    next_ack_number: u32,
    last_ack_sent: Seq,
    last_full_ack: i64,
    last_full_ack_pos: Seq,
    /// Latest data position confirmed by a retained full ACK's ACKACK.
    ack_confirmed: Seq,
    unanswered: VecDeque<(u32, i64, Seq)>,
    since_ack: u32,
    last_sent: i64,
    last_received: i64,
    /// The latest time seen.
    now: i64,
    rate: RateMeter,
    pairs: PacketPairs,
    stats: Stats,
    closed: Option<CloseReason>,
    /// Control packets to send, encoded.
    send: VecDeque<Vec<u8>>,
    /// Payloads to hand over, each marked if data was lost just before it.
    deliver: VecDeque<(Vec<u8>, bool)>,
    /// Data was given up on since the last payload queued for delivery.
    lost: bool,
    crypto: Option<Decryptor>,
    next_km: i64,
    /// One distinct KM attempt after a decrypted forward key-slot change.
    early_km: bool,
    /// How long the first NAK of a new gap is held (microseconds); 0 until
    /// the path is seen to reorder.
    reorder_wait: i64,
    /// When `reorder_wait` was last raised or halved.
    reorder_at: i64,
}

impl Receiver {
    /// `hs_timestamp` is the conclusion's timestamp, `initial_seq` the
    /// sender's first sequence number, both from its handshake.
    pub fn new(
        settings: Settings,
        now: i64,
        peer_socket: u32,
        initial_seq: Seq,
        hs_timestamp: u32,
    ) -> Self {
        Self {
            settings,
            peer_socket,
            start: now,
            base: now - i64::from(hs_timestamp),
            ts: TimestampExtender::starting_at(hs_timestamp),
            drift: Drift::default(),
            head: initial_seq,
            buf: VecDeque::new(),
            first_present: None,
            ack_pos: initial_seq,
            loss: LossList::new(),
            rtt: Rtt::new(),
            next_ack_number: 1,
            last_ack_sent: initial_seq,
            last_full_ack: now,
            last_full_ack_pos: initial_seq,
            ack_confirmed: initial_seq,
            unanswered: VecDeque::new(),
            since_ack: 0,
            last_sent: now,
            last_received: now,
            now,
            rate: RateMeter {
                since: now,
                ..RateMeter::default()
            },
            pairs: PacketPairs::default(),
            stats: Stats::default(),
            closed: None,
            send: VecDeque::new(),
            deliver: VecDeque::new(),
            lost: false,
            crypto: None,
            next_km: 0,
            early_km: false,
            reorder_wait: 0,
            reorder_at: now,
        }
    }

    /// The connection's counters so far.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// When the last packet from the peer, data or control, arrived (the
    /// caller's clock, in microseconds). Media is delivered up to the
    /// latency after it arrives; this is when the peer was last heard.
    pub fn last_received(&self) -> i64 {
        self.last_received
    }

    /// The round-trip time estimate.
    pub fn rtt(&self) -> &Rtt {
        &self.rtt
    }

    /// The TSBPD latency this connection delivers with, in microseconds.
    pub fn latency(&self) -> i64 {
        self.settings.latency
    }

    pub(crate) fn set_crypto(&mut self, crypto: Option<Decryptor>) {
        self.crypto = crypto;
    }

    /// Why the connection closed, or `None` while it is open.
    pub fn closed(&self) -> Option<CloseReason> {
        self.closed
    }

    /// Takes the next encoded control packet to send to the caller, if any.
    pub fn take_send(&mut self) -> Option<Vec<u8>> {
        self.send.pop_front()
    }

    /// Takes the next payload due for delivery, in sequence order, if any,
    /// and whether data was given up on just before it: a packet dropped as
    /// too late, or skipped by a jump past the whole buffer. MPEG-TS
    /// continuity counters cannot see every such gap, so the consumer must
    /// treat the payload as following a discontinuity.
    pub fn take_delivery(&mut self) -> Option<(Vec<u8>, bool)> {
        self.deliver.pop_front()
    }

    /// Packets held, received or not.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    fn queue(&mut self, now: i64, body: Control) {
        let ts = (now - self.start) as u32;
        let mut out = Vec::with_capacity(64);
        ControlPacket::new(ts, self.peer_socket, body).encode(&mut out);
        self.send.push_back(out);
        self.last_sent = now;
    }

    /// Takes a data packet that arrived at `now`: checks its encryption,
    /// buffers it, reports any gap in a NAK (at once, or after the reorder
    /// wait from [`on_timer`](Self::on_timer)), sends a light ACK after
    /// enough packets and releases contiguous data that is due. Missing
    /// packets expire in [`on_timer`](Self::on_timer), after the adapter has
    /// drained a bounded batch of ready input.
    pub fn on_data(&mut self, now: i64, p: &DataPacket) {
        self.now = self.now.max(now);
        if self.closed.is_some() {
            return;
        }
        if (self.crypto.is_some() && !matches!(p.key, 1 | 2))
            || (self.crypto.is_none() && p.key != 0)
        {
            // A key mismatch must never become plaintext delivery.
            self.stats.encrypted += 1;
            return;
        }
        if p.payload.len() > self.settings.max_payload {
            return;
        }
        self.stats.received += 1;
        self.stats.received_bytes += p.payload.len() as u64;
        if p.retransmitted {
            self.stats.retransmitted += 1;
        }
        // Reject replay/duplicate sequences before allocating or decrypting.
        let distance = self.head.distance_to(p.seq);
        if distance < 0 {
            self.stats.belated += 1;
            return;
        }
        if self.buf.get(distance as usize).is_some_and(Option::is_some) {
            self.stats.duplicates += 1;
            return;
        }
        let mut payload = p.payload.to_vec();
        if let Some(crypto) = &mut self.crypto {
            let previous_key = crypto.active_key();
            if crypto.decrypt(p.key, p.seq, &mut payload).is_err() {
                self.stats.encrypted += 1;
                return;
            }
            // Existing staged/highest guards fence slot progression. CTR
            // does not authenticate the data or its key flag.
            if crypto.active_key() != previous_key {
                self.early_km = true;
            }
            self.stats.decrypted += 1;
        }
        self.last_received = now;
        let ext = self.ts.extend(p.timestamp);
        if !p.retransmitted {
            self.drift
                .sample(now.saturating_sub(self.base.saturating_add(ext)));
            self.rate.add(now, p.payload.len());
            self.pairs.add(p.seq, now);
        }
        let deliver_at = self
            .base
            .saturating_add(self.drift.offset)
            .saturating_add(ext)
            .saturating_add(self.settings.latency)
            .min(now.saturating_add(self.settings.latency + MAX_HOLD_BEYOND_LATENCY));

        let mut d = self.head.distance_to(p.seq);
        if d < 0 {
            self.stats.belated += 1;
            return;
        }
        if d as usize >= self.settings.buffer {
            self.make_room(p.seq);
            d = self.head.distance_to(p.seq);
        }
        let idx = d as usize;
        let slot = Slot {
            deliver_at,
            payload,
        };
        if let Some(existing) = self.buf.get_mut(idx) {
            if existing.is_some() {
                self.stats.duplicates += 1;
                return;
            }
            *existing = Some(slot);
            let taken = self.loss.take(p.seq, deliver_at);
            if !p.retransmitted {
                self.stats.reordered += 1;
                if let Some(taken) = taken {
                    if !taken.reported {
                        self.stats.nak_held += 1;
                    }
                    self.reordered(now, now.saturating_sub(taken.found));
                }
            }
        } else {
            if idx > self.buf.len() {
                // A gap: report it at once, or hold the report while the
                // path reorders. A hold the loss list cannot take is
                // reported at once too.
                let first = self.head.add(self.buf.len() as u32);
                let last = p.seq.prev();
                self.stats.lost += first.distance_to(last) as u64 + 1;
                let wait = self.reorder_wait(now);
                let held = wait > 0
                    && self.loss.hold(
                        first,
                        last,
                        now,
                        deliver_at,
                        now.saturating_add(wait),
                        self.timing(),
                    );
                if !held {
                    self.loss.add(first, last, now, deliver_at);
                    self.stats.naks_sent += 1;
                    self.queue(now, Control::Nak(vec![(first, last)]));
                }
                self.buf.resize_with(idx, || None);
            }
            self.buf.push_back(Some(slot));
        }
        if self.first_present.is_none_or(|first| p.seq < first) {
            self.first_present = Some(p.seq);
        }
        self.advance_ack_pos();
        self.since_ack += 1;
        if self.since_ack >= LIGHT_ACK_PACKETS && self.ack_pos != self.last_ack_sent {
            self.light_ack(now);
        }
        self.nak_due(now);
        // A delayed adapter can have reordered originals or retransmissions
        // already queued in its socket. Deliver contiguous ready data here,
        // but let its timer call expire gaps after the bounded receive batch.
        self.release(now, false);
    }

    /// The most the reorder wait may reach: the setting, within an eighth of
    /// the latency; 0 (no hold) for a caller that does not flag its
    /// retransmissions.
    fn reorder_cap(&self) -> i64 {
        if !self.settings.retransmit_flag {
            return 0;
        }
        self.settings
            .reorder_wait_max
            .min(self.settings.latency / 8)
            .max(0)
    }

    /// The reorder wait at `now`: halved for every second since it was last
    /// raised, and none below a millisecond.
    fn reorder_wait(&mut self, now: i64) -> i64 {
        while self.reorder_wait > 0 && now.saturating_sub(self.reorder_at) >= REORDER_DECAY {
            self.reorder_wait /= 2;
            if self.reorder_wait < REORDER_FLOOR {
                self.reorder_wait = 0;
            }
            self.reorder_at = self.reorder_at.saturating_add(REORDER_DECAY);
        }
        self.reorder_wait
    }

    /// An original filled a gap found `lateness` ago: hold later gaps' first
    /// NAKs long enough to cover that, up to the cap.
    fn reordered(&mut self, now: i64, lateness: i64) {
        let cap = self.reorder_cap();
        if cap == 0 {
            return;
        }
        let want = (lateness.max(0).saturating_mul(5) / 4)
            .saturating_add(REORDER_MARGIN)
            .min(cap);
        self.reorder_wait = self.reorder_wait(now).max(want);
        self.reorder_at = now;
        self.stats.max_reorder_wait = self
            .stats
            .max_reorder_wait
            .max(u64::try_from(self.reorder_wait).unwrap_or(0));
    }

    /// `seq` is beyond the buffer: deliver or give up the oldest packets to
    /// make room. A jump further than the whole buffer starts afresh at
    /// `seq` rather than reporting everything between as lost.
    fn make_room(&mut self, seq: Seq) {
        let d = self.head.distance_to(seq) as usize;
        let excess = d + 1 - self.settings.buffer;
        if excess > self.buf.len() {
            self.flush();
            self.head = seq;
            self.loss.clear();
            self.lost = true;
        } else {
            for _ in 0..excess {
                self.pop_front();
            }
            self.loss.remove_before(self.head);
        }
        self.advance_ack_pos();
    }

    /// Delivers or drops the packet at the head.
    fn pop_front(&mut self) {
        let removed_first = self.first_present == Some(self.head);
        match self.buf.pop_front() {
            Some(Some(slot)) => {
                self.stats.delivered += 1;
                let lost = std::mem::take(&mut self.lost);
                self.deliver.push_back((slot.payload, lost));
            }
            Some(None) => {
                self.stats.dropped += 1;
                self.lost = true;
            }
            None => {}
        }
        self.head = self.head.next();
        if removed_first {
            self.first_present = self
                .buf
                .iter()
                .position(Option::is_some)
                .map(|i| self.head.add(i as u32));
        }
    }

    /// Earliest received packet by sequence, not by its delivery timestamp.
    fn first_slot(&self) -> Option<(usize, &Slot)> {
        let index = self.head.distance_to(self.first_present?) as usize;
        self.buf.get(index)?.as_ref().map(|slot| (index, slot))
    }

    /// Delivers everything held, in order, now.
    fn flush(&mut self) {
        while !self.buf.is_empty() {
            self.pop_front();
        }
        self.loss.clear();
        self.advance_ack_pos();
    }

    fn advance_ack_pos(&mut self) {
        let mut i = self.head.distance_to(self.ack_pos).max(0) as usize;
        while matches!(self.buf.get(i), Some(Some(_))) {
            i += 1;
        }
        self.ack_pos = self.head.add(i as u32);
    }

    /// TSBPD: hands over every packet whose time has come; a missing packet
    /// is skipped once a later one is due (too-late packet drop), but only
    /// when the adapter services timers after draining ready input.
    fn release(&mut self, now: i64, expire_missing: bool) {
        loop {
            match self.buf.front() {
                Some(Some(slot)) if slot.deliver_at <= now => self.pop_front(),
                Some(None) if expire_missing => {
                    let due = self.first_slot().map(|(i, slot)| (i, slot.deliver_at));
                    match due {
                        Some((i, at)) if at <= now => {
                            for _ in 0..i {
                                self.pop_front();
                            }
                            self.loss.remove_before(self.head);
                            self.advance_ack_pos();
                        }
                        _ => break,
                    }
                }
                _ => break,
            }
        }
    }

    fn timing(&self) -> Timing {
        Timing {
            rtt: self.rtt.rtt(),
            retry: self.rtt.retry(),
        }
    }

    fn nak_due(&mut self, now: i64) {
        if self.loss.is_empty() {
            return;
        }
        let ranges = self.loss.due(now, self.timing(), MAX_LOSS_ENTRIES / 2);
        // One NAK when it fits the path, as ever; otherwise as many
        // consecutive ones as it takes, none dropped or reordered, so the
        // request is never a datagram the path fragments or drops.
        let budget = nak_list_budget(self.settings.mtu);
        for run in split_loss_list(&ranges, budget) {
            self.stats.naks_sent += 1;
            self.queue(now, Control::Nak(run.to_vec()));
        }
    }

    fn light_ack(&mut self, now: i64) {
        self.last_ack_sent = self.ack_pos;
        self.since_ack = 0;
        self.stats.acks_sent += 1;
        let ack = Ack {
            number: 0,
            ack: self.ack_pos,
            stats: None,
        };
        self.queue(now, Control::Ack(ack));
    }

    fn full_ack(&mut self, now: i64) {
        let number = self.next_ack_number;
        self.next_ack_number = self.next_ack_number.wrapping_add(1).max(1);
        self.last_ack_sent = self.ack_pos;
        self.last_full_ack = now;
        self.last_full_ack_pos = self.ack_pos;
        self.since_ack = 0;
        self.stats.acks_sent += 1;
        if self.unanswered.len() >= ACKS_REMEMBERED {
            self.unanswered.pop_front();
        }
        self.unanswered.push_back((number, now, self.ack_pos));
        let clamp = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
        let free = self.settings.buffer.saturating_sub(self.buf.len());
        let ack = Ack {
            number,
            ack: self.ack_pos,
            stats: Some(AckStats {
                rtt: clamp(self.rtt.rtt()),
                rtt_var: clamp(self.rtt.var()),
                buffer: u32::try_from(free).unwrap_or(u32::MAX),
                rates: Some((self.rate.rate.0, self.pairs.capacity(), self.rate.rate.1)),
            }),
        };
        self.queue(now, Control::Ack(ack));
    }

    /// Takes a control packet that arrived at `now`. An ACKACK gives an RTT
    /// sample, SHUTDOWN flushes and closes, a drop request stops asking for
    /// those packets and a key material refresh is answered. KM work is
    /// limited to one attempt per 100 ms, plus one distinct attempt after
    /// successfully decrypting a forward key-slot change. Any other control
    /// packet only shows the caller is alive.
    pub fn on_control(&mut self, now: i64, c: &Control) {
        self.now = self.now.max(now);
        if self.closed.is_some() {
            return;
        }
        if let Control::Other {
            control_type: 0x7fff,
            subtype: 3,
            info: 0,
            cif,
        } = c
        {
            let cached = self
                .crypto
                .as_ref()
                .is_some_and(|crypto| crypto.matches_last_km(cif));
            // Packet-count refreshes can legitimately arrive less than
            // 100 ms apart. Only a decrypted forward key-slot transition
            // grants an early distinct attempt, including a malformed one.
            // Cached retries neither bypass the timer nor consume credit.
            if now < self.next_km && (!self.early_km || cached) {
                return;
            }
            if !cached {
                self.early_km = false;
            }
            self.next_km = now.saturating_add(100_000);
            if self
                .crypto
                .as_mut()
                .is_some_and(|crypto| crypto.refresh(cif).is_ok())
            {
                self.last_received = now;
                self.queue(
                    now,
                    Control::Other {
                        control_type: 0x7fff,
                        subtype: 4,
                        info: 0,
                        cif: cif.clone(),
                    },
                );
            } else {
                self.stats.km_rejected += 1;
            }
            return;
        }
        self.last_received = now;
        match c {
            Control::AckAck { number } => {
                if let Some(i) = self.unanswered.iter().position(|(n, _, _)| n == number) {
                    if let Some((_, sent, position)) = self.unanswered.get(i).copied() {
                        self.rtt.sample(now - sent);
                        if self.ack_confirmed.distance_to(position) > 0 {
                            self.ack_confirmed = position;
                        }
                    }
                    self.unanswered.drain(..=i);
                }
            }
            Control::Shutdown => {
                self.flush();
                self.closed = Some(CloseReason::Shutdown);
            }
            Control::DropRequest { first, last, .. } => {
                // The sender no longer has these: stop asking for them. A
                // held loss this pushes off the list makes the next timer
                // due at once, which reports it.
                self.loss.remove_range(*first, *last);
            }
            _ => {}
        }
    }

    /// Does what is due at `now`: the idle timeout, delivery of due packets,
    /// a full ACK, NAK re-requests and a keepalive.
    pub fn on_timer(&mut self, now: i64) {
        self.now = self.now.max(now);
        if self.closed.is_some() {
            return;
        }
        if now - self.last_received >= self.settings.peer_idle {
            self.closed = Some(CloseReason::Timeout);
            return;
        }
        self.release(now, true);
        if self
            .full_ack_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            self.full_ack(now);
        }
        self.nak_due(now);
        if now - self.last_sent >= KEEPALIVE_INTERVAL {
            self.queue(now, Control::KeepAlive);
        }
    }

    /// New progress uses SYN; an unchanged, unconfirmed position waits for
    /// feedback before repeating. A light ACK cannot confirm either case.
    fn full_ack_deadline(&self) -> Option<i64> {
        if self.ack_pos == self.ack_confirmed {
            return None;
        }
        let interval = if self.ack_pos != self.last_full_ack_pos {
            ACK_INTERVAL
        } else {
            self.rtt.retry().max(ACK_INTERVAL)
        };
        Some(self.last_full_ack.saturating_add(interval))
    }

    /// When [`on_timer`](Self::on_timer) next has something to do.
    pub fn next_timer(&self) -> i64 {
        let mut t = self
            .last_received
            .saturating_add(self.settings.peer_idle)
            .min(self.last_sent + KEEPALIVE_INTERVAL);
        if let Some(deadline) = self.full_ack_deadline() {
            t = t.min(deadline);
        }
        if let Some(at) = self.loss.next_due(self.now, self.timing()) {
            t = t.min(at);
        }
        if let Some((_, slot)) = self.first_slot() {
            t = t.min(slot.deliver_at);
        }
        t
    }

    /// Ends the connection from this side: a SHUTDOWN to the peer.
    pub fn shutdown(&mut self, now: i64) {
        self.queue(now, Control::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{CryptoConfig, Encryptor, KeySize};
    use crate::packet::{Packet, Position};

    const MS: i64 = 1000;
    const LATENCY: i64 = 200 * MS;
    const ISN: u32 = 1000;

    fn settings() -> Settings {
        Settings {
            latency: LATENCY,
            buffer: 64,
            peer_idle: 5_000 * MS,
            max_payload: 1456,
            mtu: 1500,
            retransmit_flag: true,
            reorder_wait_max: 50 * MS,
        }
    }

    /// A receiver opened at time 0 by a handshake stamped 0, so a packet
    /// stamped `t` is due at `t + LATENCY`.
    fn rx() -> Receiver {
        Receiver::new(settings(), 0, 77, Seq::new(ISN), 0)
    }

    fn data(seq: u32, ts: u32, payload: &[u8]) -> DataPacket<'_> {
        DataPacket {
            seq: Seq::new(seq),
            position: Position::Only,
            in_order: false,
            key: 0,
            retransmitted: false,
            message: 1,
            timestamp: ts,
            dest: 5,
            payload,
        }
    }

    fn sent(r: &mut Receiver) -> Vec<Control> {
        std::iter::from_fn(|| r.take_send())
            .map(|b| match Packet::parse(&b) {
                Ok(Packet::Control(c)) => {
                    assert_eq!(c.dest, 77);
                    c.body
                }
                other => panic!("{other:?}"),
            })
            .collect()
    }

    fn delivered(r: &mut Receiver) -> Vec<Vec<u8>> {
        marked(r).into_iter().map(|(payload, _)| payload).collect()
    }

    fn marked(r: &mut Receiver) -> Vec<(Vec<u8>, bool)> {
        std::iter::from_fn(|| r.take_delivery()).collect()
    }

    fn km_request(bytes: &[u8]) -> Control {
        Control::Other {
            control_type: 0x7fff,
            subtype: 3,
            info: 0,
            cif: bytes.to_vec(),
        }
    }

    fn km_responses(r: &mut Receiver) -> Vec<Vec<u8>> {
        sent(r)
            .into_iter()
            .filter_map(|control| match control {
                Control::Other {
                    control_type: 0x7fff,
                    subtype: 4,
                    info: 0,
                    cif,
                } => Some(cif),
                _ => None,
            })
            .collect()
    }

    /// Hold the last even-key packet, then decrypt the first odd-key one.
    fn key_transition(size: KeySize) -> (Encryptor, Receiver, Vec<u8>, Vec<u8>) {
        let mut config = CryptoConfig::new(b"scoped receiver test secret".to_vec(), size)
            .expect("crypto config");
        config.refresh_packets = 32;
        config.preannounce_packets = 8;
        let mut tx = Encryptor::new(&config).expect("encryptor");
        let mut r = rx();
        r.set_crypto(Some(
            Decryptor::new(&config, &tx.initial().expect("initial KM")).expect("decryptor"),
        ));
        let mut cached = None;
        let mut old = None;
        for n in 0..=32 {
            let mut payload = vec![n as u8; 23];
            let key = tx
                .encrypt(Seq::new(ISN + n), &mut payload, 0)
                .expect("valid packet");
            if n == 31 {
                old = Some(payload);
            } else {
                let mut p = data(ISN + n, n * MS as u32, &payload);
                p.key = key;
                r.on_data(i64::from(n) * MS, &p);
            }
            if let Some(km) = tx.poll_km(i64::from(n) * MS) {
                r.on_control(i64::from(n) * MS, &km_request(&km));
                assert_eq!(km_responses(&mut r), vec![km.clone()]);
                assert!(tx.confirm(&km));
                cached = Some(km);
            }
        }
        sent(&mut r);
        assert!(r.early_km, "forward slot transition grants one attempt");
        (tx, r, cached.expect("first KM"), old.expect("held packet"))
    }

    #[test]
    fn key_transition_credit_survives_cached_km_and_old_slot_reordering() {
        for size in [KeySize::Aes128, KeySize::Aes256] {
            let (mut tx, mut r, cached, old) = key_transition(size);
            for _ in 0..1000 {
                r.on_control(33 * MS, &km_request(&cached));
            }
            assert!(km_responses(&mut r).is_empty(), "cached flood stays timed");
            assert!(r.early_km);
            // Recover old ciphertext before its slot is replaced, as the
            // sender's retained-key rule requires.
            let mut p = data(ISN + 31, 31 * MS as u32, &old);
            p.key = 1;
            p.retransmitted = true;
            r.on_data(34 * MS, &p);
            // A timed cached response also leaves the distinct attempt available.
            r.on_control(124 * MS, &km_request(&cached));
            assert_eq!(km_responses(&mut r), vec![cached.clone()]);
            assert!(r.early_km);
            let mut replacement = None;
            for n in 33..=56 {
                let now = i64::from(n + 92) * MS;
                let mut payload = vec![n as u8; 23];
                let key = tx
                    .encrypt(Seq::new(ISN + n), &mut payload, 0)
                    .expect("valid packet");
                let mut p = data(ISN + n, now as u32, &payload);
                p.key = key;
                r.on_data(now, &p);
                if let Some(km) = tx.poll_km(now) {
                    r.on_control(now, &km_request(&km));
                    assert_eq!(km_responses(&mut r), vec![km.clone()]);
                    assert!(tx.confirm(&km));
                    replacement = Some(km);
                }
            }
            assert!(!r.early_km, "one distinct attempt consumed the credit");
            r.on_data(149 * MS, &p);
            assert_eq!(r.stats().decrypted, 57, "duplicate refused before decrypt");
            assert_eq!(r.stats().duplicates, 1);
            assert!(!r.early_km, "duplicate old slot cannot grant new credit");
            r.on_control(150 * MS, &km_request(&cached));
            assert!(km_responses(&mut r).is_empty());
            r.on_control(248 * MS, &km_request(&cached));
            assert!(
                km_responses(&mut r).is_empty(),
                "stale distinct KM rejected"
            );
            assert_eq!(r.stats().km_rejected, 1);
            let replacement = replacement.expect("second KM");
            r.on_control(348 * MS, &km_request(&replacement));
            assert_eq!(km_responses(&mut r), vec![replacement]);
            r.on_timer(500 * MS);
            assert_eq!(
                delivered(&mut r),
                (0..=56).map(|n| vec![n; 23]).collect::<Vec<_>>()
            );
            assert_eq!(r.stats().dropped, 0);
        }
    }

    #[test]
    fn malformed_km_consumes_only_one_transition_credit_then_stays_timed() {
        for size in [KeySize::Aes128, KeySize::Aes256] {
            let (_, mut r, cached, old) = key_transition(size);
            let bad = km_request(&[0; 104]);
            for _ in 0..1000 {
                r.on_control(33 * MS, &bad);
            }
            assert_eq!(r.stats().km_rejected, 1);
            assert!(!r.early_km, "malformed attempt spends the same credit");
            assert!(km_responses(&mut r).is_empty());
            let mut p = data(ISN + 31, 31 * MS as u32, &old);
            p.key = 1;
            p.retransmitted = true;
            r.on_data(34 * MS, &p);
            assert_eq!(r.stats().decrypted, 33, "reordered old slot decrypted");
            assert!(!r.early_km, "valid reordered old data cannot replenish");
            let mut p = data(ISN + 100, 35 * MS as u32, &old);
            p.key = 1;
            r.on_data(35 * MS, &p);
            assert_eq!(r.stats().encrypted, 1, "unstaged forward slot refused");
            assert!(!r.early_km, "failed decryption cannot grant credit");
            let mut invalid_wrap = cached.clone();
            *invalid_wrap.last_mut().expect("KM wrap") ^= 1;
            let invalid_wrap = km_request(&invalid_wrap);
            for _ in 0..1000 {
                r.on_control(132 * MS, &invalid_wrap);
                r.on_control(132 * MS, &km_request(&cached));
            }
            assert_eq!(r.stats().km_rejected, 1);
            assert!(km_responses(&mut r).is_empty());
            for _ in 0..1000 {
                r.on_control(133 * MS, &invalid_wrap);
            }
            assert_eq!(r.stats().km_rejected, 2, "ordinary 100ms limit remains");
            assert_eq!(r.next_km, 233 * MS);
        }
    }

    #[test]
    fn packet_pair_capacity_uses_the_upper_median_and_expires_old_gaps() {
        let mut pairs = PacketPairs::default();
        assert_eq!(pairs.capacity(), 1000, "no arrivals yet");
        let mut seq = Seq::new(Seq::MAX - 4);
        let mut now = 0;
        pairs.add(seq, now);
        assert_eq!(pairs.capacity(), 1000, "one arrival has no gap");
        for gap in [100, 400] {
            seq = seq.next();
            now += gap;
            pairs.add(seq, now);
        }
        assert_eq!(pairs.capacity(), 2500, "upper median of two gaps");
        for _ in 0..PACKET_PAIR_GAPS {
            seq = seq.next();
            now += 100;
            pairs.add(seq, now);
        }
        assert_eq!(pairs.capacity(), 10_000, "old 400 us gap has expired");
        for _ in 0..PACKET_PAIR_GAPS {
            seq = seq.next();
            now += 400;
            pairs.add(seq, now);
        }
        assert_eq!(pairs.capacity(), 2500, "all earlier gaps have expired");
        assert_eq!(pairs.len, PACKET_PAIR_GAPS);
    }

    #[test]
    fn packet_pair_capacity_matches_arrival_history_through_wrap_and_reordering() {
        let mut pairs = PacketPairs::default();
        let mut history = VecDeque::new();
        let mut previous: Option<(Seq, i64)> = None;
        let mut seq = Seq::new(Seq::MAX - 7);
        let mut now = 0;
        let mut random = 7u64;
        for i in 0..2048 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            // Consecutive originals, holes, duplicates and out-of-order
            // arrivals all occur across both sequence and history wraps.
            seq = match random >> 60 {
                0 => seq,
                1 => seq.prev(),
                2 => seq.add(3),
                _ => seq.next(),
            };
            // Include same-time and backward-clock spacing, which the
            // estimator historically clamps to one microsecond.
            now += match random >> 56 & 7 {
                0 => 0,
                1 => -1,
                _ => ((random >> 32) % 10_000) as i64 + 1,
            };
            if let Some((last_seq, last_time)) = previous
                && last_seq.next() == seq
            {
                history.push_back((now - last_time).max(1));
                while history.len() > 16 {
                    history.pop_front();
                }
            }
            previous = Some((seq, now));
            pairs.add(seq, now);
            let mut sorted: Vec<_> = history.iter().copied().collect();
            sorted.sort_unstable();
            let expected = if sorted.is_empty() {
                1000
            } else {
                (1_000_000 / sorted[sorted.len() / 2]) as u32
            };
            assert_eq!(pairs.capacity(), expected, "arrival {i}");
        }
    }

    #[test]
    fn full_ack_advertises_packet_pair_capacity_after_history_wrap() {
        let mut r = rx();
        let mut now = 0;
        for i in 0..40 {
            now += if i < 20 { 100 } else { 400 };
            r.on_data(now, &data(ISN + i, now as u32, b"x"));
        }
        r.on_timer(ACK_INTERVAL);
        let controls = sent(&mut r);
        let ack = controls
            .into_iter()
            .find_map(|c| match c {
                Control::Ack(a) => Some(a),
                _ => None,
            })
            .expect("full ACK");
        assert_eq!(
            ack.stats
                .expect("full ACK stats")
                .rates
                .expect("full ACK rates")
                .1,
            2500
        );
    }

    #[test]
    fn unauthenticated_staged_key_bit_flip_cannot_replenish_spent_km_credit() {
        let mut config = CryptoConfig::new(
            b"generated key-bit control secret".to_vec(),
            KeySize::Aes128,
        )
        .expect("valid key-bit control configuration");
        config.refresh_packets = 32;
        config.preannounce_packets = 8;
        let mut tx = Encryptor::new(&config).expect("key-bit control encryptor");
        let mut r = rx();
        r.set_crypto(Some(
            Decryptor::new(&config, &tx.initial().expect("initial key material"))
                .expect("key-bit control decryptor"),
        ));
        for n in 0..=24 {
            let mut payload = vec![n as u8; 23];
            let key = tx
                .encrypt(Seq::new(ISN + n), &mut payload, 0)
                .expect("original encryption");
            let mut packet = data(ISN + n, n * MS as u32, &payload);
            packet.key = key;
            r.on_data(i64::from(n) * MS, &packet);
            if let Some(km) = tx.poll_km(i64::from(n) * MS) {
                r.on_control(i64::from(n) * MS, &km_request(&km));
                assert_eq!(km_responses(&mut r), vec![km.clone()]);
                assert!(tx.confirm(&km));
            }
        }
        assert!(!r.early_km, "valid KM staging alone grants no credit");
        let mut payload = vec![25; 23];
        let original_key = tx
            .encrypt(Seq::new(ISN + 25), &mut payload, 0)
            .expect("even original");
        assert_eq!(original_key, 1);
        let mut forged = data(ISN + 25, 25 * MS as u32, &payload);
        forged.key = 2;
        r.on_data(25 * MS, &forged);
        // CTR provides no authenticity: an altered flag can consume an
        // already staged slot. This checks work bounds, not valid media.
        assert!(
            r.early_km,
            "one credit after the existing staged transition"
        );
        let bad = km_request(&[0; 104]);
        for _ in 0..1000 {
            r.on_control(26 * MS, &bad);
        }
        assert_eq!(r.stats().km_rejected, 1);
        assert!(!r.early_km);
        for n in 26..=31 {
            let mut packet = data(ISN + n, n * MS as u32, b"not authenticated");
            packet.key = 1;
            r.on_data(i64::from(n) * MS, &packet);
            packet.key = 2;
            r.on_data(i64::from(n) * MS, &packet);
            r.on_control(i64::from(n) * MS, &bad);
            assert!(!r.early_km, "alternating flags cannot stage another key");
        }
        r.on_data(32 * MS, &forged);
        assert!(!r.early_km, "duplicate altered packet cannot renew credit");
        assert_eq!(
            r.stats().km_rejected,
            1,
            "no additional unwrap attempt inside the timer"
        );
        assert_eq!(r.next_km, 126 * MS);
        assert!(km_responses(&mut r).is_empty());
        r.on_control(126 * MS, &bad);
        assert_eq!(
            r.stats().km_rejected,
            2,
            "ordinary timed attempt remains available"
        );
        assert!(!r.early_km);
    }

    #[test]
    fn delivers_in_order_at_timestamp_plus_latency() {
        let mut r = rx();
        r.on_data(10 * MS, &data(ISN, 10 * MS as u32, b"a"));
        r.on_data(20 * MS, &data(ISN + 1, 20 * MS as u32, b"b"));
        assert!(delivered(&mut r).is_empty());
        assert_eq!(r.next_timer(), 10 * MS, "the first full ACK");
        r.on_timer(10 * MS);
        assert_eq!(r.next_timer(), 210 * MS);
        r.on_timer(209 * MS);
        assert!(delivered(&mut r).is_empty());
        r.on_timer(210 * MS);
        assert_eq!(delivered(&mut r), vec![b"a".to_vec()]);
        r.on_timer(220 * MS);
        assert_eq!(delivered(&mut r), vec![b"b".to_vec()]);
        assert_eq!(r.stats().delivered, 2);
    }

    #[test]
    fn a_gap_is_reported_at_once_and_filled_by_a_retransmission() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 0, b"0"));
        r.on_data(MS, &data(ISN + 3, MS as u32, b"3"));
        let s = |v| Seq::new(v);
        assert_eq!(
            sent(&mut r),
            vec![Control::Nak(vec![(s(ISN + 1), s(ISN + 2))])]
        );
        assert_eq!(r.stats().lost, 2);
        let mut p = data(ISN + 1, 0, b"1");
        p.retransmitted = true;
        r.on_data(5 * MS, &p);
        let mut p = data(ISN + 2, 0, b"2");
        p.retransmitted = true;
        r.on_data(6 * MS, &p);
        r.on_timer(300 * MS);
        assert_eq!(
            delivered(&mut r),
            [b"0", b"1", b"2", b"3"].map(|x| x.to_vec()).to_vec()
        );
        assert_eq!(r.stats().dropped, 0);
        assert_eq!(r.stats().retransmitted, 2);
        assert_eq!(r.stats().reordered, 0, "retransmissions are not reordering");
    }

    #[test]
    fn queued_reordered_data_survives_an_overdue_receive_batch() {
        let mut r = rx();
        // All three datagrams are already readable after the adapter was
        // descheduled beyond the delivery deadline. The missing original is
        // later in that ready batch; expiry must wait until it is drained.
        r.on_data(300 * MS, &data(ISN, 0, b"0"));
        r.on_data(300 * MS, &data(ISN + 2, 2 * MS as u32, b"2"));
        assert_eq!(r.stats().dropped, 0, "do not expire between ready reads");
        r.on_data(300 * MS, &data(ISN + 1, MS as u32, b"1"));
        r.on_timer(300 * MS);
        assert_eq!(
            marked(&mut r),
            vec![
                (b"0".to_vec(), false),
                (b"1".to_vec(), false),
                (b"2".to_vec(), false)
            ]
        );
        assert_eq!(r.stats().dropped, 0);
        assert_eq!(r.stats().belated, 0);
        assert_eq!(r.stats().reordered, 1, "the late original filled its gap");
    }

    #[test]
    fn an_overdue_batch_still_marks_a_gap_when_no_recovery_is_queued() {
        let mut r = rx();
        r.on_data(300 * MS, &data(ISN, 0, b"0"));
        r.on_data(300 * MS, &data(ISN + 2, 2 * MS as u32, b"2"));
        assert_eq!(r.stats().dropped, 0);
        assert!(
            r.next_timer() <= 300 * MS,
            "gap expiry remains immediately due"
        );
        r.on_timer(300 * MS);
        assert_eq!(
            marked(&mut r),
            vec![(b"0".to_vec(), false), (b"2".to_vec(), true)]
        );
        assert_eq!(r.stats().dropped, 1);
    }

    #[test]
    fn the_payload_after_a_given_up_packet_is_marked_and_only_that_one() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 0, b"0"));
        r.on_data(2 * MS, &data(ISN + 2, 2 * MS as u32, b"2"));
        r.on_data(3 * MS, &data(ISN + 3, 3 * MS as u32, b"3"));
        r.on_timer(203 * MS);
        assert_eq!(
            marked(&mut r),
            vec![
                (b"0".to_vec(), false),
                (b"2".to_vec(), true),
                (b"3".to_vec(), false)
            ]
        );
        assert_eq!(r.stats().dropped, 1);
    }

    #[test]
    fn a_missing_packet_is_skipped_when_the_next_is_due() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 0, b"0"));
        r.on_data(2 * MS, &data(ISN + 2, 2 * MS as u32, b"2"));
        r.on_timer(200 * MS);
        assert_eq!(delivered(&mut r), vec![b"0".to_vec()]);
        r.on_timer(201 * MS);
        assert!(delivered(&mut r).is_empty(), "waits for 1 until 2 is due");
        r.on_timer(202 * MS);
        assert_eq!(delivered(&mut r), vec![b"2".to_vec()]);
        assert_eq!(r.stats().dropped, 1);
        // The ACK moves past the dropped packet (the fake ACK).
        sent(&mut r);
        r.on_timer(300 * MS);
        let acks: Vec<_> = sent(&mut r)
            .into_iter()
            .filter_map(|c| match c {
                Control::Ack(a) => Some(a.ack),
                _ => None,
            })
            .collect();
        assert_eq!(acks.last(), Some(&Seq::new(ISN + 3)));
        // The retransmission arriving now is too late.
        let mut p = data(ISN + 1, 0, b"1");
        p.retransmitted = true;
        r.on_data(301 * MS, &p);
        assert_eq!(r.stats().belated, 1);
        assert!(delivered(&mut r).is_empty());
    }

    #[test]
    fn acks_and_rtt_from_ackack() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 0, b"0"));
        r.on_timer(10 * MS);
        let c = sent(&mut r);
        let [Control::Ack(a)] = c.as_slice() else {
            panic!("{c:?}");
        };
        assert_eq!(a.number, 1);
        assert_eq!(a.ack, Seq::new(ISN + 1));
        let st = a.stats.expect("full ACK");
        assert_eq!((st.rtt, st.rtt_var), (100_000, 50_000));
        assert_eq!(st.buffer, 63);
        r.on_control(40 * MS, &Control::AckAck { number: 1 });
        assert_eq!(r.rtt().rtt(), 30 * MS);
        // Nothing new: no ACK.
        r.on_timer(50 * MS);
        assert!(sent(&mut r).is_empty());
        r.on_data(51 * MS, &data(ISN + 1, 51 * MS as u32, b"1"));
        r.on_timer(60 * MS);
        let c = sent(&mut r);
        assert!(
            matches!(c.as_slice(), [Control::Ack(a)] if a.number == 2 && a.stats.is_some_and(|s| s.rtt == 30_000)),
            "{c:?}"
        );
    }

    #[test]
    fn unconfirmed_ack_repeats_at_retry_interval_then_stops() {
        let mut r = rx();
        r.on_timer(10 * MS);
        assert!(
            sent(&mut r).is_empty(),
            "idle connection has no ACK traffic"
        );
        r.on_data(11 * MS, &data(ISN, 11_000, b"x"));
        r.on_timer(20 * MS);
        assert!(matches!(sent(&mut r).as_slice(), [Control::Ack(a)] if a.number == 1));
        r.on_timer(319 * MS);
        assert!(
            sent(&mut r).is_empty(),
            "do not repeat before the retry interval"
        );
        r.on_timer(320 * MS);
        assert!(matches!(sent(&mut r).as_slice(), [Control::Ack(a)] if a.number == 2));
        r.on_timer(320 * MS);
        assert!(sent(&mut r).is_empty(), "no same-time repeat loop");
        // An older ACK number for the same position confirms that position.
        r.on_control(321 * MS, &Control::AckAck { number: 1 });
        r.on_timer(900 * MS);
        assert!(sent(&mut r).is_empty());
        assert!(r.next_timer() > 900 * MS);
    }

    #[test]
    fn unchanged_ack_repeats_never_exceed_the_syn_rate() {
        let mut r = rx();
        r.rtt.sample(1);
        r.on_data(0, &data(ISN, 0, b"x"));
        r.on_timer(10 * MS);
        sent(&mut r);
        assert_eq!(r.next_timer(), 20 * MS);
        r.on_timer(20 * MS - 1);
        assert!(sent(&mut r).is_empty());
        r.on_timer(20 * MS);
        assert!(matches!(sent(&mut r).as_slice(), [Control::Ack(a)] if a.number == 2));
        assert_eq!(r.next_timer(), 30 * MS);
    }

    #[test]
    fn old_ackack_cannot_confirm_new_progress_across_sequence_wrap() {
        let mut r = Receiver::new(settings(), 0, 77, Seq::new(Seq::MAX), 0);
        r.next_ack_number = u32::MAX;
        r.on_data(0, &data(Seq::MAX, 0, b"a"));
        r.on_timer(10 * MS);
        assert!(
            matches!(sent(&mut r).as_slice(), [Control::Ack(a)] if a.number == u32::MAX && a.ack == Seq::new(0))
        );
        r.on_data(11 * MS, &data(0, 11_000, b"b"));
        r.on_timer(20 * MS);
        assert!(
            matches!(sent(&mut r).as_slice(), [Control::Ack(a)] if a.number == 1 && a.ack == Seq::new(1))
        );
        r.on_control(21 * MS, &Control::AckAck { number: u32::MAX });
        // The old position is confirmed, but the newer full ACK still needs
        // a response. The RTT sample shortens its retry interval to 33 ms.
        r.on_timer(53 * MS);
        assert!(
            matches!(sent(&mut r).as_slice(), [Control::Ack(a)] if a.number == 2 && a.ack == Seq::new(1))
        );
        let rtt = r.rtt().rtt();
        for number in [u32::MAX, 42, 0] {
            r.on_control(54 * MS, &Control::AckAck { number });
        }
        assert_eq!(
            r.rtt().rtt(),
            rtt,
            "unknown and stale ACKACKs cannot sample RTT"
        );
        r.on_control(55 * MS, &Control::AckAck { number: 1 });
        r.on_timer(500 * MS);
        assert!(sent(&mut r).is_empty());
        assert!(r.next_timer() > 500 * MS);
    }

    #[test]
    fn repeated_ack_history_and_delayed_timer_remain_bounded() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 0, b"x"));
        for i in 0..(ACKS_REMEMBERED + 4) {
            let at = 10 * MS + i as i64 * 300 * MS;
            r.on_control(at, &Control::KeepAlive);
            r.on_timer(at);
            assert!(matches!(sent(&mut r).as_slice(), [Control::Ack(_)]));
            assert!(r.unanswered.len() <= ACKS_REMEMBERED);
            r.on_timer(at);
            assert!(sent(&mut r).is_empty());
        }
        let at = 60_000 * MS;
        r.on_control(at, &Control::KeepAlive);
        r.on_control(at, &Control::AckAck { number: 1 }); // evicted
        assert_eq!(r.rtt().rtt(), 100 * MS);
        r.on_timer(at);
        assert!(
            matches!(sent(&mut r).as_slice(), [Control::Ack(_)]),
            "one ACK, no catch-up burst"
        );
        assert!(r.next_timer() > at);
    }

    #[test]
    fn light_ack_every_64_packets() {
        let mut r = Receiver::new(
            Settings {
                buffer: 1000,
                ..settings()
            },
            0,
            77,
            Seq::new(ISN),
            0,
        );
        for i in 0..64 {
            r.on_data(0, &data(ISN + i, 0, b"x"));
        }
        let c = sent(&mut r);
        assert!(
            matches!(c.as_slice(), [Control::Ack(Ack { number: 0, stats: None, ack })] if *ack == Seq::new(ISN + 64)),
            "{c:?}"
        );
        r.on_timer(10 * MS);
        assert!(
            matches!(sent(&mut r).as_slice(), [Control::Ack(a)] if a.number == 1 && a.stats.is_some()),
            "a light ACK cannot suppress the full tail ACK"
        );
    }

    #[test]
    fn nak_again_after_a_round_trip_and_its_variance() {
        let mut r = Receiver::new(
            Settings {
                latency: 2_000 * MS,
                ..settings()
            },
            0,
            77,
            Seq::new(ISN),
            0,
        );
        // One RTT sample: 60 ms, so the variance is 30 ms and a loss is
        // asked for again 60 + 4 x 30 = 180 ms after the last request.
        r.on_data(0, &data(ISN, 0, b"0"));
        r.on_timer(10 * MS);
        r.on_control(70 * MS, &Control::AckAck { number: 1 });
        assert_eq!((r.rtt().rtt(), r.rtt().retry()), (60 * MS, 180 * MS));
        sent(&mut r);
        // A gap far from its 2 s deadline, so only the ordinary interval
        // applies.
        r.on_data(71 * MS, &data(ISN + 2, 71 * MS as u32, b"2"));
        let naks = |c: Vec<Control>| c.iter().filter(|c| matches!(c, Control::Nak(_))).count();
        assert_eq!(naks(sent(&mut r)), 1, "the immediate NAK");
        assert_eq!(r.next_timer(), 251 * MS);
        r.on_timer(250 * MS);
        assert_eq!(naks(sent(&mut r)), 0);
        r.on_timer(251 * MS);
        assert_eq!(naks(sent(&mut r)), 1);
        assert_eq!(r.next_timer(), 431 * MS);
    }

    #[test]
    fn idle_timeout_and_keepalive() {
        let mut r = rx();
        assert_eq!(r.next_timer(), KEEPALIVE_INTERVAL);
        r.on_timer(KEEPALIVE_INTERVAL);
        assert_eq!(sent(&mut r), vec![Control::KeepAlive]);
        r.on_control(4_000 * MS, &Control::KeepAlive);
        r.on_timer(8_999 * MS);
        assert_eq!(r.closed(), None);
        r.on_timer(9_000 * MS);
        assert_eq!(r.closed(), Some(CloseReason::Timeout));
    }

    #[test]
    fn shutdown_delivers_what_is_held() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 0, b"0"));
        r.on_data(0, &data(ISN + 1, 0, b"1"));
        r.on_control(MS, &Control::Shutdown);
        assert_eq!(r.closed(), Some(CloseReason::Shutdown));
        assert_eq!(delivered(&mut r).len(), 2);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut r = Receiver::new(settings(), 0, 77, Seq::new(Seq::MAX - 1), 0);
        for (i, s) in [Seq::MAX - 1, Seq::MAX, 0, 1].into_iter().enumerate() {
            r.on_data(0, &data(s, i as u32, &[i as u8]));
        }
        r.on_timer(LATENCY + 10);
        assert_eq!(delivered(&mut r), vec![vec![0], vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn timestamps_wrap() {
        // The handshake 3 ms before the 32-bit wrap.
        let hs = u32::MAX - 3_000;
        let mut r = Receiver::new(settings(), 0, 77, Seq::new(ISN), hs);
        r.on_data(2 * MS, &data(ISN, hs.wrapping_add(2_000), b"0"));
        r.on_data(5 * MS, &data(ISN + 1, hs.wrapping_add(5_000), b"1"));
        r.on_timer(LATENCY + 2 * MS);
        assert_eq!(delivered(&mut r), vec![b"0".to_vec()]);
        r.on_timer(LATENCY + 5 * MS);
        assert_eq!(delivered(&mut r), vec![b"1".to_vec()]);
    }

    #[test]
    fn a_full_buffer_delivers_early_and_a_huge_jump_starts_afresh() {
        let mut r = rx();
        for i in 0..64 {
            r.on_data(0, &data(ISN + i, 0, b"x"));
        }
        r.on_data(0, &data(ISN + 64, 0, b"y"));
        assert_eq!(delivered(&mut r).len(), 1, "the oldest made room");
        assert_eq!(r.buffered(), 64);
        r.on_data(0, &data(ISN + 1_000_000, 0, b"z"));
        assert_eq!(delivered(&mut r).len(), 64, "everything held, at once");
        assert_eq!(r.buffered(), 1);
        assert_eq!(r.stats().lost, 0, "a jump is not a loss");
    }

    #[test]
    fn encrypted_and_oversized_packets_are_refused() {
        let mut r = rx();
        let mut p = data(ISN, 0, b"x");
        p.key = 1;
        r.on_data(0, &p);
        let big = vec![0u8; 1457];
        r.on_data(0, &data(ISN, 0, &big));
        assert_eq!(r.stats().encrypted, 1);
        assert_eq!(r.stats().received, 0);
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn a_far_future_timestamp_is_not_held_forever() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 2_000_000_000, b"x"));
        assert!(r.next_timer() <= LATENCY + MAX_HOLD_BEYOND_LATENCY);
    }

    #[test]
    fn drift_follows_a_slow_sender_clock() {
        let mut r = rx();
        // The sender's clock runs 0.1% slow: each packet arrives a little
        // later than its timestamp says.
        for i in 0..3000u32 {
            let now = i64::from(i) * 4 * MS;
            let ts = (now - now / 1000) as u32;
            r.on_data(now, &data(ISN + i, ts, b"x"));
            r.on_timer(now);
            delivered(&mut r);
            sent(&mut r);
        }
        // 12 s in, the sender is 12 ms behind; corrections follow it in
        // bounded steps.
        assert!(r.drift.offset > 4 * MS, "{}", r.drift.offset);
        assert!(r.drift.offset <= 12 * MS, "{}", r.drift.offset);
    }

    #[test]
    fn drop_request_stops_naks() {
        let mut r = rx();
        r.on_data(0, &data(ISN, 0, b"0"));
        r.on_data(0, &data(ISN + 5, 100 * MS as u32, b"5"));
        sent(&mut r);
        r.on_control(
            MS,
            &Control::DropRequest {
                message: 0,
                first: Seq::new(ISN + 1),
                last: Seq::new(ISN + 4),
            },
        );
        r.on_timer(150 * MS);
        assert!(!sent(&mut r).iter().any(|c| matches!(c, Control::Nak(_))));
    }

    /// At v4v's 2 s latency, so a new gap has room to be held (the reorder
    /// cap is then the 50 ms setting).
    fn rx2(retransmit_flag: bool) -> Receiver {
        Receiver::new(
            Settings {
                latency: 2_000 * MS,
                buffer: 32_768,
                retransmit_flag,
                ..settings()
            },
            0,
            77,
            Seq::new(ISN),
            0,
        )
    }

    fn nak_ranges(c: &[Control]) -> Vec<(Seq, Seq)> {
        c.iter()
            .filter_map(|c| match c {
                Control::Nak(r) => Some(r.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Original `ISN + off`, stamped `ts`, arriving at `at`.
    fn orig(r: &mut Receiver, off: u32, at: i64, ts: i64) {
        r.on_data(at, &data(ISN + off, ts as u32, b"x"));
    }

    /// 0 at 0, 2 at 2 ms and 1 at 12 ms: 1 came 10 ms after its gap was
    /// found, so the wait is 10 x 5/4 + 1 = 13.5 ms. Returns the time.
    fn adapt(r: &mut Receiver) -> i64 {
        orig(r, 0, 0, 0);
        orig(r, 2, 2 * MS, 2 * MS);
        assert_eq!(
            nak_ranges(&sent(r)),
            vec![(Seq::new(ISN + 1), Seq::new(ISN + 1))],
            "no reordering seen yet: at once, as before"
        );
        orig(r, 1, 12 * MS, MS);
        assert_eq!(r.stats().reordered, 1);
        assert_eq!(r.stats().nak_held, 0, "its NAK had gone out");
        12 * MS
    }

    /// Runs only the timer, at each time it asks for, until `end`; returns
    /// each NAK range with the time it was sent.
    fn timer_naks(r: &mut Receiver, end: i64) -> Vec<(i64, (Seq, Seq))> {
        let mut out = Vec::new();
        for _ in 0..100_000 {
            // As an adapter would: a timer already past runs now.
            let at = r.next_timer().max(r.now);
            if at > end || r.closed().is_some() {
                return out;
            }
            r.on_timer(at);
            out.extend(nak_ranges(&sent(r)).into_iter().map(|range| (at, range)));
        }
        panic!("the timer does not advance");
    }

    #[test]
    fn reordered_originals_within_the_wait_send_no_nak_once_adapted() {
        let mut r = rx2(true);
        let mut t = adapt(&mut r);
        assert_eq!(r.reorder_wait, 13_500);
        assert_eq!(r.stats().max_reorder_wait, 13_500);
        // Fifty more pairs swapped, each original 10 ms late.
        for k in 0..50 {
            let a = 3 + 2 * k;
            t += MS;
            orig(&mut r, a + 1, t, t);
            t += 10 * MS;
            orig(&mut r, a, t, t - 11 * MS);
            r.on_timer(t);
        }
        assert!(nak_ranges(&sent(&mut r)).is_empty(), "no NAK for any");
        assert!(timer_naks(&mut r, t + 3_000 * MS).is_empty());
        let st = r.stats();
        assert_eq!((st.reordered, st.nak_held, st.naks_sent), (51, 50, 1));
        assert_eq!((st.lost, st.dropped, st.delivered), (51, 0, 103));
    }

    #[test]
    fn genuine_loss_is_reported_from_the_timer_when_the_wait_ends() {
        let mut r = rx2(true);
        let t = adapt(&mut r) + MS;
        // 3 never comes.
        orig(&mut r, 4, t, t);
        assert!(nak_ranges(&sent(&mut r)).is_empty(), "held");
        let due = t + 13_500;
        assert_eq!(r.loss.next_due(r.now, r.timing()), Some(due));
        assert!(r.next_timer() <= due, "the timer wakes for it");
        // Nothing but the timer from here: the report comes when the wait
        // ends, then the ordinary re-requests.
        let naks = timer_naks(&mut r, t + 2_100 * MS);
        let s3 = (Seq::new(ISN + 3), Seq::new(ISN + 3));
        assert_eq!(naks.first(), Some(&(due, s3)));
        assert!(naks.len() > 1 && naks.iter().all(|(_, range)| *range == s3));
        assert_eq!(r.stats().nak_held, 0);
        assert_eq!(r.stats().dropped, 1);
    }

    #[test]
    fn without_the_retransmission_flag_the_wait_never_adapts() {
        let mut r = rx2(false);
        orig(&mut r, 0, 0, 0);
        let mut t = 0;
        for k in 0..20 {
            let a = 1 + 2 * k;
            t += MS;
            orig(&mut r, a + 1, t, t);
            t += 10 * MS;
            orig(&mut r, a, t, t - 11 * MS);
        }
        assert_eq!(nak_ranges(&sent(&mut r)).len(), 20, "every gap at once");
        let st = r.stats();
        assert_eq!((st.reordered, st.nak_held, st.max_reorder_wait), (20, 0, 0));
        assert_eq!(r.reorder_wait, 0);
        // Nor with the flag when retransmissions fill the gaps.
        let mut r = rx2(true);
        orig(&mut r, 0, 0, 0);
        orig(&mut r, 2, MS, MS);
        let mut p = data(ISN + 1, 0, b"x");
        p.retransmitted = true;
        r.on_data(40 * MS, &p);
        assert_eq!(r.reorder_wait, 0);
        assert_eq!(r.stats().reordered, 0);
    }

    #[test]
    fn the_wait_is_capped_by_the_setting_and_an_eighth_of_the_latency() {
        let reorder = |r: &mut Receiver, late: i64| {
            orig(r, 0, 0, 0);
            orig(r, 2, MS, MS);
            orig(r, 1, MS + late, 0);
            r.reorder_wait
        };
        assert_eq!(reorder(&mut rx2(true), 500 * MS), 50 * MS);
        let mut r = rx();
        assert_eq!(reorder(&mut r, 100 * MS), LATENCY / 8);
        assert_eq!(r.stats().max_reorder_wait, 25_000);
        let mut r = Receiver::new(
            Settings {
                latency: 2_000 * MS,
                reorder_wait_max: 0,
                ..settings()
            },
            0,
            77,
            Seq::new(ISN),
            0,
        );
        assert_eq!(reorder(&mut r, 10 * MS), 0, "0 turns it off");
    }

    #[test]
    fn no_hold_near_the_deadline() {
        // At 200 ms with the starting 100 ms round trip, every gap is
        // already within three round trips of its deadline.
        let mut r = rx();
        orig(&mut r, 0, 0, 0);
        orig(&mut r, 2, MS, MS);
        orig(&mut r, 1, 11 * MS, 0);
        assert!(r.reorder_wait > 0);
        sent(&mut r);
        orig(&mut r, 4, 12 * MS, 12 * MS);
        assert_eq!(
            nak_ranges(&sent(&mut r)),
            vec![(Seq::new(ISN + 3), Seq::new(ISN + 3))]
        );
        // At 2 s, a gap revealed by a packet due in 250 ms: at once.
        let mut r = rx2(true);
        let t = adapt(&mut r) + 1_800 * MS;
        orig(&mut r, 4, t - 10 * MS, t - 1_760 * MS);
        assert!(r.reorder_wait > 0);
        assert_eq!(
            nak_ranges(&sent(&mut r)),
            vec![(Seq::new(ISN + 3), Seq::new(ISN + 3))]
        );
        // Its original, 10 ms late, keeps the wait at 13.5 ms...
        orig(&mut r, 3, t, t - 1_761 * MS);
        assert_eq!(r.reorder_wait, 13_500);
        // ...but a gap due in 310 ms is held only until 300 ms (three of
        // the starting round trips) before: 10 ms.
        orig(&mut r, 6, t, t - 1_690 * MS);
        assert!(nak_ranges(&sent(&mut r)).is_empty());
        let naks = timer_naks(&mut r, t + 20 * MS);
        assert_eq!(
            naks.first(),
            Some(&(t + 10 * MS, (Seq::new(ISN + 5), Seq::new(ISN + 5))))
        );
    }

    #[test]
    fn the_wait_halves_every_quiet_second_back_to_none() {
        let mut r = rx2(true);
        let t = adapt(&mut r);
        assert_eq!(r.reorder_wait(t + 999 * MS), 13_500);
        assert_eq!(r.reorder_wait(t + 1_000 * MS), 6_750);
        assert_eq!(r.reorder_wait(t + 2_500 * MS), 3_375);
        assert_eq!(r.reorder_wait(t + 3_000 * MS), 1_687);
        assert_eq!(r.reorder_wait(t + 4_000 * MS), 0, "under 1 ms is none");
        assert_eq!(r.stats().max_reorder_wait, 13_500);
        // Then a gap is reported at once again.
        let t = t + 4_000 * MS;
        orig(&mut r, 3, t, t);
        orig(&mut r, 5, t, t);
        assert_eq!(
            nak_ranges(&sent(&mut r)),
            vec![(Seq::new(ISN + 4), Seq::new(ISN + 4))]
        );
        // A long quiet spell goes straight to none, and new reordering
        // raises it again.
        let mut r = rx2(true);
        let t = adapt(&mut r);
        assert_eq!(r.reorder_wait(t + 60_000 * MS), 0);
        let mut r = rx2(true);
        let t = adapt(&mut r) + 1_500 * MS;
        orig(&mut r, 3, t, t);
        orig(&mut r, 5, t, t);
        orig(&mut r, 4, t + 2 * MS, t);
        assert_eq!(r.reorder_wait, 6_750, "halved once, above 2 x 5/4 + 1 ms");
        orig(&mut r, 7, t + 3 * MS, t);
        orig(&mut r, 6, t + 23 * MS, t);
        assert_eq!(r.reorder_wait, 26_000);
    }

    #[test]
    fn a_full_loss_list_never_loses_an_unreported_gap() {
        use crate::loss::MAX_RANGES;
        let mut r = rx2(true);
        let t = adapt(&mut r) + MS;
        // Gaps of three, more than the list holds, all found at once.
        let gaps = MAX_RANGES as u32 + 10;
        for k in 1..=gaps {
            orig(&mut r, 2 + 4 * k, t, t);
        }
        let missing = |k: u32| (Seq::new(ISN + 4 * k - 1), Seq::new(ISN + 4 * k + 1));
        // The list took the first ones on hold; the rest are asked for at
        // once.
        let at_once = nak_ranges(&sent(&mut r));
        assert_eq!(
            at_once,
            (MAX_RANGES as u32 + 1..=gaps)
                .map(missing)
                .collect::<Vec<_>>()
        );
        // An original filling the middle of the first gap splits it, which
        // pushes the latest held one off the full list: reported now.
        orig(&mut r, 4, t + MS, t);
        assert_eq!(nak_ranges(&sent(&mut r)), vec![missing(MAX_RANGES as u32)]);
        // Every other gap is reported when the wait ends.
        let held: Vec<_> = timer_naks(&mut r, t + 14 * MS)
            .into_iter()
            .map(|(_, range)| range)
            .collect();
        let mut expected = vec![
            (Seq::new(ISN + 3), Seq::new(ISN + 3)),
            (Seq::new(ISN + 5), Seq::new(ISN + 5)),
        ];
        expected.extend((2..MAX_RANGES as u32).map(missing));
        assert_eq!(held, expected);
    }

    /// A small deterministic generator for the schedule below.
    struct XorShift(u64);
    impl XorShift {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    #[test]
    fn under_heavy_reordering_and_loss_every_lost_packet_is_reported_in_time() {
        for seed in [1, 7, 42] {
            let mut rng = XorShift(0x9E37_79B9_7F4A_7C15 ^ seed);
            let mut r = rx2(true);
            // A packet every 10 us for 200 ms; 2% lost, half the rest late
            // by up to 30 ms: well over MAX_RANGES gaps open at once.
            let n = 20_000u32;
            let mut arrivals = Vec::new();
            let mut lost = Vec::new();
            for i in 0..n {
                let sent_at = i64::from(i) * 10;
                if i > 0 && rng.below(100) < 2 {
                    lost.push(i);
                    continue;
                }
                let late = if rng.below(2) == 0 {
                    rng.below(30_000) as i64
                } else {
                    0
                };
                arrivals.push((sent_at + late, i, sent_at));
            }
            arrivals.sort_unstable();
            // When each gap was found: the first arrival of a later packet.
            let mut found = vec![i64::MAX; n as usize];
            let mut latest = None::<u32>;
            for &(at, i, _) in &arrivals {
                if latest.is_none_or(|l| i > l) {
                    for j in latest.map_or(0, |l| l + 1)..i {
                        found[j as usize] = at;
                    }
                    latest = Some(i);
                }
            }
            let mut first_nak = vec![None::<i64>; n as usize];
            let mut note = |naks: Vec<(i64, (Seq, Seq))>| {
                for (at, (a, b)) in naks {
                    let mut s = a;
                    loop {
                        let i = Seq::new(ISN).distance_to(s) as usize;
                        if let Some(slot) = first_nak.get_mut(i) {
                            slot.get_or_insert(at);
                        }
                        if s == b {
                            break;
                        }
                        s = s.next();
                    }
                }
            };
            for &(at, i, ts) in &arrivals {
                note(timer_naks(&mut r, at - 1));
                orig(&mut r, i, at, ts);
                note(
                    nak_ranges(&sent(&mut r))
                        .into_iter()
                        .map(|x| (at, x))
                        .collect(),
                );
            }
            note(timer_naks(&mut r, 2_400 * MS));
            assert!(r.stats().max_reorder_wait > 0, "seed {seed}: adapted");
            assert!(r.stats().nak_held > 0, "seed {seed}: held some");
            for i in lost {
                let nak = first_nak[i as usize];
                let gap = found[i as usize];
                assert!(
                    nak.is_some_and(|at| at >= gap && at <= gap + 50 * MS),
                    "seed {seed}: {i} found at {gap}, first NAK {nak:?}"
                );
            }
        }
    }

    /// Compare the cached lookup and public timer with the original linear
    /// scans after each state transition, including empty and wrapped buffers.
    fn assert_first_present_matches_scan(r: &Receiver) {
        let scanned = r
            .buf
            .iter()
            .enumerate()
            .find_map(|(i, slot)| slot.as_ref().map(|slot| (i, slot)));
        assert_eq!(r.first_present, scanned.map(|(i, _)| r.head.add(i as u32)));
        assert_eq!(
            r.first_slot()
                .map(|(i, slot)| (i, slot.deliver_at, &slot.payload)),
            scanned.map(|(i, slot)| (i, slot.deliver_at, &slot.payload))
        );
        let mut expected = r
            .last_received
            .saturating_add(r.settings.peer_idle)
            .min(r.last_sent + KEEPALIVE_INTERVAL);
        if let Some(deadline) = r.full_ack_deadline() {
            expected = expected.min(deadline);
        }
        if let Some(at) = r.loss.next_due(r.now, r.timing()) {
            expected = expected.min(at);
        }
        if let Some((_, slot)) = scanned {
            expected = expected.min(slot.deliver_at);
        }
        assert_eq!(r.next_timer(), expected);
    }

    /// The old release algorithm over a separate value-only buffer. This
    /// deliberately does not use the hint or Receiver's pop_front helper.
    fn release_against_scan_reference(r: &mut Receiver, now: i64) {
        let mut buffer: VecDeque<_> = r
            .buf
            .iter()
            .map(|slot| slot.as_ref().map(|s| (s.deliver_at, s.payload.clone())))
            .collect();
        let mut head = r.head;
        let mut lost = r.lost;
        let mut deliveries = r.deliver.clone();
        let mut dropped = r.stats().dropped;
        let mut delivered = r.stats().delivered;
        loop {
            let pop = match buffer.front() {
                Some(Some((at, _))) => *at <= now,
                Some(None) => buffer
                    .iter()
                    .find_map(|slot| slot.as_ref().map(|(at, _)| *at))
                    .is_some_and(|at| at <= now),
                None => false,
            };
            if !pop {
                break;
            }
            match buffer.pop_front() {
                Some(Some((_, payload))) => {
                    delivered += 1;
                    deliveries.push_back((payload, std::mem::take(&mut lost)));
                }
                Some(None) => {
                    dropped += 1;
                    lost = true;
                }
                None => unreachable!("a populated head was checked above"),
            }
            head = head.next();
        }
        r.release(now, true);
        assert_eq!(r.head, head);
        assert_eq!(r.lost, lost);
        assert_eq!(r.deliver, deliveries);
        assert_eq!(r.stats().dropped, dropped);
        assert_eq!(r.stats().delivered, delivered);
        let actual: VecDeque<_> = r
            .buf
            .iter()
            .map(|slot| slot.as_ref().map(|s| (s.deliver_at, s.payload.clone())))
            .collect();
        assert_eq!(actual, buffer);
        assert_first_present_matches_scan(r);
    }

    #[test]
    fn release_matches_scan_with_sparse_nonmonotonic_delivery_times() {
        for initial in [Seq::new(ISN), Seq::new(Seq::MAX - 4)] {
            let mut r = Receiver::new(settings(), 0, 77, initial, 0);
            // Later sequence numbers need not have later delivery times.
            for (offset, timestamp) in [(10, 0), (2, 50 * MS), (7, 20 * MS), (20, 0)] {
                let mut p = data(initial.add(offset).get(), timestamp as u32, b"sparse");
                p.retransmitted = true;
                r.on_data(0, &p);
            }
            for now in [0, LATENCY, LATENCY + 49 * MS, LATENCY + 50 * MS] {
                release_against_scan_reference(&mut r, now);
            }
            assert_eq!(r.stats().delivered, 4);
            assert_eq!(r.stats().dropped, 17);
            assert_eq!(r.buffered(), 0);
        }
    }

    #[test]
    fn first_present_follows_earlier_retransmissions_across_sequence_wrap() {
        let initial = Seq::new(Seq::MAX - 4);
        let mut r = Receiver::new(settings(), 0, 77, initial, 0);
        // A far packet is received first, then an earlier retransmission.
        // Its later timestamp must still hold back the later sequence.
        r.on_data(0, &data(initial.add(10).get(), 0, b"far"));
        assert_first_present_matches_scan(&r);
        let mut earlier = data(initial.add(2).get(), 50 * MS as u32, b"early");
        earlier.retransmitted = true;
        r.on_data(MS, &earlier);
        assert_first_present_matches_scan(&r);
        r.on_timer(LATENCY);
        assert!(delivered(&mut r).is_empty());
        assert_first_present_matches_scan(&r);
        r.on_timer(LATENCY + 50 * MS);
        assert_eq!(
            marked(&mut r),
            vec![(b"early".to_vec(), true), (b"far".to_vec(), true)]
        );
        assert_eq!(r.stats().dropped, 9);
        assert_first_present_matches_scan(&r);
    }

    #[test]
    fn first_present_survives_room_making_drop_requests_and_shutdown() {
        let mut r = rx();
        r.on_data(0, &data(ISN + 60, 0, b"first"));
        r.on_data(MS, &data(ISN + 70, MS as u32, b"second"));
        assert_eq!(r.head, Seq::new(ISN + 7));
        assert_first_present_matches_scan(&r);
        r.on_control(
            2 * MS,
            &Control::DropRequest {
                message: 0,
                first: r.head,
                last: Seq::new(ISN + 59),
            },
        );
        assert_first_present_matches_scan(&r);
        r.on_data(3 * MS, &data(ISN + 1_000_000, 3 * MS as u32, b"jump"));
        assert_eq!(
            marked(&mut r),
            vec![(b"first".to_vec(), true), (b"second".to_vec(), true)]
        );
        assert_eq!(r.first_present, Some(Seq::new(ISN + 1_000_000)));
        assert_first_present_matches_scan(&r);
        r.on_control(4 * MS, &Control::Shutdown);
        assert_eq!(marked(&mut r), vec![(b"jump".to_vec(), true)]);
        assert_first_present_matches_scan(&r);
    }

    #[test]
    fn first_present_matches_linear_scans_through_mixed_receiver_events() {
        for initial in [Seq::new(0), Seq::new(Seq::MAX - 31)] {
            let mut r = Receiver::new(settings(), 0, 77, initial, 0);
            let mut random = 0x91e1_0da5u32;
            for step in 0..12_000u32 {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let now = i64::from(step) * MS;
                match random % 7 {
                    0 => {
                        release_against_scan_reference(&mut r, now);
                        r.on_timer(now);
                    }
                    1 => r.on_control(
                        now,
                        &Control::DropRequest {
                            message: 0,
                            first: r.head,
                            last: r.head.add(random % 32),
                        },
                    ),
                    _ => {
                        let offset = (random >> 8) % 96;
                        let seq = if random & 0x10000 == 0 {
                            r.head.add(offset)
                        } else {
                            r.head.prev()
                        };
                        let mut packet = data(seq.get(), now as u32, b"payload");
                        packet.retransmitted = random & 0x20000 != 0;
                        // Exercise key-mismatch refusal too.
                        if random & 0x3f == 0 {
                            packet.key = 1;
                        }
                        r.on_data(now, &packet);
                    }
                }
                assert_first_present_matches_scan(&r);
                while r.take_delivery().is_some() {}
                while r.take_send().is_some() {}
            }
            r.on_control(12_001 * MS, &Control::Shutdown);
            assert_first_present_matches_scan(&r);
            assert_eq!(r.buffered(), 0);
        }
    }

    /// The raw NAK datagrams queued, with their parsed form.
    fn raw_naks(r: &mut Receiver) -> Vec<(Vec<u8>, ControlPacket)> {
        std::iter::from_fn(|| r.take_send())
            .filter_map(|b| match Packet::parse(&b) {
                Ok(Packet::Control(c)) if matches!(c.body, Control::Nak(_)) => Some((b, c)),
                _ => None,
            })
            .collect()
    }

    /// A receiver whose path reorders, with `gaps` gaps of three packets
    /// (8 bytes apiece in a loss list) found at once and so held together,
    /// and the NAK datagrams sent when the hold ends: the first timer tick
    /// that sends any. Also the `naks_sent` before and after.
    fn held_burst(r: &mut Receiver, gaps: u32) -> (Vec<(Vec<u8>, ControlPacket)>, u64, u64) {
        let t = adapt(r) + MS;
        for k in 1..=gaps {
            orig(r, 2 + 4 * k, t, t);
        }
        let _ = raw_naks(r);
        let before = r.stats().naks_sent;
        for _ in 0..100_000 {
            let at = r.next_timer().max(r.now);
            r.on_timer(at);
            let naks = raw_naks(r);
            if !naks.is_empty() {
                return (naks, before, r.stats().naks_sent);
            }
        }
        panic!("the timer does not advance");
    }

    fn missing(k: u32) -> (Seq, Seq) {
        (Seq::new(ISN + 4 * k - 1), Seq::new(ISN + 4 * k + 1))
    }

    #[test]
    fn nak_budget_leaves_room_for_ipv6_and_clamps_the_mtu() {
        assert_eq!(nak_list_budget(1500), 1500 - 48 - 16);
        assert_eq!(nak_list_budget(9000), 1436);
        assert_eq!(nak_list_budget(1200), 1200 - 48 - 16);
        assert_eq!(nak_list_budget(0), 576 - 48 - 16);
    }

    #[test]
    fn a_worst_case_nak_batch_is_split_to_fit_the_path() {
        let mut r = rx2(true);
        let (naks, before, after) = held_burst(&mut r, 256);
        assert_eq!(naks.len(), 2, "256 ranges of 8 bytes do not fit one");
        assert_eq!(after - before, 2, "naks_sent counts packets");
        let mut got = Vec::new();
        for (bytes, c) in &naks {
            // IPv6 and UDP headers on top still fit a 1500-byte path.
            assert!(bytes.len() + 48 <= 1500, "{} bytes", bytes.len());
            if let Control::Nak(ranges) = &c.body {
                got.extend(ranges.iter().copied());
            }
        }
        let expected: Vec<_> = (1..=256).map(missing).collect();
        assert_eq!(got, expected, "every range, once, in order");
    }

    #[test]
    fn a_nak_batch_that_fits_is_one_unchanged_packet() {
        let mut r = rx2(true);
        let (naks, before, after) = held_burst(&mut r, 5);
        assert_eq!(naks.len(), 1);
        assert_eq!(after - before, 1);
        let (bytes, c) = &naks[0];
        let Control::Nak(ranges) = &c.body else {
            unreachable!()
        };
        assert_eq!(ranges, &(1..=5).map(missing).collect::<Vec<_>>());
        let mut whole = Vec::new();
        ControlPacket::new(c.timestamp, 77, Control::Nak(ranges.clone())).encode(&mut whole);
        assert_eq!(bytes, &whole);
    }

    #[test]
    fn a_small_negotiated_mtu_splits_the_nak_further() {
        let mut r = Receiver::new(
            Settings {
                latency: 2_000 * MS,
                buffer: 32_768,
                mtu: 600,
                ..settings()
            },
            0,
            77,
            Seq::new(ISN),
            0,
        );
        let (naks, _, _) = held_burst(&mut r, 100);
        assert!(naks.len() > 1);
        assert!(naks.iter().all(|(b, _)| b.len() + 48 <= 600));
        let got: Vec<_> = naks
            .iter()
            .flat_map(|(_, c)| match &c.body {
                Control::Nak(r) => r.clone(),
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(got, (1..=100).map(missing).collect::<Vec<_>>());
    }

    #[test]
    fn splitting_a_loss_list_keeps_every_range_in_order_within_the_budget() {
        let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
        for round in 0..2_000 {
            let n = rng.below(300) as usize;
            let ranges: Vec<(Seq, Seq)> = (0..n)
                .map(|_| {
                    let a = rng.below(1 << 30) as u32;
                    let b = if rng.below(2) == 0 {
                        a
                    } else {
                        a + 1 + rng.below(1000) as u32
                    };
                    (Seq::new(a), Seq::new(b))
                })
                .collect();
            let budget = match round % 3 {
                0 => nak_list_budget(1500),
                1 => nak_list_budget(1 + rng.below(2000) as u32),
                _ => 8 + rng.below(1500) as usize,
            };
            let runs: Vec<_> = split_loss_list(&ranges, budget).collect();
            assert_eq!(runs.concat(), ranges, "round {round}");
            let encoded = |run: &[(Seq, Seq)]| {
                let mut out = Vec::new();
                crate::packet::encode_loss_list(run, &mut out);
                out.len()
            };
            for (i, run) in runs.iter().enumerate() {
                assert!(!run.is_empty());
                assert!(encoded(run) <= budget, "round {round}");
                // No more runs than needed: the next range would not fit.
                if let Some(next) = runs.get(i + 1).and_then(|r| r.first()) {
                    assert!(encoded(run) + encoded(&[*next]) > budget, "round {round}");
                }
            }
            if encoded(&ranges) <= budget {
                assert!(runs.len() <= 1, "a list that fits stays whole");
            }
        }
    }
}
