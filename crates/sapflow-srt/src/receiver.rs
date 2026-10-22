//! One connection's receiving side, after the handshake: the receive
//! buffer, timestamp-based delivery (TSBPD) with too-late packet drop, ACKs,
//! NAKs, RTT, keepalives and the idle timeout.
//!
//! Times are microseconds on the listener's clock (`i64`, never negative in
//! practice); packet timestamps are extended to 64 bits so nothing here
//! wraps except the sequence numbers, which are compared circularly.

use std::collections::VecDeque;

use crate::crypto::Decryptor;
use crate::loss::{LossList, Timing};
use crate::packet::{Ack, AckStats, Control, ControlPacket, DataPacket, MAX_LOSS_ENTRIES};
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
    gaps: VecDeque<i64>,
}

impl PacketPairs {
    fn add(&mut self, seq: Seq, now: i64) {
        if let Some((s, t)) = self.last
            && s.next() == seq
        {
            if self.gaps.len() >= 16 {
                self.gaps.pop_front();
            }
            self.gaps.push_back((now - t).max(1));
        }
        self.last = Some((seq, now));
    }

    /// Packets per second; 1000 before there is anything to go on (as
    /// libsrt reports at first, observed).
    fn capacity(&self) -> u32 {
        if self.gaps.is_empty() {
            return 1000;
        }
        let mut g: Vec<i64> = self.gaps.iter().copied().collect();
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
    /// The first sequence number not yet received (what an ACK reports).
    ack_pos: Seq,
    loss: LossList,
    rtt: Rtt,
    next_ack_number: u32,
    last_ack_sent: Seq,
    last_full_ack: i64,
    unanswered: VecDeque<(u32, i64)>,
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
    deliver: VecDeque<Vec<u8>>,
    crypto: Option<Decryptor>,
    next_km: i64,
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
            ack_pos: initial_seq,
            loss: LossList::new(),
            rtt: Rtt::new(),
            next_ack_number: 1,
            last_ack_sent: initial_seq,
            last_full_ack: now,
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
            crypto: None,
            next_km: 0,
        }
    }

    /// The connection's counters so far.
    pub fn stats(&self) -> Stats {
        self.stats
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

    /// Takes the next payload due for delivery, in sequence order, if any.
    pub fn take_delivery(&mut self) -> Option<Vec<u8>> {
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
    /// buffers it, reports any gap in a NAK, sends a light ACK after enough
    /// packets and releases what is due.
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
            if crypto.decrypt(p.key, p.seq, &mut payload).is_err() {
                self.stats.encrypted += 1;
                return;
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
            self.loss.remove(p.seq);
        } else {
            if idx > self.buf.len() {
                // A gap: report it at once.
                let first = self.head.add(self.buf.len() as u32);
                let last = p.seq.prev();
                self.stats.lost += first.distance_to(last) as u64 + 1;
                self.loss.add(first, last, now, deliver_at);
                self.stats.naks_sent += 1;
                self.queue(now, Control::Nak(vec![(first, last)]));
                self.buf.resize_with(idx, || None);
            }
            self.buf.push_back(Some(slot));
        }
        self.advance_ack_pos();
        self.since_ack += 1;
        if self.since_ack >= LIGHT_ACK_PACKETS && self.ack_pos != self.last_ack_sent {
            self.light_ack(now);
        }
        self.nak_due(now);
        self.release(now);
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
        match self.buf.pop_front() {
            Some(Some(slot)) => {
                self.stats.delivered += 1;
                self.deliver.push_back(slot.payload);
            }
            Some(None) => self.stats.dropped += 1,
            None => {}
        }
        self.head = self.head.next();
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
    /// is skipped once a later one is due (too-late packet drop).
    fn release(&mut self, now: i64) {
        loop {
            match self.buf.front() {
                Some(Some(slot)) if slot.deliver_at <= now => self.pop_front(),
                Some(None) => {
                    let due = self
                        .buf
                        .iter()
                        .position(Option::is_some)
                        .and_then(|i| Some((i, self.buf.get(i)?.as_ref()?.deliver_at)));
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
        if !ranges.is_empty() {
            self.stats.naks_sent += 1;
            self.queue(now, Control::Nak(ranges));
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
        self.since_ack = 0;
        self.stats.acks_sent += 1;
        if self.unanswered.len() >= ACKS_REMEMBERED {
            self.unanswered.pop_front();
        }
        self.unanswered.push_back((number, now));
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
    /// those packets and a key material refresh is answered (at most one per
    /// 100 ms). Any other control packet only shows the caller is alive.
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
            // One bounded unwrap and response per 100ms per connection;
            // repeated or hostile KM cannot drive KDF work or queue growth.
            if now < self.next_km {
                return;
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
                if let Some(i) = self.unanswered.iter().position(|(n, _)| n == number) {
                    if let Some((_, sent)) = self.unanswered.get(i).copied() {
                        self.rtt.sample(now - sent);
                    }
                    self.unanswered.drain(..=i);
                }
            }
            Control::Shutdown => {
                self.flush();
                self.closed = Some(CloseReason::Shutdown);
            }
            Control::DropRequest { first, last, .. } => {
                // The sender no longer has these: stop asking for them.
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
        self.release(now);
        if self.ack_pos != self.last_ack_sent && now - self.last_full_ack >= ACK_INTERVAL {
            self.full_ack(now);
        }
        self.nak_due(now);
        if now - self.last_sent >= KEEPALIVE_INTERVAL {
            self.queue(now, Control::KeepAlive);
        }
    }

    /// When [`on_timer`](Self::on_timer) next has something to do.
    pub fn next_timer(&self) -> i64 {
        let mut t = self
            .last_received
            .saturating_add(self.settings.peer_idle)
            .min(self.last_sent + KEEPALIVE_INTERVAL);
        if self.ack_pos != self.last_ack_sent {
            t = t.min(self.last_full_ack + ACK_INTERVAL);
        }
        if let Some(at) = self.loss.next_due(self.now, self.timing()) {
            t = t.min(at);
        }
        if let Some(at) = self
            .buf
            .iter()
            .find_map(|s| s.as_ref().map(|s| s.deliver_at))
        {
            t = t.min(at);
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
        std::iter::from_fn(|| r.take_delivery()).collect()
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
}
