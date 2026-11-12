//! Entry points shared by the fuzz targets (`fuzz/fuzz_targets/`) and the
//! seed regression test, so committed seeds run through exactly the code
//! the fuzzer drives even where cargo-fuzz is not installed.
//!
//! - [`stun_message`]: a selector byte, then either raw bytes through the
//!   parser and the agent, or attribute bytes wrapped in an honest header,
//!   USERNAME, MESSAGE-INTEGRITY and FINGERPRINT, so the agent's
//!   authenticated paths are reached.
//! - [`sdp_offer`]: an offer through the parser, then the answer, then the
//!   answer back through the parser.
//! - [`rfc4571_frames`]: a chunking byte, then a stream through the
//!   decoder.
//! - [`whip_session`]: a scripted run of a [`Session`] against the scripted
//!   publisher: raw, mutated and honest datagrams from two tuples, checks
//!   with and without USE-CANDIDATE or ICE-CONTROLLED, DTLS flights, SRTP
//!   and SRTCP, time, and captured checks and copies of fresh SRTP and SRTCP
//!   delivered from another tuple before or after their originals, with an
//!   ID cache of 16 so that checks are forgotten and room for 3 validated
//!   tuples of the 6 it uses so that tuples are evicted. After every
//!   datagram in and every one out: every tuple has been sent at most
//!   `amplification_limit` times what it sent (STUN responses included); a
//!   tuple never validated only a STUN error response to the request it
//!   just sent, no larger; DTLS and RTCP only to the send tuple; none over
//!   1500 bytes; an evicted tuple had the stalest media of those that could
//!   go, the earliest validated breaking ties. After every step: at most
//!   [`MAX_DATAGRAMS_PER_STEP`] datagrams, the selected and send tuples
//!   validated, and no timer once ended. A tuple never validated gets
//!   nothing at all for an unauthenticated datagram, and a check re-sent
//!   from another tuple while its ID is remembered draws nothing (A3.7).
//! - [`whip_router`]: two publishers' sessions in one [`Router`], with four
//!   strangers sending raw bytes and copies of anything sent so far, checks
//!   sent from the other publisher's tuple, DTLS, media, time and closes.
//!   After every datagram in and every one out: every tuple has been sent
//!   at most `amplification_limit` times what it sent, across sessions; a
//!   stranger that has only sent its own bytes (raw bytes, or checks whose
//!   MESSAGE-INTEGRITY is corrupted) gets nothing; none over
//!   1500 bytes. After every step: the deadline index holds each session's
//!   deadline, every owned tuple's owner is live, and a closed session is
//!   gone (A3.7).

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]

use crate::Transport;
use crate::framing::{self, Decoder, Framing};
use crate::ice::{Agent, Credentials, IceConfig, Kind, Tuple, classify};
use crate::resource::SessionId;
use crate::router::{Router, RouterConfig};
use crate::sdp::{self, AnswerConfig, Candidate, Offer};
use crate::session::{Event, Session, SessionConfig, State, Transmit};
use crate::stun::{self, Builder, Message};
use crate::testing::{self, Publisher, tuple};
use sapflow_dtls::Fingerprint;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Most script steps read from one input.
pub const MAX_STEPS: usize = 64;
/// Most datagrams one step may make the session send: its output queue
/// (`SessionConfig::max_queued`, 64 by default), whatever the step does.
pub const MAX_DATAGRAMS_PER_STEP: usize = 64;

/// Splits `[op, len_hi, len_lo, bytes]` steps, stopping at a truncated one.
pub fn steps(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let (&op, tail) = rest.split_first()?;
        let (len, tail) = tail.split_first_chunk::<2>()?;
        let len = usize::from(u16::from_be_bytes(*len));
        let bytes = tail.get(..len)?;
        rest = &tail[len..];
        Some((op, bytes))
    })
    .take(MAX_STEPS)
}

/// Appends one step to `out`.
pub fn push_step(out: &mut Vec<u8>, op: u8, bytes: &[u8]) {
    out.push(op);
    out.extend_from_slice(&u16::try_from(bytes.len()).expect("fits").to_be_bytes());
    out.extend_from_slice(bytes);
}

fn fixed_credentials() -> Credentials {
    Credentials::generate_seeded([0x42; 32]).unwrap()
}

/// One STUN message. Selector 0 to 127: `data` is a raw datagram. 128 and
/// up: `data` is attribute bytes, given an honest header, USERNAME,
/// MESSAGE-INTEGRITY and FINGERPRINT. Returns whether the agent answered.
pub fn stun_message(data: &[u8]) -> bool {
    let Some((&selector, body)) = data.split_first() else {
        return false;
    };
    let creds = fixed_credentials();
    let datagram = if selector < 128 {
        body.to_vec()
    } else {
        let username = format!("{}:peer", creds.ufrag());
        let mut b = Builder::new(stun::BINDING_REQUEST, &[selector; 12])
            .attribute(stun::attr::USERNAME, username.as_bytes())
            .finish();
        // The body as attributes; a length field that runs over is the
        // parser's to refuse.
        b.extend_from_slice(body.get(..body.len() & !3).unwrap_or_default());
        let len = u16::try_from(b.len() - stun::HEADER_LEN).unwrap_or(0);
        b[2..4].copy_from_slice(&len.to_be_bytes());
        rebuild_signed(&b, creds.pwd().as_bytes())
    };
    if let Ok(m) = Message::parse(&datagram) {
        assert!(m.attributes <= stun::MAX_ATTRIBUTES);
        assert!(m.unknown_required.len() <= stun::MAX_UNKNOWN);
        let _ = m.check_integrity(&datagram, creds.pwd().as_bytes());
        let _ = m.check_fingerprint(&datagram);
    }
    let _ = stun::request_username(&datagram);
    let mut agent = Agent::new(IceConfig::default(), &creds, "peer");
    let from = Tuple::udp(if selector & 1 == 0 {
        "192.0.2.1:5000".parse().unwrap()
    } else {
        "[2001:db8::1]:5000".parse().unwrap()
    });
    let Some(answer) = agent.handle(Instant::now(), from, &datagram) else {
        assert!(!agent.is_validated(&from));
        return false;
    };
    assert!(answer.response.len() <= datagram.len(), "never larger");
    let m = Message::parse(&answer.response).expect("our answer parses");
    assert!(m.check_integrity(&answer.response, creds.pwd().as_bytes()));
    assert!(m.check_fingerprint(&answer.response));
    assert_eq!(agent.is_validated(&from), answer.success);
    if answer.success {
        assert_eq!(m.xor_mapped, Some(from.remote));
    }
    true
}

/// Re-signs a message whose attributes are already in place: appends
/// MESSAGE-INTEGRITY and FINGERPRINT if the attributes walk cleanly.
fn rebuild_signed(b: &[u8], key: &[u8]) -> Vec<u8> {
    let Ok(t) = <[u8; 12]>::try_from(&b[8..20]) else {
        return b.to_vec();
    };
    let mut out = Builder::new(u16::from_be_bytes([b[0], b[1]]), &t);
    let mut at = stun::HEADER_LEN;
    while at + 4 <= b.len() {
        let ty = u16::from_be_bytes([b[at], b[at + 1]]);
        let len = usize::from(u16::from_be_bytes([b[at + 2], b[at + 3]]));
        let Some(v) = b.get(at + 4..at + 4 + len) else {
            // Not walkable: send it as it is, unsigned.
            return b.to_vec();
        };
        if ty == stun::attr::MESSAGE_INTEGRITY || ty == stun::attr::FINGERPRINT {
            break;
        }
        out = out.attribute(ty, v);
        at += 4 + len.next_multiple_of(4);
    }
    out.integrity(key).fingerprint()
}

/// One offer. Returns whether it was answered.
pub fn sdp_offer(data: &[u8]) -> bool {
    let Ok(offer) = Offer::parse(data) else {
        return false;
    };
    assert!(offer.sections.len() <= sdp::Limits::default().max_sections);
    let creds = fixed_credentials();
    let candidates = [
        Candidate {
            transport: Transport::Udp,
            address: "192.0.2.1:47200".parse().unwrap(),
        },
        Candidate {
            transport: Transport::Tcp,
            address: "[2001:db8::1]:47201".parse().unwrap(),
        },
    ];
    let config = AnswerConfig {
        credentials: &creds,
        fingerprint: Fingerprint::from_bytes([7; 32]),
        candidates: &candidates,
        opus_max_bitrate: 128_000,
        origin: 1,
    };
    let Ok((answer, n)) = sdp::answer(&offer, &config) else {
        return false;
    };
    assert!(n.video.is_some() || n.audio.is_some());
    assert!((1..=sapflow_dtls::MAX_EXPECTED_FINGERPRINTS).contains(&n.fingerprints.len()));
    // Our answer repeats some of the offer's lines and adds our own, so it
    // can be longer than the offer cap: read it back with room to spare.
    let room = sdp::Limits {
        max_offer: 64 * 1024,
        max_lines: 4096,
        ..sdp::Limits::default()
    };
    let back = Offer::parse_with(answer.as_bytes(), &room).expect("our answer parses");
    assert!(back.ice_lite);
    assert_eq!(back.sections.len(), offer.sections.len());
    assert_eq!(back.bundles.len(), 1);
    for s in &back.sections {
        if s.port != 0 {
            assert_eq!(back.direction(s), sdp::Direction::RecvOnly);
            assert_eq!(back.setup(s), Some(sdp::Setup::Passive));
        }
    }
    true
}

/// A byte stream through the RFC 4571 decoder, pushed in chunks of the
/// first byte's size. Returns the frames read.
pub fn rfc4571_frames(data: &[u8]) -> usize {
    let Some((&chunk, stream)) = data.split_first() else {
        return 0;
    };
    let framing = Framing {
        max_frame: 1500,
        max_buffered: 4096,
    };
    let mut d = Decoder::new(framing);
    let mut frames = 0;
    let mut reencoded = Vec::new();
    for c in stream.chunks(usize::from(chunk).max(1)) {
        if d.push(c).is_err() {
            break;
        }
        assert!(d.buffered() <= framing.max_buffered);
        loop {
            match d.pop() {
                Ok(Some(f)) => {
                    assert!(!f.is_empty() && f.len() <= framing.max_frame);
                    framing::encode(&framing, &f, &mut reencoded).expect("fits");
                    frames += 1;
                }
                Ok(None) => break,
                Err(_) => return frames,
            }
        }
    }
    // What was read, re-encoded, is the stream with empty frames removed.
    let mut stripped = Vec::new();
    let mut at = 0;
    while at + 2 <= stream.len() {
        let len = usize::from(u16::from_be_bytes([stream[at], stream[at + 1]]));
        let Some(f) = stream.get(at + 2..at + 2 + len) else {
            break;
        };
        if !f.is_empty() {
            stripped.extend_from_slice(&stream[at..at + 2 + len]);
        }
        at += 2 + len;
    }
    assert_eq!(reencoded, stripped);
    frames
}

/// Script operations for [`whip_session`]; the op byte is taken modulo
/// the count.
pub mod op {
    /// Raw bytes from tuple A.
    pub const RAW_A: u8 = 0;
    /// Raw bytes from tuple B.
    pub const RAW_B: u8 = 1;
    /// Honest checks, as many as the first byte's top four bits plus one;
    /// bit 0 sets USE-CANDIDATE, bit 1 sends from tuple B, and bit 3
    /// corrupts each one's MESSAGE-INTEGRITY, so it is unauthenticated.
    pub const CHECK: u8 = 2;
    /// A check with ICE-CONTROLLED.
    pub const CONTROLLED: u8 = 3;
    /// The client's pending DTLS datagrams, the first byte choosing which
    /// to drop, duplicate or mutate (with the rest as the mutation).
    pub const DTLS: u8 = 4;
    /// Advance time by the first byte times 25 ms (two bytes: big-endian
    /// milliseconds), then run timers.
    pub const TIME: u8 = 5;
    /// Protected RTP: the bytes are the payload; byte 0 picks video, a
    /// keyframe, audio or RTX.
    pub const RTP: u8 = 6;
    /// Protected RTCP with the bytes as the compound, or sender reports.
    pub const RTCP: u8 = 7;
    /// Replays one of the datagrams sent so far, chosen by the first byte.
    pub const REPLAY: u8 = 8;
    /// Closes the session.
    pub const CLOSE: u8 = 9;
    /// An observer's copy from another tuple: bits 5 to 7 of the first byte
    /// pick one of four that never send a check of their own, or (4)
    /// tuple B. Bits 0 and 1 pick what: 0, a STUN datagram sent so far (chosen by
    /// the second byte); 1, a copy of a fresh SRTP packet ahead of its
    /// original; 2, the same after its original; 3, copies of fresh sender
    /// reports, ahead of the originals unless bit 2 is set.
    pub const ELSEWHERE: u8 = 10;
    /// How many.
    pub const COUNT: u8 = 11;
}

/// A scripted session run. Returns how far it got: 0 checking, 1
/// handshaking, 2 connected, 3 ended.
pub fn whip_session(data: &[u8]) -> u8 {
    whip_session_run(data).stage
}

/// What a scripted session run reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// 0 checking, 1 handshaking, 2 connected, 3 ended.
    pub stage: u8,
    /// Bytes the session sent the third tuple, which only copies come from.
    pub to_third: u64,
    /// Bytes the third tuple sent the session.
    pub from_third: u64,
    /// Validated tuples the session evicted.
    pub evicted: u64,
}

/// What the harness knows about the tuples, for the oracles: bytes both
/// ways, the tuples ever validated, and its own record of when each last
/// delivered fresh media and when it was validated.
struct Oracle {
    limit: u64,
    known: Vec<Tuple>,
    from: HashMap<Tuple, u64>,
    to: HashMap<Tuple, u64>,
    ever: Vec<Tuple>,
    fresh: HashMap<Tuple, Instant>,
    order: HashMap<Tuple, u64>,
    validations: u64,
    out: usize,
    /// The publisher's own checks, byte for byte: a datagram that is not
    /// one of these (or a copy of one) is unauthenticated.
    authentic: Vec<Vec<u8>>,
    /// Transaction IDs of requests the session answered, each with the
    /// tuple it first came from and how many authentic checks had been
    /// delivered then. The session records at most one ID per authentic
    /// check it is handed (it may record one whose answer the per-tuple
    /// budget then drops), so an ID is surely still remembered while fewer
    /// than `max_transactions` authentic checks have arrived since. A
    /// retransmission it answers from the tuple that first sent the ID is
    /// not recorded again and does not move the ID's place, so it starts no
    /// entry here.
    remembered: Vec<([u8; 12], Tuple, u64)>,
    /// Authentic checks delivered so far.
    authentic_delivered: u64,
    max_transactions: usize,
}

impl Oracle {
    /// Hands one datagram to the session and checks what follows.
    fn deliver(
        &mut self,
        s: &mut Session,
        now: Instant,
        t: Tuple,
        d: &[u8],
        replies: &mut Vec<Transmit>,
        home: Tuple,
    ) {
        let before: Vec<bool> = self.known.iter().map(|k| s.accepts(k)).collect();
        let (selected, sending) = (s.selected(), s.send_tuple());
        let media = |s: &Session| s.stats().srtp_packets + s.stats().srtcp_packets;
        let media_before = media(s);
        let repeats_before = s.stats().ice.repeats;
        *self.from.entry(t).or_default() += d.len() as u64;
        s.handle_datagram(now, t, d);
        let after: Vec<bool> = self.known.iter().map(|k| s.accepts(k)).collect();
        // Eviction: whoever went had the stalest media of those that could.
        let key = |o: &Self, k: &Tuple| o.fresh.get(k).copied();
        for (i, gone) in self.known.iter().enumerate() {
            if !before[i] || after[i] {
                continue;
            }
            for (j, kept) in self.known.iter().enumerate() {
                if i == j
                    || !(before[j] && after[j])
                    || Some(*kept) == selected
                    || Some(*kept) == sending
                {
                    continue;
                }
                let (kg, kk) = (key(self, gone), key(self, kept));
                let earlier = self.order.get(gone) < self.order.get(kept);
                assert!(
                    kk > kg || (kk == kg && earlier),
                    "evicted a tuple with fresher media than one kept"
                );
            }
        }
        for (i, k) in self.known.clone().iter().enumerate() {
            if before[i] && !after[i] {
                self.fresh.remove(k);
            }
            if !before[i] && after[i] {
                self.validations += 1;
                self.order.insert(*k, self.validations);
                self.fresh.remove(k);
                if !self.ever.contains(k) {
                    self.ever.push(*k);
                }
            }
        }
        if media(s) > media_before {
            self.fresh.insert(t, now);
        }
        let authentic = self.authentic.iter().any(|a| a == d);
        let id = authentic
            .then(|| Message::parse(d).ok().map(|m| m.transaction))
            .flatten();
        // A re-sent check whose original it remembers, from another tuple:
        // nothing at all goes back to that tuple (A3.7).
        let since = self.authentic_delivered;
        let window = self.max_transactions as u64;
        let copy = id.and_then(|id| {
            self.remembered
                .iter()
                .find(|(seen, _, _)| *seen == id)
                .filter(|(_, first, at)| *first != t && since - at < window)
                .map(|_| t)
        });
        if authentic {
            self.authentic_delivered += 1;
        }
        let answered = self.drain(s, Some((t, d.len(), authentic)), replies, home);
        if let Some(copier) = copy {
            assert!(
                !answered.contains(&copier),
                "a re-sent check drew bytes while its ID was remembered"
            );
        }
        // Answered as a retransmission: the session kept the ID where it
        // was, so its memory of it is not renewed.
        let repeat = s.stats().ice.repeats > repeats_before;
        if let Some(id) = id
            && answered.contains(&t)
            && !repeat
            && !self.remembered.iter().any(|(seen, _, _)| *seen == id)
        {
            self.remembered.push((id, t, since));
            // Only entries that can still be asserted on are kept.
            let now = self.authentic_delivered;
            self.remembered.retain(|(_, _, at)| now - at < window);
        }
    }

    /// Takes what the session queued and checks it; `request` is the
    /// datagram it answers, if any: its tuple, its length and whether it is
    /// authentic. Returns the tuples sent to.
    fn drain(
        &mut self,
        s: &mut Session,
        request: Option<(Tuple, usize, bool)>,
        replies: &mut Vec<Transmit>,
        home: Tuple,
    ) -> Vec<Tuple> {
        let target = s.send_tuple();
        let mut sent_to = Vec::new();
        while let Some(x) = s.poll_transmit() {
            self.out += 1;
            assert!(x.bytes.len() <= 1500, "a datagram within the MTU cap");
            // Every tuple, whatever it has proven: at most the limit times
            // what it sent, STUN responses included.
            let total = self.to.entry(x.tuple).or_default();
            *total += x.bytes.len() as u64;
            let got = self.from.get(&x.tuple).copied().unwrap_or(0);
            assert!(
                *total <= self.limit * got,
                "{total} bytes to a tuple that sent {got}"
            );
            // A tuple never validated: only a STUN error response to the
            // authenticated request it just sent, no larger than it; to an
            // unauthenticated one, nothing (A3.7).
            if !self.ever.contains(&x.tuple) {
                let error =
                    Message::parse(&x.bytes).is_ok_and(|m| m.msg_type == stun::BINDING_ERROR);
                let answers = request.is_some_and(|(t, n, authentic)| {
                    t == x.tuple && x.bytes.len() <= n && authentic
                });
                assert!(error && answers, "sent to a tuple that never authenticated");
            }
            if !sent_to.contains(&x.tuple) {
                sent_to.push(x.tuple);
            }
            if classify(&x.bytes) != Kind::Stun {
                assert_eq!(
                    Some(x.tuple),
                    target,
                    "DTLS and RTCP only to the send tuple"
                );
            }
            if x.tuple == home {
                replies.push(x);
            }
        }
        sent_to
    }
}

/// [`whip_session`], reporting what reached the third tuple.
pub fn whip_session_run(data: &[u8]) -> Run {
    let t0 = Instant::now();
    let mut p = Publisher::new(3, &[0x0007, 0x0001]);
    // The smallest ID cache, so that checks are forgotten within a script,
    // and room for 3 tuples of the 6 used, so that tuples are evicted.
    let mut config = SessionConfig::default();
    config.ice.max_transactions = 16;
    config.ice.max_tuples = 3;
    let limit = u64::from(config.amplification_limit);
    let mut a = testing::accept(&p.offer(), config, t0, 3);
    p.answered(&a);
    let s = &mut a.session;
    let other = tuple(9);
    let stranger = tuple(77);
    let copiers = [stranger, tuple(78), tuple(79), tuple(80), other];
    let mut now = t0;
    let mut o = Oracle {
        limit,
        known: [p.tuple, other, stranger, tuple(78), tuple(79), tuple(80)].to_vec(),
        from: HashMap::new(),
        to: HashMap::new(),
        ever: Vec::new(),
        fresh: HashMap::new(),
        order: HashMap::new(),
        validations: 0,
        out: 0,
        authentic: Vec::new(),
        remembered: Vec::new(),
        authentic_delivered: 0,
        max_transactions: 16,
    };
    let mut pending: Vec<Vec<u8>> = Vec::new();
    let mut history: Vec<(Tuple, Vec<u8>)> = Vec::new();
    let mut started = false;
    let mut furthest = 0u8;
    for (op, bytes) in steps(data) {
        let first = bytes.first().copied().unwrap_or(0);
        let mut sent: Vec<(Tuple, Vec<u8>)> = Vec::new();
        match op % op::COUNT {
            op::RAW_A => sent.push((p.tuple, bytes.to_vec())),
            op::RAW_B => sent.push((other, bytes.to_vec())),
            op::CHECK => {
                let from = if first & 2 == 0 { p.tuple } else { other };
                for _ in 0..=first >> 4 {
                    let mut c = p.check(first & 1 == 1);
                    if first & 8 == 0 {
                        o.authentic.push(c.clone());
                    } else {
                        unauthenticate(&mut c);
                    }
                    sent.push((from, c));
                }
            }
            op::CONTROLLED => {
                let c = p.stun.as_mut().unwrap().request(first & 1 == 1, true);
                o.authentic.push(c.clone());
                sent.push((p.tuple, c));
            }
            op::DTLS => {
                if !started {
                    pending = p.client.start();
                    started = true;
                }
                for (i, mut d) in std::mem::take(&mut pending).into_iter().enumerate() {
                    let pick = usize::from(first >> 4);
                    match first & 3 {
                        1 if i == pick => continue,
                        2 if i == pick => sent.push((p.tuple, d.clone())),
                        3 if i == pick => {
                            let n = d.len().max(1);
                            for (j, m) in bytes.iter().skip(1).enumerate() {
                                if let Some(x) = d.get_mut(j % n) {
                                    *x ^= m;
                                }
                            }
                        }
                        _ => {}
                    }
                    sent.push((p.tuple, d));
                }
            }
            op::TIME => {
                let step = match bytes {
                    [hi, lo, ..] => u64::from(u16::from_be_bytes([*hi, *lo])),
                    _ => u64::from(first) * 25,
                };
                now += Duration::from_millis(step);
                testing::run_until(s, now);
            }
            op::RTP if p.has_keys() => {
                let pkts = match first % 4 {
                    0 => p.video(&[bytes], testing::VIDEO_SSRC),
                    1 => p.keyframe(usize::from(first) * 8 + 2).0,
                    2 => vec![p.audio(first)],
                    _ => vec![p.rtx(
                        p.video_seq.wrapping_sub(u16::from(first)),
                        bytes,
                        p.video_ts,
                        first & 8 != 0,
                    )],
                };
                sent.extend(pkts.into_iter().map(|d| (p.tuple, d)));
            }
            op::RTCP if p.has_keys() => {
                if first & 1 == 0 {
                    let ntp = u64::from_be_bytes([first, 1, 2, 3, 4, 5, 6, 7]);
                    sent.extend(p.sender_reports(ntp).into_iter().map(|d| (p.tuple, d)));
                } else if let Some(d) = p.try_protect_rtcp(bytes) {
                    sent.push((p.tuple, d));
                }
            }
            op::REPLAY => {
                if !history.is_empty() {
                    let (t, d) = history[usize::from(first) % history.len()].clone();
                    sent.push((t, d));
                }
            }
            op::CLOSE => s.close(now),
            op::ELSEWHERE => {
                let copier = copiers[usize::from(first >> 5) % copiers.len()];
                let pick = usize::from(bytes.get(1).copied().unwrap_or(0));
                match first & 3 {
                    0 => {
                        let checks: Vec<&Vec<u8>> = history
                            .iter()
                            .map(|(_, d)| d)
                            .filter(|d| classify(d) == Kind::Stun)
                            .collect();
                        if !checks.is_empty() {
                            let d = checks[pick % checks.len()].clone();
                            sent.push((copier, d));
                        }
                    }
                    m if p.has_keys() => {
                        let fresh = if m == 3 {
                            p.sender_reports(u64::from_be_bytes([first, 9, 8, 7, 6, 5, 4, 3]))
                        } else {
                            vec![p.audio(first)]
                        };
                        let ahead = m == 1 || (m == 3 && first & 4 == 0);
                        for d in fresh {
                            if ahead {
                                sent.push((copier, d.clone()));
                                sent.push((p.tuple, d));
                            } else {
                                sent.push((p.tuple, d.clone()));
                                sent.push((copier, d));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        // Invariants on what the session sends: timer output first, then
        // each datagram's.
        o.out = 0;
        let mut replies = Vec::new();
        let _ = o.drain(s, None, &mut replies, p.tuple);
        for (t, d) in sent {
            if history.len() < 256 {
                history.push((t, d.clone()));
            }
            o.deliver(s, now, t, &d, &mut replies, p.tuple);
        }
        assert!(
            o.out <= MAX_DATAGRAMS_PER_STEP,
            "{} datagrams from one step",
            o.out
        );
        // The publisher reads what was sent to it.
        for t in replies {
            match classify(&t.bytes) {
                Kind::Dtls => {
                    if let Ok(r) = p.client.handle(&t.bytes) {
                        pending.extend(r);
                    }
                }
                Kind::Stun => {
                    let m = Message::parse(&t.bytes).expect("our STUN parses");
                    assert!(m.check_integrity(&t.bytes, &p.stun.as_ref().unwrap().key));
                }
                _ => {}
            }
        }
        assert!(
            s.selected().is_none_or(|t| s.accepts(&t)),
            "selected unvalidated"
        );
        assert!(
            s.send_tuple().is_none_or(|t| s.accepts(&t)),
            "sending unvalidated"
        );
        p.receive_keys();
        while let Some(e) = s.poll_event() {
            if let Event::Connected(_) = e {
                assert!(s.profile().is_some());
            }
        }
        let stage = match s.state() {
            State::Checking => 0,
            State::Handshaking => 1,
            State::Connected => 2,
            State::Ended(_) => 3,
        };
        furthest = furthest.max(stage);
        if let State::Ended(_) = s.state() {
            assert!(s.poll_timeout().is_none());
        }
    }
    Run {
        stage: furthest,
        to_third: o.to.get(&stranger).copied().unwrap_or(0),
        from_third: o.from.get(&stranger).copied().unwrap_or(0),
        evicted: s.stats().ice.evicted,
    }
}

/// Signs a check again with the wrong key and a good FINGERPRINT, so it
/// reaches the MESSAGE-INTEGRITY check and fails it.
fn unauthenticate(check: &mut Vec<u8>) {
    *check = rebuild_signed(check, b"not the session's key at all");
}

/// [`whip_router`]'s script steps: the first byte's bit 0 picks a
/// publisher where one is meant.
pub mod router_op {
    /// Raw bytes from a stranger (bits 0 and 1 of the first byte).
    pub const RAW: u8 = 0;
    /// An honest check, with USE-CANDIDATE if bit 1 is set, from the
    /// publisher's tuple, or the other publisher's if bit 2 is set. With
    /// bit 3, its MESSAGE-INTEGRITY corrupted, from the stranger bits 4 and
    /// 5 pick.
    pub const CHECK: u8 = 1;
    /// The publisher's pending DTLS datagrams.
    pub const DTLS: u8 = 2;
    /// Advance time by the first byte times 25 ms, then run due timers.
    pub const TIME: u8 = 3;
    /// The publisher's media once keyed: bits 1 and 2 pick audio, a delta
    /// frame or a keyframe.
    pub const MEDIA: u8 = 4;
    /// A datagram sent so far, chosen by the second byte, sent again from
    /// a stranger (bits 0 and 1), or from the other publisher's tuple if
    /// bit 2 is set.
    pub const COPY: u8 = 5;
    /// Closes the publisher's session.
    pub const CLOSE: u8 = 6;
    /// How many.
    pub const COUNT: u8 = 7;
}

/// A scripted run of two publishers' sessions in one router. Returns how
/// many connected.
pub fn whip_router(data: &[u8]) -> u8 {
    whip_router_run(data).connected
}

/// What a scripted router run reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterRun {
    /// Sessions that connected.
    pub connected: u8,
    /// Bytes sent to the strangers, which only send raw bytes and copies.
    pub to_strangers: u64,
}

/// [`whip_router`], reporting what it reached.
pub fn whip_router_run(data: &[u8]) -> RouterRun {
    let t0 = Instant::now();
    let config = SessionConfig::default();
    let limit = u64::from(config.amplification_limit);
    let mut r = Router::new(RouterConfig::default()).expect("defaults");
    let mut ps = [Publisher::new(5, &[0x0007]), Publisher::new(6, &[0x0007])];
    let mut ids = Vec::new();
    for (k, p) in ps.iter_mut().enumerate() {
        let seed = 5 + k as u8;
        let a = testing::accept(&p.offer(), config.clone(), t0, seed);
        p.answered(&a);
        let id = SessionId::generate_seeded([seed; 32]).expect("id");
        r.insert(id, a.session).expect("room");
        ids.push(id);
    }
    let strangers = [tuple(70), tuple(71), tuple(72), tuple(73)];
    let mut from: HashMap<Tuple, u64> = HashMap::new();
    let mut to: HashMap<Tuple, u64> = HashMap::new();
    // Tuples that have sent a copy of something, which may then be
    // answered (within the bound); the others have sent only their own.
    let mut copiers: Vec<Tuple> = Vec::new();
    let mut pending: [Vec<Vec<u8>>; 2] = [Vec::new(), Vec::new()];
    let mut started = [false; 2];
    let mut closed = [false; 2];
    let mut connected = [false; 2];
    let mut history: Vec<Vec<u8>> = Vec::new();
    let mut now = t0;
    // What went out: checked, and handed to the publisher it is for.
    let mut drain = |r: &mut Router,
                     ps: &mut [Publisher; 2],
                     pending: &mut [Vec<Vec<u8>>; 2],
                     from: &HashMap<Tuple, u64>,
                     copiers: &[Tuple]| {
        while let Some(x) = r.poll_transmit() {
            assert!(x.bytes.len() <= 1500, "a datagram within the MTU cap");
            let total = to.entry(x.tuple).or_default();
            *total += x.bytes.len() as u64;
            let got = from.get(&x.tuple).copied().unwrap_or(0);
            assert!(
                *total <= limit * got,
                "{total} bytes to a tuple that sent {got}"
            );
            assert!(
                !strangers.contains(&x.tuple) || copiers.contains(&x.tuple),
                "sent to a stranger that has only sent its own bytes"
            );
            for (k, p) in ps.iter_mut().enumerate() {
                if p.tuple != x.tuple {
                    continue;
                }
                if classify(&x.bytes) == Kind::Rtcp && !p.has_keys() {
                    continue;
                }
                pending[k].extend(p.take(x.clone()));
            }
        }
        to.clone()
    };
    for (op, bytes) in steps(data) {
        let first = bytes.first().copied().unwrap_or(0);
        let k = usize::from(first & 1);
        let mut sent: Vec<(Tuple, Vec<u8>)> = Vec::new();
        match op % router_op::COUNT {
            router_op::RAW => sent.push((strangers[usize::from(first & 3)], bytes.to_vec())),
            router_op::CHECK if first & 8 != 0 => {
                let mut c = ps[k].check(first & 2 != 0);
                unauthenticate(&mut c);
                sent.push((strangers[usize::from(first >> 4) & 3], c));
            }
            router_op::CHECK => {
                let from = if first & 4 == 0 {
                    ps[k].tuple
                } else {
                    ps[1 - k].tuple
                };
                sent.push((from, ps[k].check(first & 2 != 0)));
            }
            router_op::DTLS => {
                if !started[k] {
                    pending[k] = ps[k].client.start();
                    started[k] = true;
                }
                let t = ps[k].tuple;
                sent.extend(std::mem::take(&mut pending[k]).into_iter().map(|d| (t, d)));
            }
            router_op::TIME => {
                now += Duration::from_millis(u64::from(first) * 25);
                r.handle_timeout(now);
            }
            router_op::MEDIA if ps[k].has_keys() => {
                let t = ps[k].tuple;
                let packets = match (first >> 1) & 3 {
                    1 => ps[k].delta(200).0,
                    2 => ps[k].keyframe(600).0,
                    _ => vec![ps[k].audio(first)],
                };
                sent.extend(packets.into_iter().map(|d| (t, d)));
            }
            router_op::COPY if !history.is_empty() => {
                let pick = usize::from(bytes.get(1).copied().unwrap_or(0));
                let d = history[pick % history.len()].clone();
                let t = if first & 4 == 0 {
                    strangers[usize::from(first & 3)]
                } else {
                    ps[1 - k].tuple
                };
                if !copiers.contains(&t) {
                    copiers.push(t);
                }
                sent.push((t, d));
            }
            router_op::CLOSE => {
                r.close(&ids[k], now);
                closed[k] = true;
            }
            _ => {}
        }
        let _ = drain(&mut r, &mut ps, &mut pending, &from, &copiers);
        for (t, d) in sent {
            if history.len() < 256 {
                history.push(d.clone());
            }
            *from.entry(t).or_default() += d.len() as u64;
            r.handle_datagram(now, t, &d);
            let _ = drain(&mut r, &mut ps, &mut pending, &from, &copiers);
        }
        for p in &mut ps {
            p.receive_keys();
        }
        while let Some((id, e)) = r.poll_event() {
            if let Event::Connected(_) = e
                && let Some(k) = ids.iter().position(|i| *i == id)
            {
                connected[k] = true;
            }
        }
        assert!(r.deadlines_indexed(), "the deadline index is exact");
        for t in strangers.iter().chain(ps.iter().map(|p| &p.tuple)) {
            if let Some(owner) = r.owner(t) {
                assert!(
                    r.session(&owner).is_some(),
                    "an owned tuple's owner is live"
                );
            }
        }
        for (k, id) in ids.iter().enumerate() {
            if closed[k] {
                assert!(r.session(id).is_none(), "a closed session is gone");
            }
        }
    }
    let to = drain(&mut r, &mut ps, &mut pending, &from, &copiers);
    RouterRun {
        connected: connected.iter().filter(|c| **c).count() as u8,
        to_strangers: strangers
            .iter()
            .map(|t| to.get(t).copied().unwrap_or(0))
            .sum(),
    }
}

#[cfg(test)]
mod tests;
