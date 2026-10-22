//! Bounded live sending window (SRT draft sections 4.8 and 5.1).
//!
//! This first sender uses an explicit bandwidth ceiling. It fails the
//! session when retained media reaches its age budget; negotiated late
//! dropping and automatic bitrate adaptation are subsequent milestones.

use std::collections::VecDeque;

use crate::crypto::Encryptor;
use crate::packet::{Ack, Control, ControlPacket, DataPacket, Position};
use crate::rtt::Rtt;
use crate::seq::Seq;

/// Limits cover pending AND unacknowledged payloads. Payload ownership is
/// transferred into the queue, retained once, and copied once per wire send.
#[derive(Debug, Clone)]
pub struct SendConfig {
    /// Most packets held at once, queued or awaiting acknowledgement; default
    /// 4096.
    pub max_packets: usize,
    /// Sum of retained payload allocation capacities (metadata is packet-bounded).
    pub max_bytes: usize,
    /// Maximum age since enqueue, in microseconds. Expiry closes the session.
    pub max_age_us: u64,
    /// One complete live message per datagram (1316 fits seven TS packets).
    pub max_payload: usize,
    /// Bytes/second including the SRT header, shared by originals and retries.
    pub bandwidth: u64,
    /// Per-packet retransmissions before ending the session.
    pub max_retransmits: u32,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            max_packets: 4096,
            max_bytes: 4 * 1024 * 1024,
            max_age_us: 3_000_000,
            max_payload: 1316,
            bandwidth: 1_250_000,
            max_retransmits: 32,
        }
    }
}

impl SendConfig {
    pub(crate) fn valid(&self) -> bool {
        (1..=65_536).contains(&self.max_packets)
            && (1..=64 * 1024 * 1024).contains(&self.max_bytes)
            && (1..=60_000_000).contains(&self.max_age_us)
            && (1..=1456).contains(&self.max_payload)
            && (1..=1_000_000_000).contains(&self.bandwidth)
            && (1..=1024).contains(&self.max_retransmits)
    }
}

/// Why a message was not accepted for sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The session is not open: not yet connected, or already closed or
    /// failed.
    NotConnected,
    /// The message has no bytes.
    Empty,
    /// The message is longer than [`SendConfig::max_payload`].
    PayloadTooLarge,
    /// The packet or byte budget is used up (backpressure).
    QueueFull,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotConnected => "SRT sender is not connected",
            Self::Empty => "empty live message",
            Self::PayloadTooLarge => "live message exceeds negotiated payload size",
            Self::QueueFull => "SRT packet or byte budget exhausted",
        })
    }
}
impl std::error::Error for SendError {}

/// Counters for the sender. Byte counts are payload bytes, without the SRT
/// header, unless noted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendStats {
    /// Messages accepted into the send queue.
    pub accepted_packets: u64,
    /// Payload bytes in accepted messages.
    pub accepted_bytes: u64,
    /// Data packets put on the wire, originals and retransmissions.
    pub sent_packets: u64,
    /// Payload bytes in those packets.
    pub sent_bytes: u64,
    /// Of those, retransmissions.
    pub retransmitted_packets: u64,
    /// Payload bytes in retransmissions.
    pub retransmitted_bytes: u64,
    /// Transport acknowledgements, not proof of playback or application receipt.
    pub acknowledged_packets: u64,
    /// Payload bytes acknowledged.
    pub acknowledged_bytes: u64,
    /// Messages refused with a [`SendError`].
    pub rejected_packets: u64,
    /// Packets still queued or unacknowledged when the session closed.
    pub abandoned_packets: u64,
    /// Payload bytes in those packets.
    pub abandoned_bytes: u64,
    /// Packets queued or awaiting acknowledgement now.
    pub buffered_packets: usize,
    /// Allocation capacity held by those payloads, in bytes; what
    /// [`SendConfig::max_bytes`] limits.
    pub buffered_bytes: usize,
    /// Highest `buffered_packets` reached.
    pub peak_buffered_packets: usize,
    /// Highest `buffered_bytes` reached.
    pub peak_buffered_bytes: usize,
    /// Valid NAK packets received.
    pub nak_packets: u64,
    /// Control packets ignored as invalid or unsupported.
    pub invalid_controls: u64,
    /// Packets queued for retransmission by the retransmission timeout rather
    /// than by a NAK.
    pub timeout_retransmissions: u64,
    /// Smoothed round-trip time, in microseconds.
    pub rtt_us: u64,
    /// New data packets encrypted; retransmissions reuse the ciphertext and
    /// are not counted.
    pub encrypted_packets: u64,
    /// Key material requests sent, repeats included.
    pub km_requests: u64,
    /// Key material responses that matched the outstanding request.
    pub km_responses: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    MediaExpired,
    Crypto,
    RetryLimit,
    PeerClosed,
    PeerError(u32),
}

struct Queued {
    payload: Vec<u8>,
    born: i64,
}

struct Flight {
    data: Queued,
    seq: Seq,
    message: u32,
    last_sent: i64,
    retries: u32,
    requested: bool,
    key: u8,
}

pub(crate) struct Sender {
    config: SendConfig,
    pending: VecDeque<Queued>,
    flight: VecDeque<Flight>,
    retries: VecDeque<Seq>,
    next_seq: Seq,
    next_message: u32,
    acked: Seq,
    peer_socket: u32,
    flow_window: usize,
    peer_space: usize,
    next_send: i64,
    last_send: i64,
    last_receive: i64,
    next_rto: Option<i64>,
    rto_count: u32,
    rtt: Rtt,
    peer_variance: i64,
    ack_ack: Option<u32>,
    stats: SendStats,
    failure: Option<Failure>,
    crypto: Option<Encryptor>,
    key_counts: [usize; 2],
}

impl Sender {
    pub(crate) fn new(
        config: SendConfig,
        now: i64,
        peer_socket: u32,
        initial: Seq,
        window: usize,
    ) -> Self {
        Self {
            config,
            pending: VecDeque::new(),
            flight: VecDeque::new(),
            retries: VecDeque::new(),
            next_seq: initial,
            next_message: 1,
            acked: initial,
            peer_socket,
            flow_window: window,
            peer_space: window,
            next_send: now,
            last_send: now,
            last_receive: now,
            next_rto: None,
            rto_count: 0,
            rtt: Rtt::new(),
            peer_variance: 50_000,
            ack_ack: None,
            stats: SendStats::default(),
            failure: None,
            crypto: None,
            key_counts: [0; 2],
        }
    }

    pub(crate) fn set_crypto(&mut self, crypto: Option<Encryptor>) {
        self.crypto = crypto;
    }

    pub(crate) fn stats(&self) -> SendStats {
        let mut s = self.stats;
        s.rtt_us = self.rtt.rtt() as u64;
        s
    }

    pub(crate) fn peer_socket(&self) -> u32 {
        self.peer_socket
    }
    pub(crate) fn last_receive(&self) -> i64 {
        self.last_receive
    }
    pub(crate) fn failure(&self) -> Option<Failure> {
        self.failure
    }
    pub(crate) fn max_payload(&self) -> usize {
        self.config.max_payload
    }

    pub(crate) fn try_enqueue(
        &mut self,
        now: i64,
        payload: Vec<u8>,
    ) -> Result<(), (SendError, Vec<u8>)> {
        let error = if self.failure.is_some() {
            Some(SendError::NotConnected)
        } else if payload.is_empty() {
            Some(SendError::Empty)
        } else if payload.len() > self.config.max_payload {
            Some(SendError::PayloadTooLarge)
        } else if self.stats.buffered_packets >= self.config.max_packets
            || payload.capacity()
                > self
                    .config
                    .max_bytes
                    .saturating_sub(self.stats.buffered_bytes)
        {
            Some(SendError::QueueFull)
        } else {
            None
        };
        if let Some(error) = error {
            self.stats.rejected_packets += 1;
            return Err((error, payload));
        }
        self.stats.accepted_packets += 1;
        self.stats.accepted_bytes += payload.len() as u64;
        self.stats.buffered_packets += 1;
        self.stats.buffered_bytes += payload.capacity();
        self.stats.peak_buffered_packets = self
            .stats
            .peak_buffered_packets
            .max(self.stats.buffered_packets);
        self.stats.peak_buffered_bytes = self
            .stats
            .peak_buffered_bytes
            .max(self.stats.buffered_bytes);
        self.pending.push_back(Queued { payload, born: now });
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        self.stats.abandoned_packets += self.stats.buffered_packets as u64;
        self.stats.abandoned_bytes += self
            .pending
            .iter()
            .map(|p| p.payload.len() as u64)
            .chain(self.flight.iter().map(|f| f.data.payload.len() as u64))
            .sum::<u64>();
        self.crypto = None;
        self.key_counts = [0; 2];
        self.stats.buffered_packets = 0;
        self.stats.buffered_bytes = 0;
        self.pending = VecDeque::new();
        self.flight = VecDeque::new();
        self.retries = VecDeque::new();
        self.ack_ack = None;
        self.next_rto = None;
    }

    fn rto(&self) -> i64 {
        self.rtt
            .retry()
            .max(self.rtt.rtt() + 4 * self.peer_variance)
            + 20_000
    }

    pub(crate) fn on_control(&mut self, now: i64, control: &Control) {
        if self.failure.is_some() {
            return;
        }
        let valid = match control {
            Control::Ack(ack) => self.on_ack(now, ack),
            Control::Nak(ranges) => self.on_nak(now, ranges),
            Control::Other {
                control_type: 0x7fff,
                subtype: 4,
                info: 0,
                cif,
            } => {
                let confirmed = self
                    .crypto
                    .as_mut()
                    .is_some_and(|crypto| crypto.confirm(cif));
                if confirmed {
                    self.stats.km_responses += 1;
                }
                confirmed
            }
            Control::KeepAlive => true,
            Control::Shutdown => {
                self.failure = Some(Failure::PeerClosed);
                true
            }
            Control::PeerError { code } => {
                self.failure = Some(Failure::PeerError(*code));
                true
            }
            // File-mode congestion notifications are not LiveCC signals.
            _ => false,
        };
        if valid {
            self.last_receive = now;
        } else {
            self.stats.invalid_controls += 1;
        }
    }

    fn on_ack(&mut self, now: i64, ack: &Ack) -> bool {
        let advance = self.acked.distance_to(ack.ack);
        if advance < 0 || ack.ack.distance_to(self.next_seq) < 0 {
            return false;
        }
        if ack.number != 0 && ack.stats.is_some() {
            self.ack_ack = Some(ack.number);
        }
        if let Some(stats) = ack.stats {
            self.peer_space = (stats.buffer as usize).min(self.flow_window);
            self.rtt.sample(i64::from(stats.rtt));
            self.peer_variance = i64::from(stats.rtt_var).min(60_000_000);
        }
        while self
            .flight
            .front()
            .is_some_and(|f| f.seq.distance_to(ack.ack) > 0)
        {
            if let Some(f) = self.flight.pop_front() {
                if let Some(count) = f
                    .key
                    .checked_sub(1)
                    .and_then(|k| self.key_counts.get_mut(k as usize))
                {
                    *count = count.saturating_sub(1);
                }
                self.stats.acknowledged_packets += 1;
                self.stats.acknowledged_bytes += f.data.payload.len() as u64;
                self.stats.buffered_packets -= 1;
                self.stats.buffered_bytes -= f.data.payload.capacity();
            }
        }
        self.acked = ack.ack;
        self.retries.retain(|seq| seq.distance_to(ack.ack) <= 0);
        if advance > 0 {
            self.rto_count = 0;
            self.next_rto = (!self.flight.is_empty()).then(|| now.saturating_add(self.rto()));
        }
        true
    }

    fn on_nak(&mut self, now: i64, ranges: &[(Seq, Seq)]) -> bool {
        // Validate every range before changing state. Never enumerate an
        // attacker-selected range: intersect it with our retained window.
        if ranges.is_empty()
            || ranges
                .iter()
                .any(|(a, b)| a.distance_to(*b) < 0 || b.distance_to(self.next_seq) <= 0)
        {
            return false;
        }
        self.stats.nak_packets += 1;
        let Some(first) = self.flight.front().map(|f| f.seq) else {
            return true;
        };
        let mut work_left = self.config.max_packets;
        for (a, b) in ranges {
            let start = first.distance_to(*a).max(0) as usize;
            let end = first.distance_to(*b);
            if end < 0 {
                continue;
            }
            let end = (end as usize).min(self.flight.len().saturating_sub(1));
            for index in start..=end {
                if work_left == 0 {
                    return true;
                }
                work_left -= 1;
                if let Some(f) = self.flight.get_mut(index) {
                    // An initial NAK is urgent; repeated requests must not
                    // race the retransmission already on its way.
                    if !f.requested
                        && (f.retries == 0
                            || now.saturating_sub(f.last_sent) >= self.rtt.rtt().max(1000))
                    {
                        f.requested = true;
                        self.retries.push_back(f.seq);
                    }
                }
            }
        }
        true
    }

    pub(crate) fn on_timer(&mut self, now: i64) {
        if self.failure.is_some() {
            return;
        }
        if self
            .oldest()
            .is_some_and(|born| now.saturating_sub(born) >= self.config.max_age_us as i64)
        {
            self.failure = Some(Failure::MediaExpired);
            return;
        }
        if self.next_rto.is_some_and(|deadline| now >= deadline) {
            self.rto_count = self.rto_count.saturating_add(1);
            for f in &mut self.flight {
                if !f.requested {
                    f.requested = true;
                    self.retries.push_back(f.seq);
                    self.stats.timeout_retransmissions += 1;
                }
            }
            self.next_rto =
                Some(now.saturating_add(self.rto().saturating_mul(i64::from(self.rto_count + 1))));
        }
    }

    fn oldest(&self) -> Option<i64> {
        self.flight
            .front()
            .map(|f| f.data.born)
            .into_iter()
            .chain(self.pending.front().map(|p| p.born))
            .min()
    }

    pub(crate) fn next_timer(&self) -> i64 {
        let mut next = self.last_send.saturating_add(1_000_000);
        if self.ack_ack.is_some() {
            next = next.min(self.last_receive);
        }
        if !self.retries.is_empty()
            || (!self.pending.is_empty()
                && self.flight.len() < self.flow_window
                && self.peer_space > 0)
        {
            next = next.min(self.next_send);
        }
        if let Some(born) = self.oldest() {
            next = next.min(born.saturating_add(self.config.max_age_us as i64));
        }
        if let Some(at) = self.crypto.as_ref().and_then(Encryptor::deadline) {
            next = next.min(at);
        }
        if let Some(rto) = self.next_rto {
            next = next.min(rto);
        }
        next
    }

    pub(crate) fn poll_transmit(&mut self, now: i64) -> Option<Vec<u8>> {
        self.on_timer(now);
        if self.failure.is_some() {
            return None;
        }
        if let Some(km) = self.crypto.as_mut().and_then(|c| c.poll_km(now)) {
            self.stats.km_requests += 1;
            return Some(self.control(
                now,
                Control::Other {
                    control_type: 0x7fff,
                    subtype: 3,
                    info: 0,
                    cif: km,
                },
            ));
        }
        if let Some(number) = self.ack_ack.take() {
            return Some(self.control(now, Control::AckAck { number }));
        }
        if now >= self.next_send {
            while let Some(seq) = self.retries.pop_front() {
                let first = self.flight.front()?.seq;
                let index = first.distance_to(seq);
                if index < 0 {
                    continue;
                }
                let Some(f) = self.flight.get_mut(index as usize) else {
                    continue;
                };
                f.requested = false;
                if f.retries >= self.config.max_retransmits {
                    self.failure = Some(Failure::RetryLimit);
                    return None;
                }
                f.retries += 1;
                f.last_sent = now;
                let len = f.data.payload.len();
                let out = encode_data(f, self.peer_socket, true);
                self.stats.retransmitted_packets += 1;
                self.stats.retransmitted_bytes += len as u64;
                self.sent(now, len);
                return Some(out);
            }
            if self.flight.len() < self.flow_window
                && self.peer_space > 0
                && let Some(mut data) = self.pending.pop_front()
            {
                let retained = self
                    .key_counts
                    .iter()
                    .enumerate()
                    .fold(0, |bits, (i, count)| {
                        bits | if *count > 0 { 1 << i } else { 0 }
                    });
                let key = if let Some(crypto) = &mut self.crypto {
                    match crypto.encrypt(self.next_seq, &mut data.payload, retained) {
                        Ok(key) => key,
                        Err(_) => {
                            self.pending.push_front(data);
                            self.failure = Some(Failure::Crypto);
                            return None;
                        }
                    }
                } else {
                    0
                };
                if let Some(count) = key
                    .checked_sub(1)
                    .and_then(|k| self.key_counts.get_mut(k as usize))
                {
                    *count += 1;
                }
                if key != 0 {
                    self.stats.encrypted_packets += 1;
                }
                let f = Flight {
                    key,
                    data,
                    seq: self.next_seq,
                    message: self.next_message,
                    last_sent: now,
                    retries: 0,
                    requested: false,
                };
                self.next_seq = self.next_seq.next();
                self.next_message = (self.next_message + 1) & 0x03ff_ffff;
                if self.next_message == 0 {
                    self.next_message = 1;
                }
                let len = f.data.payload.len();
                let out = encode_data(&f, self.peer_socket, false);
                self.flight.push_back(f);
                self.peer_space -= 1;
                if self.next_rto.is_none() {
                    self.next_rto = Some(now.saturating_add(self.rto()));
                }
                self.sent(now, len);
                return Some(out);
            }
        }
        if now >= self.last_send.saturating_add(1_000_000) {
            return Some(self.control(now, Control::KeepAlive));
        }
        None
    }

    fn sent(&mut self, now: i64, len: usize) {
        self.stats.sent_packets += 1;
        self.stats.sent_bytes += len as u64;
        self.last_send = now;
        // Charge actual packet size, conservatively including the SRT
        // header. No catch-up burst after a suspended or delayed executor.
        let period = ((len as u64 + 16) * 1_000_000).div_ceil(self.config.bandwidth);
        self.next_send = now.saturating_add(period.max(1) as i64);
    }

    fn control(&mut self, now: i64, body: Control) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16);
        ControlPacket::new(now as u32, self.peer_socket, body).encode(&mut bytes);
        self.last_send = now;
        bytes
    }
}

fn encode_data(f: &Flight, dest: u32, retransmitted: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + f.data.payload.len());
    DataPacket {
        seq: f.seq,
        position: Position::Only,
        in_order: false,
        key: f.key,
        retransmitted,
        message: f.message,
        timestamp: f.data.born as u32,
        dest,
        payload: &f.data.payload,
    }
    .encode(&mut bytes);
    bytes
}
