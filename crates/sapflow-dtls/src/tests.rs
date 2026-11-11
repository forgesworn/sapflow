//! Server tests: complete handshakes with the test client, the W0 captures
//! (A2.2), each malformed, oversized, out-of-order and retransmitted case
//! (A2.2), the fingerprint check (A2.3), and offline replays of the
//! black-box OpenSSL runs (A2.1).

use crate::crypto::{self, KeyBlock};
use crate::message::{self, Fragment};
use crate::record::{self, Protection};
use crate::testing::{self, Client, Fault, Options, TEST_TIME};
use crate::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const SEED: [u8; 32] = [0x11; 32];

struct Pair {
    server: Server,
    client: Client,
    now: Instant,
}

fn pair_with(options: Options, config: Config, expected: Option<Fingerprint>) -> Pair {
    let id = testing::identity(1);
    let client = Client::new(options, testing::identity(2), 3);
    let expected = expected.unwrap_or_else(|| client.fingerprint());
    let mut client = client;
    client.expect_server(id.fingerprint());
    Pair {
        server: Server::new_seeded(config, id, expected, SEED).expect("server"),
        client,
        now: Instant::now(),
    }
}

fn pair(options: Options) -> Pair {
    pair_with(options, Config::default(), None)
}

impl Pair {
    fn feed(&mut self, grams: &[Vec<u8>]) -> Result<(), Error> {
        for g in grams {
            self.server.handle_datagram(self.now, g)?;
        }
        Ok(())
    }

    fn drain(&mut self) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| self.server.poll_transmit()).collect()
    }

    /// Runs the exchange until neither side has anything to send.
    fn run(&mut self) -> Result<(), Error> {
        let mut to_server = self.client.start();
        for _ in 0..10 {
            self.feed(&to_server)?;
            let to_client = self.drain();
            if to_client.is_empty() {
                break;
            }
            to_server = Vec::new();
            for d in &to_client {
                to_server.extend(self.client.handle(d).expect("client"));
            }
        }
        Ok(())
    }

    fn connected(&mut self) -> Connected {
        match self.server.poll_event() {
            Some(Event::Connected(c)) => c,
            other => panic!("expected Connected, got {other:?}"),
        }
    }
}

/// The single alert record in `grams`, as `[level, description]`, read at
/// epoch 0.
fn plain_alert(grams: &[Vec<u8>]) -> [u8; 2] {
    let recs: Vec<_> = grams.iter().flat_map(|g| record::records(g)).collect();
    assert_eq!(recs.len(), 1, "one record");
    assert_eq!(recs[0].content_type, record::ALERT);
    assert_eq!(recs[0].epoch, 0);
    recs[0].body.try_into().expect("two bytes")
}

#[test]
fn handshake_completes_and_keys_match() {
    for (profiles, profile) in [
        (vec![1, 8, 7], SrtpProfile::AeadAes128Gcm),
        (vec![1], SrtpProfile::Aes128CmHmacSha1_80),
    ] {
        let mut p = pair(Options {
            srtp_profiles: Some(profiles),
            ..Options::default()
        });
        p.run().expect("handshake");
        assert!(p.client.connected());
        assert_eq!(p.server.state(), State::Connected);
        let c = p.connected();
        assert_eq!(c.profile, profile);
        assert_eq!(c.peer_fingerprint, p.client.fingerprint());
        assert_eq!(
            c.keying_material.as_bytes().len(),
            profile.keying_material_len()
        );
        assert_eq!(
            c.keying_material.as_bytes(),
            p.client
                .keying_material
                .as_deref()
                .expect("client keys")
                .as_slice()
        );
        assert_eq!(p.client.profile(), Some(profile.id()));
        assert!(!format!("{c:?}").contains(&testing::hex(&c.keying_material.as_bytes()[..4])));
        assert_eq!(
            p.server.poll_timeout(),
            None,
            "no timer after the last flight"
        );
        let s = p.server.stats();
        assert_eq!((s.signatures, s.key_agreements, s.verifications), (1, 1, 1));
        assert_eq!(p.server.buffered(), 0);
    }
}

#[test]
fn small_mtus_fragment_both_ways() {
    let config = Config {
        mtu: 256,
        ..Config::default()
    };
    let mut p = pair_with(
        Options {
            mtu: 120,
            ..Options::default()
        },
        config,
        None,
    );
    let first = p.client.start();
    assert!(first.len() > 1, "the client hello is fragmented");
    p.feed(&first).expect("hello");
    let flight = p.drain();
    assert!(flight.len() > 2);
    assert!(flight.iter().all(|d| d.len() <= 256));
    let mut back = Vec::new();
    for d in &flight {
        back.extend(p.client.handle(d).expect("client"));
    }
    p.feed(&back).expect("flight 5");
    for d in &p.drain() {
        p.client.handle(d).expect("client");
    }
    assert!(p.client.connected());
    p.connected();
}

#[test]
fn dtls13_offer_is_answered_with_1_2() {
    let mut p = pair(Options {
        offer_dtls13: true,
        client_version: 0xfefd,
        ..Options::default()
    });
    p.run().expect("handshake");
    assert!(
        p.client.connected(),
        "the client checks version, suite and sentinel"
    );
}

#[test]
fn downgrade_sentinel_is_recognised() {
    let mut r = [5u8; 32];
    assert!(!server::downgrade_sentinel(&r));
    r[24..].copy_from_slice(b"DOWNGRD\x01");
    assert!(server::downgrade_sentinel(&r));
    r[31] = 0;
    assert!(server::downgrade_sentinel(&r));
    r[31] = 2;
    assert!(!server::downgrade_sentinel(&r));
}

/// A2.3: a certificate that does not match the offer's fingerprint is
/// refused with `bad_certificate`, before the certificate is parsed.
#[test]
fn fingerprint_mismatch_is_refused() {
    let wrong = Fingerprint::from_bytes([0xab; 32]);
    let mut p = pair_with(Options::default(), Config::default(), Some(wrong));
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    let mut back = Vec::new();
    for d in &p.drain() {
        back.extend(p.client.handle(d).expect("client"));
    }
    assert_eq!(p.feed(&back), Err(Error::FingerprintMismatch));
    assert_eq!(plain_alert(&p.drain()), [2, alert::BAD_CERTIFICATE]);
    assert_eq!(p.server.state(), State::Failed(Error::FingerprintMismatch));
    assert_eq!(p.server.peer_fingerprint(), None);
    assert_eq!(p.server.stats().verifications, 0);
    assert!(p.server.poll_event().is_none());
    // Silence afterwards, whatever arrives.
    p.feed(&back).expect("ignored");
    assert!(p.drain().is_empty());
}

#[test]
fn client_flight_faults_are_refused() {
    use alert::*;
    for (fault, err, desc) in [
        (Fault::WrongSigningKey, Error::BadSignature, DECRYPT_ERROR),
        (
            Fault::VerifyScheme,
            Error::NoSignatureScheme,
            HANDSHAKE_FAILURE,
        ),
        (
            Fault::NoCertificate,
            Error::NoCertificate,
            HANDSHAKE_FAILURE,
        ),
        (
            Fault::CompressedPoint,
            Error::BadKeyShare,
            ILLEGAL_PARAMETER,
        ),
        (Fault::BadFinished, Error::BadFinished, DECRYPT_ERROR),
    ] {
        let mut p = pair(Options {
            fault,
            ..Options::default()
        });
        assert_eq!(p.run(), Err(err), "{fault:?}");
        assert_eq!(plain_alert(&p.drain()), [FATAL, desc], "{fault:?}");
        assert!(p.server.poll_event().is_none());
    }
}

#[test]
fn hello_refusal_sends_the_alert() {
    let mut p = pair(Options {
        mki: vec![1],
        ..Options::default()
    });
    assert_eq!(p.run(), Err(Error::SrtpMki));
    assert_eq!(plain_alert(&p.drain()), [2, alert::HANDSHAKE_FAILURE]);
}

/// A2.2: each capture gets a correct first flight: ServerHello with the
/// suite and the profile, our certificate, a signed key exchange, a
/// certificate request and ServerHelloDone. A capture cannot go further
/// without the browser's key, so the handshake then times out cleanly.
#[test]
fn captured_hellos_get_a_first_flight_then_time_out() {
    for &(dir, stem, profile) in testing::CAPTURES {
        let id = testing::identity(1);
        let config = Config::default();
        let mut s = Server::new_seeded(
            config.clone(),
            id.clone(),
            Fingerprint::from_bytes([0; 32]),
            SEED,
        )
        .expect("server");
        let now = Instant::now();
        for g in testing::capture(dir, stem) {
            s.handle_datagram(now, &g)
                .unwrap_or_else(|e| panic!("{dir}/{stem}: {e}"));
        }
        assert_eq!(s.state(), State::Handshaking, "{dir}/{stem}");
        let flight: Vec<_> = std::iter::from_fn(|| s.poll_transmit()).collect();
        assert!(!flight.is_empty());
        let types = flight_types(&flight);
        assert_eq!(
            types,
            [
                message::SERVER_HELLO,
                message::CERTIFICATE,
                message::SERVER_KEY_EXCHANGE,
                message::CERTIFICATE_REQUEST,
                message::SERVER_HELLO_DONE
            ],
            "{dir}/{stem}"
        );
        let hello = server_hello_of(&flight);
        assert_eq!(
            &hello[35..37],
            &message::SUITE.to_be_bytes(),
            "{dir}/{stem}"
        );
        let [hi, lo] = profile.id().to_be_bytes();
        assert!(
            hello
                .windows(9)
                .any(|w| w == [0, 14, 0, 5, 0, 2, hi, lo, 0]),
            "{dir}/{stem}: use_srtp answers {profile:?}"
        );
        // The first datagram fits the default MTU.
        assert!(flight.iter().all(|d| d.len() <= config.mtu));
        // Then: retransmissions with backoff, and a clean timeout.
        let mut at = now;
        let mut sends = 1;
        while let Some(t) = s.poll_timeout() {
            assert!(t > at);
            at = t;
            match s.handle_timeout(t) {
                Ok(()) => {
                    sends += 1;
                    let again: Vec<_> = std::iter::from_fn(|| s.poll_transmit()).collect();
                    assert_eq!(flight_types(&again), types, "same messages, new records");
                    assert_ne!(again, flight, "new record sequence numbers");
                }
                Err(e) => {
                    assert_eq!(e, Error::Timeout);
                    break;
                }
            }
        }
        assert_eq!(sends, config.max_transmissions);
        assert_eq!(s.state(), State::Failed(Error::Timeout));
        assert!(s.poll_transmit().is_none(), "a timeout sends no alert");
    }
}

fn flight_types(grams: &[Vec<u8>]) -> Vec<u8> {
    let mut types = Vec::new();
    for g in grams {
        for r in record::records(g) {
            for f in message::fragments(r.body) {
                if f.offset == 0 {
                    types.push(f.msg_type);
                }
            }
        }
    }
    types
}

fn server_hello_of(grams: &[Vec<u8>]) -> Vec<u8> {
    let r = record::records(&grams[0]).next().expect("record");
    let f = message::fragments(r.body).next().expect("fragment");
    assert_eq!(f.msg_type, message::SERVER_HELLO);
    assert_eq!(f.data.len() as u32, f.length);
    f.data.to_vec()
}

#[test]
fn backoff_doubles_to_the_cap() {
    let config = Config {
        initial_retransmit: Duration::from_millis(100),
        max_retransmit: Duration::from_millis(350),
        max_transmissions: 6,
        ..Config::default()
    };
    let mut p = pair_with(Options::default(), config, None);
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    p.drain();
    let mut gaps = Vec::new();
    let mut last = p.now;
    while let Some(t) = p.server.poll_timeout() {
        gaps.push((t - last).as_millis());
        last = t;
        if p.server.handle_timeout(t).is_err() {
            break;
        }
        p.drain();
    }
    assert_eq!(gaps, [100, 200, 350, 350, 350, 350]);
}

#[test]
fn retransmitted_hello_resends_once_per_spacing_and_within_budget() {
    let mut p = pair(Options::default());
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    let first = p.drain();
    // Immediately repeated: too soon after our send.
    p.feed(&hello).expect("repeat");
    assert!(p.drain().is_empty());
    // Half the initial timeout later: resent, with new record numbers.
    p.now += Duration::from_millis(500);
    p.feed(&hello).expect("repeat");
    let again = p.drain();
    assert_eq!(flight_types(&again), flight_types(&first));
    assert_ne!(again, first);
    // A datagram carrying the hello twice resends at most once.
    p.now += Duration::from_millis(600);
    let doubled = [hello[0].clone(), hello[0].clone()].concat();
    p.feed(&[doubled]).expect("doubled");
    assert_eq!(p.drain().len(), again.len());
    // The budget caps the total, whatever the peer sends.
    for _ in 0..20 {
        p.now += Duration::from_secs(1);
        p.feed(&hello).expect("repeat");
        p.drain();
    }
    assert_eq!(p.server.stats().retransmissions, 5, "six sends in all");
    assert_eq!(
        p.server.stats().signatures,
        1,
        "the flight is cached, not rebuilt"
    );
}

#[test]
fn a_short_prefix_of_the_hello_does_not_trigger_a_resend() {
    let mut p = pair(Options::default());
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    p.drain();
    let random_len = 34;
    let r = record::records(&hello[0]).next().expect("record");
    let f = message::fragments(r.body).next().expect("fragment");
    for n in [1, 2, random_len - 1] {
        p.now += Duration::from_secs(1);
        let d = fragment_record(1, 0, f.length, 0, &f.data[..n], 50 + n as u64);
        p.feed(&[d]).expect("prefix");
        assert!(p.drain().is_empty(), "{n}-byte prefix");
    }
    p.now += Duration::from_secs(1);
    let d = fragment_record(1, 0, f.length, 0, &f.data[..random_len], 90);
    p.feed(&[d]).expect("prefix");
    assert!(!p.drain().is_empty(), "version and random repeated: resent");
}

#[test]
fn a_different_hello_under_the_same_seq_is_ignored() {
    let mut p = pair(Options::default());
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    p.drain();
    let mut other = Client::new(Options::default(), testing::identity(2), 99);
    p.now += Duration::from_secs(1);
    p.feed(&other.start()).expect("other hello");
    assert!(p.drain().is_empty());
    assert_eq!(p.server.stats().signatures, 1);
}

#[test]
fn out_of_order_and_overlapping_hello_fragments() {
    let mut c = Client::new(
        Options {
            mtu: 70,
            ..Options::default()
        },
        testing::identity(2),
        3,
    );
    let mut grams = c.start();
    assert!(grams.len() >= 3);
    grams.reverse();
    let dup = grams[1].clone();
    grams.insert(0, dup);
    let mut p = pair(Options::default());
    p.client = c;
    p.feed(&grams).expect("hello");
    assert_eq!(p.server.state(), State::Handshaking);
}

/// One record whose single fragment is `data` at `offset` of a message of
/// `length`.
fn fragment_record(
    msg_type: u8,
    seq: u16,
    length: u32,
    offset: u32,
    data: &[u8],
    rseq: u64,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(msg_type);
    body.extend_from_slice(&length.to_be_bytes()[1..]);
    body.extend_from_slice(&seq.to_be_bytes());
    body.extend_from_slice(&offset.to_be_bytes()[1..]);
    body.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
    body.extend_from_slice(data);
    let mut out = Vec::new();
    record::put_record(&mut out, record::HANDSHAKE, 0, rseq, &body).expect("record");
    out
}

#[test]
fn oversized_hello_is_refused() {
    let mut p = pair(Options::default());
    let d = fragment_record(1, 0, 8193, 0, &[1; 100], 0);
    assert_eq!(p.feed(&[d]), Err(Error::Limit(Limit::MessageSize)));
    assert_eq!(plain_alert(&p.drain()), [2, alert::HANDSHAKE_FAILURE]);
}

#[test]
fn too_many_fragments_are_refused() {
    let mut p = pair(Options::default());
    let mut last = Ok(());
    for i in 0..33u64 {
        last = p.feed(&[fragment_record(1, 0, 4000, 0, &[1; 10], i)]);
    }
    assert_eq!(last, Err(Error::Limit(Limit::Fragments)));
}

#[test]
fn buffered_bytes_are_capped() {
    let mut p = pair(Options::default());
    // Messages 0 to 2 at 6000 declared bytes each: 18000 > 16384.
    p.feed(&[fragment_record(1, 0, 6000, 0, &[1], 0)])
        .expect("first");
    p.feed(&[fragment_record(1, 1, 6000, 0, &[1], 1)])
        .expect("second");
    assert_eq!(p.server.buffered(), 12000);
    assert_eq!(
        p.feed(&[fragment_record(1, 2, 6000, 0, &[1], 2)]),
        Err(Error::Limit(Limit::Buffered))
    );
}

#[test]
fn malformed_records_are_dropped_silently() {
    let mut p = pair(Options::default());
    let hello = p.client.start();
    let g = &hello[0];
    let mut cases: Vec<Vec<u8>> = vec![
        g[..12].to_vec(),          // truncated header
        g[..g.len() - 1].to_vec(), // length past the datagram
        vec![0; 1501],             // above max_datagram
        vec![],                    // empty
    ];
    let mut bad_version = g.clone();
    bad_version[1..3].copy_from_slice(&[0x03, 0x03]);
    cases.push(bad_version);
    let mut epoch1 = g.clone();
    epoch1[3..5].copy_from_slice(&[0, 1]);
    cases.push(epoch1);
    let mut epoch2 = g.clone();
    epoch2[3..5].copy_from_slice(&[0, 2]);
    cases.push(epoch2);
    let mut app = g.clone();
    app[0] = record::APPLICATION_DATA;
    cases.push(app);
    let mut overlong = g.clone();
    overlong[11..13].copy_from_slice(&[0xff, 0xff]);
    cases.push(overlong);
    for c in &cases {
        p.feed(std::slice::from_ref(c)).expect("dropped");
        assert!(p.drain().is_empty());
        assert_eq!(p.server.state(), State::AwaitingHello);
    }
    assert_eq!(p.server.stats().datagrams_dropped, 1);
    // The real hello still works afterwards.
    p.feed(&hello).expect("hello");
    assert_eq!(p.server.state(), State::Handshaking);
}

#[test]
fn unexpected_first_message_is_refused() {
    let mut p = pair(Options::default());
    let d = fragment_record(message::CERTIFICATE, 0, 3, 0, &[0, 0, 0], 0);
    assert_eq!(p.feed(&[d]), Err(Error::UnexpectedMessage));
    assert_eq!(plain_alert(&p.drain()), [2, alert::UNEXPECTED_MESSAGE]);
}

#[test]
fn garbage_hello_is_a_decode_error() {
    let mut p = pair(Options::default());
    let d = fragment_record(1, 0, 5, 0, &[0xfe, 0xfd, 1, 2, 3], 0);
    assert_eq!(p.feed(&[d]), Err(Error::Decode));
    assert_eq!(plain_alert(&p.drain()), [2, alert::DECODE_ERROR]);
}

#[test]
fn first_records_reuse_the_hello_record_sequence_number() {
    let mut p = pair(Options::default());
    let mut hello = p.client.start();
    hello[0][5..11].copy_from_slice(&[0, 0, 0, 0, 0, 9]);
    p.feed(&hello).expect("hello");
    let flight = p.drain();
    assert_eq!(record::records(&flight[0]).next().expect("record").seq, 9);
}

/// Splits a client datagram into its records, one datagram each.
fn split_records(grams: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for g in grams {
        for r in record::records(g) {
            let mut one = Vec::new();
            record::put_record(&mut one, r.content_type, r.epoch, r.seq, r.body).expect("record");
            out.push(one);
        }
    }
    out
}

fn flight5(p: &mut Pair) -> Vec<Vec<u8>> {
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    let mut back = Vec::new();
    for d in &p.drain() {
        back.extend(p.client.handle(d).expect("client"));
    }
    split_records(&back)
}

#[test]
fn change_cipher_spec_too_early_is_dropped_and_retransmission_recovers() {
    let mut p = pair(Options::default());
    let recs = flight5(&mut p);
    // Certificate, Certificate continued?, ..., CCS, Finished: put the CCS
    // first.
    let ccs = recs
        .iter()
        .position(|r| r[0] == record::CHANGE_CIPHER_SPEC)
        .expect("ccs");
    let mut reordered = recs.clone();
    let c = reordered.remove(ccs);
    reordered.insert(0, c);
    p.feed(&reordered).expect("flight 5");
    assert!(
        p.server.poll_event().is_none(),
        "Finished could not be read"
    );
    assert_eq!(p.server.state(), State::Handshaking);
    // The client retransmits its flight (same records): now in order.
    p.feed(&recs).expect("retransmitted flight 5");
    p.connected();
}

#[test]
fn no_change_cipher_spec_means_no_finished() {
    let mut p = pair(Options {
        fault: Fault::NoChangeCipherSpec,
        ..Options::default()
    });
    let recs = flight5(&mut p);
    p.feed(&recs).expect("flight 5");
    assert_eq!(p.server.state(), State::Handshaking);
    assert!(p.server.stats().records_dropped >= 1);
}

#[test]
fn finished_at_epoch_0_is_unexpected() {
    let mut p = pair(Options::default());
    let recs = flight5(&mut p);
    let mut epoch0: Vec<Vec<u8>> = recs
        .iter()
        .filter(|r| r[0] == record::HANDSHAKE && r[3..5] == [0, 0])
        .cloned()
        .collect();
    let fin_seq = p.client.finished_seq();
    epoch0.push(fragment_record(
        message::FINISHED,
        fin_seq,
        12,
        0,
        &[0; 12],
        40,
    ));
    assert_eq!(p.feed(&epoch0), Err(Error::UnexpectedMessage));
}

fn connected_pair() -> Pair {
    let mut p = pair(Options::default());
    p.run().expect("handshake");
    p.connected();
    p
}

/// The content types of the records in `grams`.
fn record_types(grams: &[Vec<u8>]) -> Vec<u8> {
    grams
        .iter()
        .flat_map(|g| {
            record::records(g)
                .map(|r| r.content_type)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn replayed_and_tampered_records_are_dropped() {
    let mut p = connected_pair();
    let data = p.client.seal(record::APPLICATION_DATA, b"x");
    p.feed(std::slice::from_ref(&data)).expect("app data");
    p.feed(std::slice::from_ref(&data)).expect("replay");
    let mut tampered = p.client.seal(record::APPLICATION_DATA, b"y");
    let n = tampered.len();
    tampered[n - 1] ^= 1;
    p.feed(&[tampered]).expect("tampered");
    let s = p.server.stats();
    assert_eq!(s.application_data_dropped, 1, "data is ignored, once");
    assert_eq!(s.records_dropped, 2, "the replay and the forgery");
    assert!(p.drain().is_empty());
    assert_eq!(p.server.state(), State::Connected);
}

#[test]
fn retransmitted_finished_resends_our_last_flight() {
    let mut p = connected_pair();
    let fin = p.client.finished_seq();
    p.now += Duration::from_secs(1);
    let rec = p.client.seal_handshake(message::FINISHED, fin, &[0; 12]);
    p.feed(&[rec]).expect("retransmitted finished");
    let again = p.drain();
    assert_eq!(
        record_types(&again),
        [record::CHANGE_CIPHER_SPEC, record::HANDSHAKE]
    );
    for d in &again {
        p.client
            .handle(d)
            .expect("the client reads the resent Finished");
    }
    // An unauthenticated epoch 0 repeat of flight 5 does nothing.
    let hello = testing::capture("obs-32.0.4-macos", "clienthello");
    p.now += Duration::from_secs(1);
    p.feed(&hello).expect("ignored");
    assert!(p.drain().is_empty());
}

#[test]
fn renegotiation_is_refused_once_with_a_warning() {
    let mut p = connected_pair();
    let hello = p.client.options.hello_body(&[1; 32], &[]);
    let seq = p.client.finished_seq() + 1;
    let rec = p.client.seal_handshake(message::CLIENT_HELLO, seq, &hello);
    p.feed(&[rec]).expect("renegotiation");
    let out = p.drain();
    assert_eq!(out.len(), 1);
    let r = record::records(&out[0]).next().expect("record");
    assert_eq!((r.content_type, r.epoch), (record::ALERT, 1));
    let rec = p.client.seal_handshake(message::CLIENT_HELLO, seq, &hello);
    p.feed(&[rec]).expect("again");
    assert!(p.drain().is_empty(), "refused once, then silence");
    assert_eq!(p.server.state(), State::Connected);
    assert_eq!(p.server.stats().signatures, 1);
}

#[test]
fn close_notify_both_ways() {
    let mut p = connected_pair();
    let close = p.client.seal(record::ALERT, &[1, 0]);
    p.feed(&[close]).expect("close");
    assert!(matches!(p.server.poll_event(), Some(Event::Closed)));
    assert_eq!(p.server.state(), State::Closed);
    let out = p.drain();
    assert_eq!(out.len(), 1, "close_notify answered");
    let r = record::records(&out[0]).next().expect("record");
    assert_eq!(
        (r.content_type, r.epoch, r.body.len()),
        (record::ALERT, 1, 2 + 24)
    );

    let mut p = connected_pair();
    p.server.close();
    let out = p.drain();
    assert_eq!(out.len(), 1);
    assert_eq!(record::records(&out[0]).next().expect("record").epoch, 1);
    assert_eq!(p.server.state(), State::Closed);
    let data = p.client.seal(record::APPLICATION_DATA, b"late");
    p.feed(&[data]).expect("ignored");
    assert_eq!(p.server.stats().datagrams_dropped, 1);
}

#[test]
fn fatal_alert_from_the_peer_ends_the_handshake() {
    let mut p = pair(Options::default());
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    p.drain();
    let mut a = Vec::new();
    record::put_record(&mut a, record::ALERT, 0, 1, &[2, 42]).expect("record");
    assert_eq!(p.feed(&[a]), Err(Error::PeerAlert(42)));
    assert!(p.drain().is_empty(), "no alert in reply to an alert");
    // A warning (other than close_notify) is ignored during the handshake.
    let mut p = pair(Options::default());
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    let mut w = Vec::new();
    record::put_record(&mut w, record::ALERT, 0, 1, &[1, 90]).expect("record");
    p.feed(&[w]).expect("warning ignored");
    assert_eq!(p.server.state(), State::Handshaking);
}

#[test]
fn unpolled_output_is_bounded() {
    let config = Config {
        initial_retransmit: Duration::from_millis(10),
        max_retransmit: Duration::from_millis(10),
        max_transmissions: 32,
        mtu: 256,
        ..Config::default()
    };
    let mut p = pair_with(Options::default(), config, None);
    let hello = p.client.start();
    p.feed(&hello).expect("hello");
    while let Some(t) = p.server.poll_timeout() {
        if p.server.handle_timeout(t).is_err() {
            break;
        }
    }
    let queued = p.drain().len();
    assert_eq!(queued, MAX_QUEUED);
    assert!(p.server.stats().output_dropped > 0);
}

#[test]
fn cookie_exchange() {
    let config = Config {
        hello_verify: true,
        ..Config::default()
    };
    let mut p = pair_with(Options::default(), config.clone(), None);
    p.run().expect("handshake");
    assert_eq!(p.client.hello_verify_requests, 1);
    let c = p.connected();
    assert_eq!(
        c.keying_material.as_bytes(),
        p.client
            .keying_material
            .as_deref()
            .expect("keys")
            .as_slice()
    );

    // A hello whose cookie is wrong gets a fresh HelloVerifyRequest, up to
    // the budget.
    let mut p = pair_with(Options::default(), config, None);
    let mut last = Ok(());
    for seq in 0..8u16 {
        let body = p.client.options.hello_body(&[3; 32], &[0xee; 32]);
        let d = fragment_record(1, seq, body.len() as u32, 0, &body, u64::from(seq));
        last = p.feed(&[d]);
        if last.is_err() {
            break;
        }
        let out = p.drain();
        assert_eq!(flight_types(&out), [message::HELLO_VERIFY_REQUEST]);
    }
    assert_eq!(last, Err(Error::Limit(Limit::HelloVerify)));
    assert_eq!(
        p.server.stats().signatures,
        0,
        "no work before a valid cookie"
    );
}

#[test]
fn config_ranges() {
    let ok = Config::default();
    ok.validate().expect("default");
    for bad in [
        Config {
            mtu: 100,
            ..ok.clone()
        },
        Config {
            max_datagram: 70_000,
            ..ok.clone()
        },
        Config {
            max_transmissions: 0,
            ..ok.clone()
        },
        Config {
            max_message_len: 100,
            ..ok.clone()
        },
        Config {
            max_buffered: 1000,
            ..ok.clone()
        },
        Config {
            max_fragments: 0,
            ..ok.clone()
        },
        Config {
            initial_retransmit: Duration::from_secs(90),
            ..ok.clone()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
        let id = testing::identity(1);
        assert!(Server::new(bad, id, Fingerprint::from_bytes([0; 32])).is_err());
    }
}

#[test]
fn debug_output_holds_no_secrets() {
    let p = connected_pair();
    let s = format!("{:?}", p.server);
    assert!(s.contains("Connected"));
    let id = testing::identity(1);
    assert!(format!("{id:?}").starts_with("Identity { fingerprint"));
}

// A2.1: offline replays of the black-box OpenSSL runs.

struct Run {
    name: &'static str,
    expected_peer: Fingerprint,
    openssl: Vec<u8>,
    sapflow: Vec<u8>,
    client_random: [u8; 32],
    master: [u8; 48],
    lines: Vec<(char, u64, Vec<u8>)>,
}

const RUNS: &[&str] = &[
    "gcm",
    "cm80",
    "both",
    "gcm-mtu300",
    "gcm-drop",
    "cm80-mtu300-drop",
    "gcm-cookie",
    "gcm-drop-final",
];

/// The seed `tests/openssl_interop.sh` passes for port 47010.
const INTEROP_SEED: &str = "5361706c6f77445444534f70656e53534c496e7465726f700000000000000010";

fn load_run(name: &'static str) -> Run {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openssl");
    let keys = std::fs::read_to_string(dir.join(format!("{name}.keys"))).expect("keys");
    let mut k = keys.lines();
    let expected_peer = Fingerprint::parse_sdp(k.next().expect("fp")).expect("fp");
    let openssl = testing::unhex(&k.next().expect("openssl").to_lowercase());
    let sapflow = testing::unhex(&k.next().expect("sapflow").to_lowercase());
    let keylog = std::fs::read_to_string(dir.join(format!("{name}.keylog"))).expect("keylog");
    let line = keylog
        .lines()
        .find(|l| l.starts_with("CLIENT_RANDOM "))
        .expect("CLIENT_RANDOM");
    let mut parts = line.split(' ').skip(1);
    let client_random = testing::unhex(parts.next().expect("random"))
        .try_into()
        .expect("32");
    let master = testing::unhex(parts.next().expect("master"))
        .try_into()
        .expect("48");
    let record = std::fs::read_to_string(dir.join(format!("{name}.record"))).expect("record");
    let lines = record
        .lines()
        .map(|l| {
            let mut f = l.split(' ');
            let dir = f.next().expect("dir").chars().next().expect("dir");
            let ms = f.next().expect("ms").parse().expect("ms");
            (dir, ms, testing::unhex(f.next().unwrap_or("")))
        })
        .collect();
    Run {
        name,
        expected_peer,
        openssl,
        sapflow,
        client_random,
        master,
        lines,
    }
}

fn interop_config(name: &str) -> Config {
    Config {
        mtu: if name.contains("mtu300") { 300 } else { 1200 },
        hello_verify: name.contains("cookie"),
        ..Config::default()
    }
}

/// The recorded OpenSSL datagrams, fed to an identically seeded server
/// with the recorded timing, give byte-identical output and the keying
/// material OpenSSL printed. This keeps A2.1 in CI without OpenSSL.
#[test]
fn openssl_runs_replay_byte_for_byte() {
    let seed: [u8; 32] = testing::unhex(INTEROP_SEED).try_into().expect("seed");
    for name in RUNS {
        let run = load_run(name);
        assert_eq!(run.openssl, run.sapflow, "{name}: the live run matched");
        let id = Arc::new(Identity::generate_seeded(seed, TEST_TIME).expect("identity"));
        let mut s =
            Server::new_seeded(interop_config(name), id, run.expected_peer, seed).expect("server");
        let base = Instant::now();
        let mut produced = Vec::new();
        for (dir, ms, bytes) in &run.lines {
            let at = base + Duration::from_millis(*ms);
            match dir {
                'C' => s.handle_datagram(at, bytes).expect("datagram"),
                'T' => s.handle_timeout(at).expect("timeout"),
                _ => continue,
            }
            produced.extend(std::iter::from_fn(|| s.poll_transmit()));
        }
        let recorded: Vec<Vec<u8>> = run
            .lines
            .iter()
            .filter(|l| l.0 == 'S' || l.0 == 'D')
            .map(|l| l.2.clone())
            .collect();
        assert_eq!(produced, recorded, "{name}: same datagrams");
        let Some(Event::Connected(c)) = s.poll_event() else {
            panic!("{name}: connected");
        };
        assert_eq!(
            c.keying_material.as_bytes(),
            run.openssl.as_slice(),
            "{name}"
        );
        assert_eq!(c.peer_fingerprint, run.expected_peer);
        assert!(
            matches!(s.poll_event(), Some(Event::Closed)),
            "{name}: close_notify"
        );
        assert_eq!(s.state(), State::Closed, "{}", run.name);
    }
}

/// The server random from a run's first ServerHello.
fn server_random(run: &Run) -> [u8; 32] {
    for (dir, _, bytes) in &run.lines {
        if *dir != 'S' && *dir != 'D' {
            continue;
        }
        for r in record::records(bytes) {
            for f in message::fragments(r.body) {
                if f.msg_type == message::SERVER_HELLO && f.offset == 0 {
                    return f.data[2..34].try_into().expect("random");
                }
            }
        }
    }
    panic!("no ServerHello");
}

/// RFC 5705 and the key block, from OpenSSL's master secret (its key log):
/// the exporter gives the keying material OpenSSL printed, and the client
/// write key opens OpenSSL's own epoch 1 records, its Finished and its
/// close_notify (AES-128-GCM, RFC 5288).
#[test]
fn exporter_and_record_keys_from_openssls_master_secret() {
    for name in RUNS {
        let run = load_run(name);
        let master = Zeroizing::new(run.master);
        let sr = server_random(&run);
        let mut km = vec![0u8; run.openssl.len()];
        crypto::export(
            &master,
            b"EXTRACTOR-dtls_srtp",
            &run.client_random,
            &sr,
            &mut km,
        )
        .expect("export");
        assert_eq!(km, run.openssl, "{name}: exporter");
        let keys = KeyBlock::derive(&master, &run.client_random, &sr).expect("keys");
        let client = Protection::new(
            keys.client_key().expect("ok"),
            keys.client_iv().expect("ok"),
        )
        .expect("ok");
        let server = Protection::new(
            keys.server_key().expect("ok"),
            keys.server_iv().expect("ok"),
        )
        .expect("ok");
        let mut opened = Vec::new();
        for (dir, _, bytes) in &run.lines {
            for r in record::records(bytes) {
                if r.epoch != 1 {
                    continue;
                }
                let key = if *dir == 'C' { &client } else { &server };
                let plain = key
                    .open(&r)
                    .unwrap_or_else(|| panic!("{name}: {dir} record opens"));
                opened.push((*dir, r.content_type, plain.to_vec()));
            }
        }
        let fin = opened
            .iter()
            .find(|o| o.0 == 'C' && o.1 == record::HANDSHAKE)
            .expect("client finished");
        assert_eq!(&fin.2[..4], &[message::FINISHED, 0, 0, 12], "{name}");
        assert!(
            opened.contains(&('C', record::ALERT, vec![1, 0])),
            "{name}: close_notify"
        );
        assert!(
            opened.contains(&('S', record::ALERT, vec![1, 0])),
            "{name}: our close_notify"
        );
    }
}

/// TLS 1.2 PRF with SHA-256, against `openssl kdf ... TLS1-PRF` run as a
/// black box (the secret, label and seed of the widely used vector).
#[test]
fn prf_matches_openssl_kdf() {
    let secret = testing::unhex("9bbe436ba940f017b17652849a71db35");
    let seed = testing::unhex("a0ba9f936cda311827a6f796ffd5198c");
    let mut out = [0u8; 100];
    crypto::prf(&secret, b"test label", &[&seed], &mut out).expect("prf");
    assert_eq!(
        testing::hex(&out),
        "e3f229ba727be17b8d122620557cd453c2aab21d07c3d495329b52d4e61edb5a\
         6b301791e90d35c9c9a46b4e14baf9af0fa022f7077def17abfd3797c0564bab\
         4fbc91666e9def9b97fce34f796789baa48082d122ee42c5a72e5a5110fff701\
         87347b66"
    );
    let mut short = [0u8; 13];
    crypto::prf(&[0], b"x", &[], &mut short).expect("prf");
    assert_eq!(testing::hex(&short), "4fcbb82c96fe43d5a59ca22372");
}

/// The exported material drives `sapflow-rtp`: OpenSSL's half (the
/// client) protects RTP that our half unprotects, and our RTCP opens with
/// the client's.
#[test]
fn exported_keys_drive_srtp() {
    use sapflow_rtp::srtp::{self as srtp, Inbound, Outbound, Role};
    for name in RUNS {
        let run = load_run(name);
        let profile = if run.openssl.len() == 56 {
            srtp::Profile::AeadAes128Gcm
        } else {
            srtp::Profile::Aes128CmHmacSha1_80
        };
        let client =
            srtp::split_keying_material(profile, Role::Client, &run.openssl).expect("client");
        let server =
            srtp::split_keying_material(profile, Role::Server, &run.sapflow).expect("server");
        let mut rtp = vec![0x80, 96, 0, 1, 0, 0, 0, 1, 0x12, 0x34, 0x56, 0x78];
        rtp.extend_from_slice(b"hello srtp");
        let plain = rtp.clone();
        rtp.resize(plain.len() + 16, 0);
        let n = Outbound::new(&client.local)
            .expect("out")
            .protect_rtp(&mut rtp, plain.len())
            .expect("protect");
        rtp.truncate(n);
        assert_ne!(&rtp[12..plain.len()], &plain[12..], "{name}: encrypted");
        let mut inbound = Inbound::new(&server.remote, srtp::Config::default()).expect("in");
        let m = inbound.unprotect_rtp(&mut rtp).expect("unprotect");
        assert_eq!(&rtp[..m], plain.as_slice(), "{name}");
        // RTCP the other way: a receiver report from us.
        let rr = vec![0x80, 201, 0, 1, 0, 0, 0, 2];
        let mut buf = rr.clone();
        buf.resize(rr.len() + 20, 0);
        let n = Outbound::new(&server.local)
            .expect("out")
            .protect_rtcp(&mut buf, rr.len())
            .expect("protect");
        buf.truncate(n);
        let mut back = Inbound::new(&client.remote, srtp::Config::default()).expect("in");
        let m = back.unprotect_rtcp(&mut buf).expect("unprotect");
        assert_eq!(&buf[..m], rr.as_slice(), "{name}");
    }
}

#[test]
fn fragments_helper_is_consistent() {
    let d = fragment_record(1, 3, 10, 2, b"abc", 7);
    let r = record::records(&d).next().expect("record");
    let f: Vec<Fragment<'_>> = message::fragments(r.body).collect();
    assert_eq!(f[0].seq, 3);
    assert_eq!((f[0].offset, f[0].length, f[0].data), (2, 10, &b"abc"[..]));
}
