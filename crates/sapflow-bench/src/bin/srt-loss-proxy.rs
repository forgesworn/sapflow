//! UDP proxy that drops and delays packets, for SRT loss tests
//! (`scripts/srt-loss.sh`).
//!
//!     srt-loss-proxy --listen 127.0.0.1:8891 --target 127.0.0.1:8890 \
//!         --loss random:10 --delay-ms 30 --seed 1 --stats stats.txt
//!
//! Forwards datagrams between one client and one target. Each direction
//! drops packets with its own loss process and delays every packet by a fixed
//! time, so the round trip is twice `--delay-ms`. Packets leave in the order
//! they arrived; `reordered_orig` in the stats counts original data packets
//! that did not, and should be 0. SRT handshakes are never
//! dropped, so a connection always forms. On SIGINT or SIGTERM it writes what
//! it saw as `key=value` lines: packets sent and dropped each way, and, read
//! from the SRT headers, original and retransmitted data packets, NAK and ACK
//! packets and the handshake flags the target advertised. With `--latency-ms`
//! set to the SRT latency it also counts the original data packets that reached
//! the target in time, by any copy, within that latency of their first send;
//! the rest are residual loss whatever the receiver does with them.
//!
//! Loss models (`--loss`):
//!
//! - `none`
//! - `random:P`: each packet is lost with probability P percent.
//! - `burst:P,L`: Gilbert-Elliott. A good and a bad state; every packet in the
//!   bad state is lost. P percent of packets are lost on average and a burst
//!   lasts L packets on average.
//!
//! The same seed gives the same loss pattern for the same packet sequence.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::signal::unix::{SignalKind, signal};

/// SplitMix64: small, fast and good enough for loss patterns.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Packets waiting out their delay, oldest first. The delay is the same for
/// every packet, so arrival order is also release order.
struct DelayQueue<T> {
    delay: Duration,
    queue: VecDeque<(Instant, T)>,
}

impl<T> DelayQueue<T> {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            queue: VecDeque::new(),
        }
    }

    fn push(&mut self, now: Instant, item: T) {
        self.queue.push_back((now + self.delay, item));
    }

    fn next_due(&self) -> Option<Instant> {
        self.queue.front().map(|(at, _)| *at)
    }

    fn pop_due(&mut self, now: Instant) -> Option<T> {
        if self.queue.front().is_some_and(|(at, _)| *at <= now) {
            return self.queue.pop_front().map(|(_, item)| item);
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LossSpec {
    None,
    Random(f64),
    /// Average loss and mean burst length in packets.
    Burst(f64, f64),
}

impl LossSpec {
    fn parse(s: &str) -> Result<Self, String> {
        let bad = || format!("--loss: expected none, random:P or burst:P,L, got {s}");
        if s == "none" {
            return Ok(Self::None);
        }
        let (kind, rest) = s.split_once(':').ok_or_else(bad)?;
        let nums: Vec<f64> = rest
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| bad())?;
        match (kind, nums.as_slice()) {
            ("random", [p]) if (0.0..100.0).contains(p) => Ok(Self::Random(p / 100.0)),
            ("burst", [p, l]) if (0.0..100.0).contains(p) && *l >= 1.0 => {
                Ok(Self::Burst(p / 100.0, *l))
            }
            _ => Err(bad()),
        }
    }
}

/// One direction's loss process.
struct Lossy {
    spec: LossSpec,
    rng: Rng,
    bad: bool,
}

impl Lossy {
    fn new(spec: LossSpec, seed: u64) -> Self {
        Self {
            spec,
            rng: Rng(seed),
            bad: false,
        }
    }

    fn drops(&mut self) -> bool {
        match self.spec {
            LossSpec::None => false,
            LossSpec::Random(p) => self.rng.next_f64() < p,
            LossSpec::Burst(avg, len) => {
                // Mean burst len gives the exit probability; the entry
                // probability follows from the stationary loss rate.
                let p_bg = 1.0 / len;
                let p_gb = avg * p_bg / (1.0 - avg);
                let flip = self.rng.next_f64() < if self.bad { p_bg } else { p_gb };
                if flip {
                    self.bad = !self.bad;
                }
                self.bad
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Kind {
    Handshake,
    Control(u16),
    Data { seq: u32, retransmit: bool },
    Other,
}

fn word(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// Read what kind of SRT packet this is from its header.
fn classify(d: &[u8]) -> Kind {
    let (Some(w0), Some(w1)) = (word(d, 0), word(d, 4)) else {
        return Kind::Other;
    };
    if d.len() < 16 {
        return Kind::Other;
    }
    if w0 & 0x8000_0000 != 0 {
        match ((w0 >> 16) & 0x7FFF) as u16 {
            0 => Kind::Handshake,
            t => Kind::Control(t),
        }
    } else {
        Kind::Data {
            seq: w0 & 0x7FFF_FFFF,
            // The R bit of the message number word.
            retransmit: w1 & 0x0400_0000 != 0,
        }
    }
}

/// Sequence numbers a NAK asks for: a range starts with the high bit set.
fn nak_count(d: &[u8]) -> u64 {
    let mut n = 0u64;
    let mut at = 16;
    while let Some(w) = word(d, at) {
        if w & 0x8000_0000 != 0 {
            let end = word(d, at + 4).unwrap_or(w) & 0x7FFF_FFFF;
            n += u64::from((end.wrapping_sub(w & 0x7FFF_FFFF)) & 0x7FFF_FFFF) + 1;
            at += 8;
        } else {
            n += 1;
            at += 4;
        }
    }
    n
}

/// The flags in a handshake reply's SRT extension (type 2), if there is one.
fn handshake_rsp_flags(d: &[u8]) -> Option<u32> {
    // 16 byte header, 48 byte handshake, then extensions: type, length in
    // words, body. HSRSP is type 2; its body is version then flags.
    let mut at = 64;
    while let (Some(w), true) = (word(d, at), at < d.len()) {
        let (kind, words) = ((w >> 16) as u16, (w & 0xFFFF) as usize);
        if kind == 2 {
            return word(d, at + 8);
        }
        at += 4 + words * 4;
    }
    None
}

#[derive(Default)]
struct Stats {
    sent: [u64; 2],
    dropped: [u64; 2],
    data_orig: u64,
    data_retx: u64,
    retx_seqs: HashMap<u32, u32>,
    nak_packets: u64,
    nak_seqs: u64,
    ack_packets: u64,
    hs_flags: Option<u32>,
    reordered_orig: u64,
    last_orig_sent: Option<u32>,
    /// Per original data packet: when it first reached the proxy and when a
    /// copy of it was first let through.
    seen: HashMap<u32, Seen>,
    latency: Duration,
}

struct Seen {
    first: Instant,
    through: Option<Instant>,
}

impl Stats {
    fn account(&mut self, dir: usize, d: &[u8]) {
        self.sent[dir] += 1;
        match classify(d) {
            Kind::Data { seq, retransmit } => {
                if retransmit {
                    self.data_retx += 1;
                    *self.retx_seqs.entry(seq).or_insert(0) += 1;
                } else {
                    self.data_orig += 1;
                }
            }
            Kind::Control(3) => {
                self.nak_packets += 1;
                self.nak_seqs += nak_count(d);
            }
            Kind::Control(2) => self.ack_packets += 1,
            Kind::Handshake if dir == 1 => {
                if let Some(f) = handshake_rsp_flags(d) {
                    self.hs_flags = Some(f);
                }
            }
            _ => {}
        }
    }

    /// Records a client-to-target data packet as it arrives and whether the
    /// loss process dropped it.
    fn note_data(&mut self, now: Instant, d: &[u8], dropped: bool) {
        let Kind::Data { seq, retransmit } = classify(d) else {
            return;
        };
        if !retransmit {
            self.seen.entry(seq).or_insert(Seen {
                first: now,
                through: None,
            });
        }
        if !dropped && let Some(s) = self.seen.get_mut(&seq) {
            s.through.get_or_insert(now);
        }
    }

    /// Original data packets that got through within the latency of their
    /// first send.
    fn recovered_in_time(&self) -> usize {
        self.seen
            .values()
            .filter(|s| {
                s.through
                    .is_some_and(|t| t.saturating_duration_since(s.first) <= self.latency)
            })
            .count()
    }

    /// Called as a packet leaves towards the target: an original data packet
    /// older than one already sent means the proxy reordered the stream.
    fn note_sent_to_target(&mut self, d: &[u8]) {
        let Kind::Data {
            seq,
            retransmit: false,
        } = classify(d)
        else {
            return;
        };
        if let Some(last) = self.last_orig_sent {
            // Sequence numbers are 31 bits and wrap.
            if seq.wrapping_sub(last) & 0x7FFF_FFFF > 0x3FFF_FFFF {
                self.reordered_orig += 1;
                return;
            }
        }
        self.last_orig_sent = Some(seq);
    }

    fn render(&self) -> String {
        let mut s = String::new();
        let mut kv = |k: &str, v: String| {
            s.push_str(k);
            s.push('=');
            s.push_str(&v);
            s.push('\n');
        };
        kv("sent_c2s", self.sent[0].to_string());
        kv("dropped_c2s", self.dropped[0].to_string());
        kv("sent_s2c", self.sent[1].to_string());
        kv("dropped_s2c", self.dropped[1].to_string());
        kv("data_orig", self.data_orig.to_string());
        kv("data_retx", self.data_retx.to_string());
        kv("distinct_retx", self.retx_seqs.len().to_string());
        kv("nak_packets", self.nak_packets.to_string());
        kv("nak_seqs", self.nak_seqs.to_string());
        kv("ack_packets", self.ack_packets.to_string());
        kv("reordered_orig", self.reordered_orig.to_string());
        kv("orig_distinct", self.seen.len().to_string());
        kv("in_time", self.recovered_in_time().to_string());
        kv(
            "hs_flags",
            self.hs_flags
                .map_or_else(|| "none".into(), |f| format!("{f:#x}")),
        );
        s
    }
}

struct Args {
    listen: SocketAddr,
    target: SocketAddr,
    loss: LossSpec,
    delay: Duration,
    latency: Duration,
    seed: u64,
    stats: Option<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        listen: "127.0.0.1:0".parse().map_err(|_| "listen")?,
        target: "127.0.0.1:0".parse().map_err(|_| "target")?,
        loss: LossSpec::None,
        delay: Duration::ZERO,
        latency: Duration::ZERO,
        seed: 1,
        stats: None,
    };
    let mut have_target = false;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().ok_or(format!("{k} needs a value"))?;
        match k.as_str() {
            "--listen" => a.listen = v.parse().map_err(|_| "--listen: host:port")?,
            "--target" => {
                a.target = v.parse().map_err(|_| "--target: host:port")?;
                have_target = true;
            }
            "--loss" => a.loss = LossSpec::parse(&v)?,
            "--delay-ms" => {
                a.delay = Duration::from_millis(v.parse().map_err(|_| "--delay-ms: a number")?);
            }
            "--latency-ms" => {
                a.latency = Duration::from_millis(v.parse().map_err(|_| "--latency-ms: a number")?);
            }
            "--seed" => a.seed = v.parse().map_err(|_| "--seed: a number")?,
            "--stats" => a.stats = Some(v.into()),
            _ => return Err(format!("unknown option {k}")),
        }
    }
    if !have_target {
        return Err("--target host:port is required".into());
    }
    Ok(a)
}

async fn run(a: Args) -> std::io::Result<Stats> {
    let front = UdpSocket::bind(a.listen).await?;
    let up = UdpSocket::bind("127.0.0.1:0").await?;
    up.connect(a.target).await?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut term = signal(SignalKind::terminate())?;

    // Each direction gets its own loss process so one does not shift the other.
    let mut loss = [
        Lossy::new(a.loss, a.seed),
        Lossy::new(a.loss, a.seed ^ 0x5DEE_CE66_D1CE_4E5B),
    ];
    let mut stats = Stats {
        latency: a.latency,
        ..Stats::default()
    };
    let mut client: Option<SocketAddr> = None;
    // Direction 0 is client to target, 1 is target to client.
    let mut queue: DelayQueue<(usize, Vec<u8>)> = DelayQueue::new(a.delay);
    let (mut fbuf, mut ubuf) = (vec![0u8; 65536], vec![0u8; 65536]);
    loop {
        let due = queue.next_due().map(tokio::time::Instant::from_std);
        tokio::select! {
            r = front.recv_from(&mut fbuf) => {
                let (n, from) = r?;
                client = Some(from);
                let d = fbuf[..n].to_vec();
                stats.account(0, &d);
                let now = Instant::now();
                let dropped = classify(&d) != Kind::Handshake && loss[0].drops();
                stats.note_data(now, &d, dropped);
                if dropped {
                    stats.dropped[0] += 1;
                } else {
                    queue.push(now, (0, d));
                }
            }
            r = up.recv(&mut ubuf) => {
                let n = r?;
                let d = ubuf[..n].to_vec();
                stats.account(1, &d);
                if client.is_none() {
                    continue;
                }
                if classify(&d) != Kind::Handshake && loss[1].drops() {
                    stats.dropped[1] += 1;
                } else {
                    queue.push(Instant::now(), (1, d));
                }
            }
            () = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                let now = Instant::now();
                while let Some((dir, d)) = queue.pop_due(now) {
                    if dir == 0 {
                        stats.note_sent_to_target(&d);
                        up.send(&d).await?;
                    } else if let Some(to) = client {
                        front.send_to(&d, to).await?;
                    }
                }
            }
            _ = int.recv() => return Ok(stats),
            _ = term.recv() => return Ok(stats),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "{e}\nusage: srt-loss-proxy --target host:port [--listen host:port] [--loss none|random:P|burst:P,L] [--delay-ms N] [--latency-ms N] [--seed N] [--stats file]"
            );
            return ExitCode::from(2);
        }
    };
    let out = a.stats.clone();
    match run(a).await {
        Ok(stats) => {
            let text = stats.render();
            match out {
                Some(p) => {
                    if let Err(e) = std::fs::write(&p, text) {
                        eprintln!("cannot write {}: {e}", p.display());
                        return ExitCode::FAILURE;
                    }
                }
                None => print!("{text}"),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("proxy failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(spec: LossSpec, n: usize) -> (f64, f64) {
        let mut l = Lossy::new(spec, 42);
        let (mut lost, mut bursts, mut prev) = (0usize, 0usize, false);
        for _ in 0..n {
            let d = l.drops();
            if d {
                lost += 1;
                if !prev {
                    bursts += 1;
                }
            }
            prev = d;
        }
        let mean_burst = if bursts == 0 {
            0.0
        } else {
            lost as f64 / bursts as f64
        };
        (lost as f64 / n as f64, mean_burst)
    }

    #[test]
    fn parses_loss_specs() {
        assert_eq!(LossSpec::parse("none"), Ok(LossSpec::None));
        assert_eq!(LossSpec::parse("random:10"), Ok(LossSpec::Random(0.1)));
        assert_eq!(LossSpec::parse("burst:8,6"), Ok(LossSpec::Burst(0.08, 6.0)));
        assert!(LossSpec::parse("random").is_err());
        assert!(LossSpec::parse("random:150").is_err());
        assert!(LossSpec::parse("burst:8").is_err());
        assert!(LossSpec::parse("burst:8,0.5").is_err());
        assert!(LossSpec::parse("gilbert:1").is_err());
    }

    #[test]
    fn random_loss_matches_its_rate() {
        let (r, _) = rate(LossSpec::Random(0.1), 200_000);
        assert!((r - 0.1).abs() < 0.005, "rate {r}");
    }

    #[test]
    fn burst_loss_matches_rate_and_length() {
        let (r, b) = rate(LossSpec::Burst(0.08, 6.0), 400_000);
        assert!((r - 0.08).abs() < 0.008, "rate {r}");
        assert!((b - 6.0).abs() < 0.6, "mean burst {b}");
    }

    #[test]
    fn same_seed_same_pattern() {
        let pattern = |seed| {
            let mut l = Lossy::new(LossSpec::Random(0.2), seed);
            (0..1000).map(|_| l.drops()).collect::<Vec<_>>()
        };
        assert_eq!(pattern(7), pattern(7));
        assert_ne!(pattern(7), pattern(8));
    }

    fn header(w0: u32, w1: u32) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(w0.to_be_bytes());
        d.extend(w1.to_be_bytes());
        d.extend([0u8; 8]);
        d
    }

    #[test]
    fn classifies_packets() {
        assert_eq!(classify(&[1, 2, 3]), Kind::Other);
        assert_eq!(classify(&header(0x8000_0000, 0)), Kind::Handshake);
        assert_eq!(classify(&header(0x8003_0000, 0)), Kind::Control(3));
        assert_eq!(
            classify(&header(77, 0)),
            Kind::Data {
                seq: 77,
                retransmit: false
            }
        );
        assert_eq!(
            classify(&header(77, 0x0400_0000)),
            Kind::Data {
                seq: 77,
                retransmit: true
            }
        );
    }

    #[test]
    fn counts_nak_sequence_numbers() {
        let mut d = header(0x8003_0000, 0);
        // One single, then a range 10..=14.
        d.extend(5u32.to_be_bytes());
        d.extend((0x8000_0000u32 | 10).to_be_bytes());
        d.extend(14u32.to_be_bytes());
        assert_eq!(nak_count(&d), 6);
    }

    #[test]
    fn reads_handshake_reply_flags() {
        let mut d = header(0x8000_0000, 5);
        d.extend([0u8; 48]);
        // An HSRSP extension: type 2, 3 words, version, flags, delays.
        d.extend(0x0002_0003u32.to_be_bytes());
        d.extend(0x0001_0504u32.to_be_bytes());
        d.extend(0x3Fu32.to_be_bytes());
        d.extend(0u32.to_be_bytes());
        assert_eq!(handshake_rsp_flags(&d), Some(0x3F));
        assert_eq!(handshake_rsp_flags(&header(0x8000_0000, 5)), None);
    }

    #[test]
    fn delay_queue_keeps_order_and_holds_until_due() {
        let t0 = Instant::now();
        let mut q = DelayQueue::new(Duration::from_millis(30));
        for i in 0..5u32 {
            q.push(t0 + Duration::from_micros(u64::from(i)), i);
        }
        assert_eq!(q.next_due(), Some(t0 + Duration::from_millis(30)));
        assert_eq!(q.pop_due(t0 + Duration::from_millis(29)), None);
        let now = t0 + Duration::from_millis(31);
        let out: Vec<u32> = std::iter::from_fn(|| q.pop_due(now)).collect();
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
        assert_eq!(q.next_due(), None);
    }

    #[test]
    fn counts_reordered_original_packets_including_across_a_wrap() {
        let mut s = Stats::default();
        for seq in [1u32, 2, 4, 3, 5] {
            s.note_sent_to_target(&header(seq, 0));
        }
        assert_eq!(s.reordered_orig, 1);
        // A retransmission is allowed to be old.
        s.note_sent_to_target(&header(2, 0x0400_0000));
        assert_eq!(s.reordered_orig, 1);
        // In order across the 31-bit wrap.
        let mut w = Stats::default();
        for seq in [0x7FFF_FFFEu32, 0x7FFF_FFFF, 0, 1] {
            w.note_sent_to_target(&header(seq, 0));
        }
        assert_eq!(w.reordered_orig, 0);
    }

    #[test]
    fn counts_packets_recovered_within_the_latency() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut s = Stats {
            latency: ms(200),
            ..Stats::default()
        };
        // 1 arrives. 2 is dropped, then a copy gets through 150 ms later.
        // 3 is dropped and its copy gets through 250 ms later. 4 is dropped
        // and never recovered.
        s.note_data(t0, &header(1, 0), false);
        s.note_data(t0, &header(2, 0), true);
        s.note_data(t0, &header(3, 0), true);
        s.note_data(t0, &header(4, 0), true);
        s.note_data(t0 + ms(150), &header(2, 0x0400_0000), false);
        s.note_data(t0 + ms(250), &header(3, 0x0400_0000), false);
        assert_eq!(s.seen.len(), 4);
        assert_eq!(s.recovered_in_time(), 2);
        // A retransmission that is itself dropped does not count.
        s.note_data(t0 + ms(100), &header(4, 0x0400_0000), true);
        assert_eq!(s.recovered_in_time(), 2);
    }

    #[test]
    fn stats_count_original_and_retransmitted_data() {
        let mut s = Stats::default();
        s.account(0, &header(1, 0));
        s.account(0, &header(2, 0));
        s.account(0, &header(2, 0x0400_0000));
        s.account(0, &header(2, 0x0400_0000));
        assert_eq!((s.data_orig, s.data_retx, s.retx_seqs.len()), (2, 2, 1));
        assert!(s.render().contains("data_retx=2\n"));
    }
}
