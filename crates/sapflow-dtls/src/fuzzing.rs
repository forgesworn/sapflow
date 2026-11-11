//! Entry points shared by the fuzz targets (`fuzz/fuzz_targets/dtls_*.rs`)
//! and the seed regression test below, so the committed seeds run through
//! exactly the code the fuzzer drives even where cargo-fuzz is not
//! installed.
//!
//! - [`dtls_record`]: one datagram through record framing, handshake
//!   fragment framing, epoch 1 opening with a fixed key, a seal and open
//!   round trip, and the replay window.
//! - [`dtls_handshake_messages`]: a selector byte, then one message body
//!   for the ClientHello, Certificate (and the X.509 leaf), ClientKeyExchange
//!   (and the P-256 point), CertificateVerify (and the DER signature),
//!   Finished and SDP fingerprint parsers, or all of them.
//! - [`dtls_server`]: a scripted run of a [`crate::Server`] against the test
//!   client, interleaving raw datagrams, mutated or dropped or replayed
//!   client datagrams, timer firings and sealed records, with invariants on
//!   state size, output per input, crypto work and timers after every step.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]

use crate::message::{self, ClientHello};
use crate::record::{self, Protection, ReplayWindow};
use crate::testing::{self, Client, Options};
use crate::{Config, Event, Fingerprint, Identity, MAX_CHAIN, Server, State, identity};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Most bytes one input may make the server send: a flight and an alert.
pub const MAX_OUTPUT_PER_INPUT: usize = 4096;
/// Most script steps read from one input.
pub const MAX_STEPS: usize = 64;

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

/// One datagram through the record layer. Returns the records framed.
pub fn dtls_record(data: &[u8]) -> usize {
    let key = Protection::new(&[0x42; 16], &[1, 2, 3, 4]).expect("key");
    let mut window = ReplayWindow::default();
    let mut framed = 0;
    let mut consumed = 0;
    for r in record::records(data) {
        framed += 1;
        consumed += record::HEADER_LEN + r.body.len();
        assert!(consumed <= data.len());
        assert!(r.body.len() <= record::MAX_CIPHERTEXT);
        assert!(r.seq <= record::MAX_SEQUENCE);
        for f in message::fragments(r.body) {
            assert!(u64::from(f.offset) + f.data.len() as u64 <= u64::from(f.length));
        }
        if r.epoch == 1 {
            // Almost always fails to authenticate; must never panic.
            let _ = key.open(&r);
        }
        if window.check(r.seq) {
            window.accept(r.seq);
            assert!(!window.check(r.seq), "accepted twice");
        }
        if r.body.len() <= record::MAX_PLAINTEXT {
            let mut sealed = Vec::new();
            key.seal(&mut sealed, r.content_type, r.epoch, r.seq, r.body)
                .expect("seal");
            let back = record::records(&sealed).next().expect("framed");
            assert_eq!(key.open(&back).expect("opens").as_slice(), r.body);
        }
    }
    framed
}

/// One message body through a parser chosen by the first byte (0 to 4),
/// or all of them (5 and up). Returns how many accepted it.
pub fn dtls_handshake_messages(data: &[u8]) -> usize {
    let Some((&selector, body)) = data.split_first() else {
        return 0;
    };
    let all = selector % 6 == 5;
    let pick = |n| all || selector % 6 == n;
    let mut accepted = 0;
    if pick(0)
        && let Ok(hello) = ClientHello::parse(body)
    {
        accepted += 1;
        assert!(hello.session_id.len() <= 32);
        assert!(hello.cipher_suites.len().is_multiple_of(2));
        if let Ok(n) = hello.negotiate() {
            assert!(matches!(n.profile.keying_material_len(), 56 | 60));
            assert_eq!(hello.use_srtp.map(|s| s.mki_len), Some(0));
            assert!(hello.extended_master_secret);
        }
    }
    if pick(1) {
        if let Ok(leaf) = message::certificate_leaf(body, MAX_CHAIN) {
            accepted += 1;
            assert!(!leaf.is_empty());
            let _ = Fingerprint::of_certificate(leaf);
            let _ = identity::peer_key(leaf);
        }
        // The X.509 parser on its own, without the TLS framing.
        let _ = identity::peer_key(body);
    }
    if pick(2)
        && let Ok(point) = message::client_key_exchange(body)
    {
        accepted += 1;
        assert_eq!(point.len(), 65);
        let _ = p256::PublicKey::from_sec1_bytes(point);
    }
    if pick(3)
        && let Ok(sig) = message::certificate_verify(body)
    {
        accepted += 1;
        let id = fixed_identity();
        // A signature over another digest never verifies.
        let r = identity::verify_der(id.key().verifying_key(), &[0x55; 32], sig);
        assert!(r.is_err());
    }
    if pick(4) && message::finished(body).is_ok() {
        accepted += 1;
    }
    if all
        && let Ok(s) = std::str::from_utf8(body)
        && let Some(f) = Fingerprint::parse_sdp(s)
    {
        accepted += 1;
        assert_eq!(Fingerprint::parse_sdp(&f.to_sdp()), Some(f));
    }
    accepted
}

fn fixed_identity() -> Arc<Identity> {
    testing::identity(1)
}

/// The seed `tests/openssl_interop.sh` gives the server on port 47010.
const INTEROP_SEED: [u8; 32] = {
    let mut s = *b"SaplowDTDSOpenSSLInterop\0\0\0\0\0\0\0\0";
    s[31] = 0x10;
    s
};

fn interop_identity() -> Arc<Identity> {
    use std::sync::OnceLock;
    static ID: OnceLock<Arc<Identity>> = OnceLock::new();
    ID.get_or_init(|| {
        Arc::new(Identity::generate_seeded(INTEROP_SEED, testing::TEST_TIME).expect("identity"))
    })
    .clone()
}

/// How far a [`dtls_server`] run got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reached {
    /// No ClientHello accepted.
    Nothing,
    /// Flight 4 sent.
    Handshaking,
    /// The server connected.
    Connected,
    /// Both sides connected, with equal keys.
    BothConnected,
}

struct Harness {
    server: Server,
    config: Config,
    expected: Fingerprint,
    client: Client,
    pending: Vec<Vec<u8>>,
    history: Vec<Vec<u8>>,
    server_out: Vec<Vec<u8>>,
    now: Instant,
    connected: Option<Vec<u8>>,
    reached: Reached,
    total_output: usize,
}

/// Most bytes a whole run may make the server send, whatever the peer
/// does: every flight at most `max_transmissions` times (flight 4 under
/// 2 KB, the final flight and each HelloVerifyRequest small), at most
/// `max_transmissions` HelloVerifyRequest flights, and a few alerts.
pub fn total_output_bound(config: &Config) -> usize {
    let t = config.max_transmissions as usize;
    t * 2048 + t * t * 128 + t * 256 + 4 * 64
}

impl Harness {
    fn check(&mut self) {
        let mut produced = 0;
        while let Some(d) = self.server.poll_transmit() {
            produced += d.len();
            assert!(d.len() <= self.config.mtu.max(64), "datagram above the MTU");
            if self.server_out.len() < 64 {
                self.server_out.push(d);
            }
        }
        assert!(
            produced <= MAX_OUTPUT_PER_INPUT,
            "{produced} bytes for one input"
        );
        assert!(self.server.buffered() <= self.config.max_buffered);
        let s = self.server.stats();
        assert!(s.signatures <= 1 && s.key_agreements <= 1 && s.verifications <= 1);
        self.total_output += produced;
        assert!(
            self.total_output <= total_output_bound(&self.config),
            "total output"
        );
        let state = self.server.state();
        if self.server.poll_timeout().is_some() {
            assert_eq!(
                state,
                State::Handshaking,
                "a timer only while waiting for flight 5"
            );
        }
        if state == State::Handshaking {
            self.reached = self.reached.max(Reached::Handshaking);
        }
        while let Some(ev) = self.server.poll_event() {
            match ev {
                Event::Connected(c) => {
                    assert!(self.connected.is_none(), "connected twice");
                    assert_eq!(c.peer_fingerprint, self.expected);
                    assert_eq!(
                        c.keying_material.as_bytes().len(),
                        c.profile.keying_material_len()
                    );
                    self.connected = Some(c.keying_material.as_bytes().to_vec());
                    self.reached = self.reached.max(Reached::Connected);
                }
                Event::Closed => assert!(self.connected.is_some()),
            }
        }
        if let (Some(ours), true) = (&self.connected, self.client.connected()) {
            let theirs = self.client.keying_material.as_deref().expect("client keys");
            assert_eq!(ours.as_slice(), theirs.as_slice(), "both sides agree");
            self.reached = Reached::BothConnected;
        }
    }

    fn input(&mut self, d: &[u8]) {
        let before = self.server.state();
        let _ = self.server.handle_datagram(self.now, d);
        if matches!(before, State::Failed(_) | State::Closed) {
            assert!(
                self.server.poll_transmit().is_none(),
                "silent after the end"
            );
        }
        self.check();
    }
}

/// A scripted server run. The first byte sets the mode:
///
/// - bit 0: `hello_verify`;
/// - bit 1: a 256-byte MTU;
/// - bit 2: the client offers only `SRTP_AES128_CM_HMAC_SHA1_80`;
/// - bit 3: the client fragments to 90-byte datagrams;
/// - bit 4: the server expects another fingerprint;
/// - bit 5: the server is the seeded OpenSSL interop server, and the next 32
///   bytes are the fingerprint it expects (so op 0 can replay a recorded
///   OpenSSL run's flights).
///
/// Then steps of `[op, u16 length, bytes]`, op modulo 8: 0 a raw datagram;
/// 1 the next client datagram with `(u16 position, xor)` mutations; 2 drop
/// the next client datagram (op 5 can send it later); 3 advance time by `bytes[0]` times 50 ms and
/// fire the timer if due; 4 deliver the server's output to the client; 5
/// replay an earlier client datagram; 6 a sealed client record (type
/// `bytes[0]`, body the rest) once the client has keys; 7 the next client
/// datagram twice.
pub fn dtls_server(data: &[u8]) -> Reached {
    let Some((&mode, mut script)) = data.split_first() else {
        return Reached::Nothing;
    };
    let interop = mode & 32 != 0;
    let mut interop_peer = Fingerprint::from_bytes([0; 32]);
    if interop {
        let Some((fp, rest)) = script.split_first_chunk::<32>() else {
            return Reached::Nothing;
        };
        interop_peer = Fingerprint::from_bytes(*fp);
        script = rest;
    }
    let config = Config {
        hello_verify: mode & 1 != 0,
        mtu: if mode & 2 != 0 { 256 } else { 1200 },
        initial_retransmit: Duration::from_millis(100),
        max_retransmit: Duration::from_millis(800),
        ..Config::default()
    };
    let options = Options {
        srtp_profiles: Some(if mode & 4 != 0 { vec![1] } else { vec![1, 7] }),
        mtu: if mode & 8 != 0 { 90 } else { 1200 },
        ..Options::default()
    };
    let client = Client::new(options, testing::identity(2), 3);
    let (identity, seed, expected) = if interop {
        (interop_identity(), INTEROP_SEED, interop_peer)
    } else if mode & 16 != 0 {
        (
            testing::identity(1),
            [7; 32],
            Fingerprint::from_bytes([0xee; 32]),
        )
    } else {
        (testing::identity(1), [7; 32], client.fingerprint())
    };
    let server = Server::new_seeded(config.clone(), identity, &[expected], seed).expect("server");
    let mut h = Harness {
        server,
        config,
        expected,
        client,
        pending: Vec::new(),
        history: Vec::new(),
        server_out: Vec::new(),
        now: Instant::now(),
        connected: None,
        reached: Reached::Nothing,
        total_output: 0,
    };
    h.pending = h.client.start();
    for (op, bytes) in steps(script) {
        match op % 8 {
            0 => h.input(bytes),
            1 | 7 => {
                if h.pending.is_empty() {
                    continue;
                }
                let mut d = h.pending.remove(0);
                for m in bytes.chunks_exact(3) {
                    if !d.is_empty() {
                        let at = usize::from(u16::from_be_bytes([m[0], m[1]])) % d.len();
                        d[at] ^= m[2];
                    }
                }
                h.input(&d);
                if op % 8 == 7 {
                    h.input(&d);
                }
                if h.history.len() < 32 {
                    h.history.push(d);
                }
            }
            2 => {
                // Lost; op 5 can send it later, as the client's
                // retransmission.
                if !h.pending.is_empty() {
                    let d = h.pending.remove(0);
                    if h.history.len() < 32 {
                        h.history.push(d);
                    }
                }
            }
            3 => {
                let step = u64::from(bytes.first().copied().unwrap_or(1));
                h.now += Duration::from_millis(50 * step);
                if h.server.poll_timeout().is_some_and(|t| t <= h.now) {
                    let _ = h.server.handle_timeout(h.now);
                }
                h.check();
            }
            4 => {
                for d in std::mem::take(&mut h.server_out) {
                    if let Ok(more) = h.client.handle(&d) {
                        h.pending.extend(more);
                    }
                }
                h.pending.truncate(32);
                h.check();
            }
            5 => {
                if !h.history.is_empty() {
                    let k = usize::from(bytes.first().copied().unwrap_or(0)) % h.history.len();
                    let d = h.history[k].clone();
                    h.input(&d);
                }
            }
            _ => {
                if h.client.has_keys() && !bytes.is_empty() {
                    let d = h.client.seal(bytes[0], &bytes[1..]);
                    h.input(&d);
                }
            }
        }
    }
    h.reached
}

#[cfg(test)]
mod tests;
