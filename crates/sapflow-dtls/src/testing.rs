//! A deterministic DTLS 1.2 client, for tests and the fuzz harness only.
//!
//! It shares this crate's record, PRF and message code, so it cannot catch
//! a misreading that both sides share; the black-box OpenSSL runs
//! (`tests/fixtures/openssl-*`) are the independent check. What it gives
//! is reach: complete handshakes driven datagram by datagram, with knobs
//! for every refusal the server must make.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs
)]

use crate::codec::{Reader, put_u16, put_vec8, put_vec16, put_vec24};
use crate::crypto::{self, Entropy, KeyBlock, MasterSecret};
use crate::flight::{self, Flight, OutMessage, Sequences};
use crate::identity::{self, Fingerprint, Identity};
use crate::message::{self, ext};
use crate::reassembly::{Limits, Message, Push, Reassembler};
use crate::record::{self, Protection};
use p256::elliptic_curve::sec1::ToSec1Point;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use zeroize::Zeroizing;

/// The wall clock the seeded test identities use: 2026-10-27.
pub const TEST_TIME: Duration = Duration::from_secs(1_793_000_000);

/// How the client signals RFC 5746.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renegotiation {
    Extension,
    Scsv,
    None,
    NonEmpty,
}

/// A deliberate fault in the client's flight 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Sign CertificateVerify with a key that is not the certificate's.
    WrongSigningKey,
    /// Flip a bit in the Finished verify_data.
    BadFinished,
    /// Use `ecdsa_secp384r1_sha384` as the CertificateVerify scheme.
    VerifyScheme,
    /// Send an empty Certificate message.
    NoCertificate,
    /// Send a compressed point in ClientKeyExchange.
    CompressedPoint,
    /// Send an uncompressed point (`0x04` prefix) whose y is not on the
    /// curve for its x.
    OffCurvePoint,
    /// Send an uncompressed point whose x coordinate is above the field
    /// prime.
    CoordinateAboveP,
    /// Leave ChangeCipherSpec out.
    NoChangeCipherSpec,
}

/// ClientHello and flight knobs.
#[derive(Debug, Clone)]
pub struct Options {
    pub record_version: u16,
    pub client_version: u16,
    pub suites: Vec<u16>,
    pub renegotiation: Renegotiation,
    pub extended_master_secret: bool,
    pub groups: Option<Vec<u16>>,
    pub signature_algorithms: Option<Vec<u16>>,
    pub point_formats: Option<Vec<u8>>,
    pub srtp_profiles: Option<Vec<u16>>,
    pub mki: Vec<u8>,
    /// Extensions appended as given (for duplicates and unknown types).
    pub extra: Vec<(u16, Vec<u8>)>,
    /// Supported versions and a key share, as a DTLS 1.3 client offers.
    pub offer_dtls13: bool,
    /// Fragment our messages to this datagram size.
    pub mtu: usize,
    pub fault: Fault,
    /// Send this DER as our certificate instead of our own.
    pub certificate: Option<Vec<u8>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            record_version: record::DTLS_1_0,
            client_version: record::DTLS_1_2,
            suites: vec![0x1301, message::SUITE, 0xc02f],
            renegotiation: Renegotiation::Extension,
            extended_master_secret: true,
            groups: Some(vec![0x001d, message::SECP256R1]),
            signature_algorithms: Some(vec![message::ECDSA_P256_SHA256, 0x0804]),
            point_formats: Some(vec![0]),
            srtp_profiles: Some(vec![0x0001, 0x0008, 0x0007]),
            mki: Vec::new(),
            extra: Vec::new(),
            offer_dtls13: false,
            mtu: 1200,
            fault: Fault::None,
            certificate: None,
        }
    }
}

impl Options {
    /// The ClientHello body.
    pub fn hello_body(&self, random: &[u8; 32], cookie: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        put_u16(&mut b, self.client_version);
        b.extend_from_slice(random);
        put_vec8(&mut b, &[]).unwrap();
        put_vec8(&mut b, cookie).unwrap();
        let mut suites: Vec<u8> = self.suites.iter().flat_map(|s| s.to_be_bytes()).collect();
        if self.renegotiation == Renegotiation::Scsv {
            suites.extend_from_slice(&message::RENEGOTIATION_SCSV.to_be_bytes());
        }
        put_vec16(&mut b, &suites).unwrap();
        put_vec8(&mut b, &[0]).unwrap();
        let mut e = Vec::new();
        let mut add = |kind: u16, data: &[u8]| {
            put_u16(&mut e, kind);
            put_vec16(&mut e, data).unwrap();
        };
        let list16 = |v: &[u16]| {
            let mut l = Vec::new();
            put_vec16(
                &mut l,
                &v.iter().flat_map(|x| x.to_be_bytes()).collect::<Vec<_>>(),
            )
            .unwrap();
            l
        };
        if self.offer_dtls13 {
            add(43, &[4, 0xfe, 0xfc, 0xfe, 0xfd]);
        }
        if self.extended_master_secret {
            add(ext::EXTENDED_MASTER_SECRET, &[]);
        }
        if let Some(p) = &self.srtp_profiles {
            let mut d = list16(p);
            put_vec8(&mut d, &self.mki).unwrap();
            add(ext::USE_SRTP, &d);
        }
        match self.renegotiation {
            Renegotiation::Extension => add(ext::RENEGOTIATION_INFO, &[0]),
            Renegotiation::NonEmpty => add(ext::RENEGOTIATION_INFO, &[2, 9, 9]),
            _ => {}
        }
        if self.offer_dtls13 {
            let mut ks = Vec::new();
            put_u16(&mut ks, 0x001d);
            put_vec16(&mut ks, &[7; 32]).unwrap();
            let mut d = Vec::new();
            put_vec16(&mut d, &ks).unwrap();
            add(51, &d);
        }
        if let Some(g) = &self.groups {
            add(ext::SUPPORTED_GROUPS, &list16(g));
        }
        if let Some(s) = &self.signature_algorithms {
            add(ext::SIGNATURE_ALGORITHMS, &list16(s));
        }
        if let Some(p) = &self.point_formats {
            let mut d = Vec::new();
            put_vec8(&mut d, p).unwrap();
            add(ext::EC_POINT_FORMATS, &d);
        }
        for (kind, data) in &self.extra {
            add(*kind, data);
        }
        put_vec16(&mut b, &e).unwrap();
        b
    }
}

/// Rewrites the record version of every record in a datagram.
pub fn set_record_version(datagram: &mut [u8], version: u16) {
    let mut at = 0;
    while at + record::HEADER_LEN <= datagram.len() {
        datagram[at + 1..at + 3].copy_from_slice(&version.to_be_bytes());
        let len = u16::from_be_bytes([datagram[at + 11], datagram[at + 12]]) as usize;
        at += record::HEADER_LEN + len;
    }
}

/// A seeded identity for tests, made once per seed and shared.
pub fn identity(seed: u8) -> Arc<Identity> {
    static CACHE: OnceLock<Mutex<HashMap<u8, Arc<Identity>>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cache
        .entry(seed)
        .or_insert_with(|| {
            Arc::new(Identity::generate_seeded([seed; 32], TEST_TIME).expect("identity"))
        })
        .clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    ServerFlight,
    Finished,
    Done,
}

/// The client.
pub struct Client {
    pub options: Options,
    identity: Arc<Identity>,
    expected_server: Option<Fingerprint>,
    entropy: Entropy,
    random: [u8; 32],
    hello_seq: u16,
    hello: Vec<u8>,
    reassembler: Reassembler,
    seqs: Sequences,
    transcript: Sha256,
    server_random: [u8; 32],
    server_key: Option<p256::ecdsa::VerifyingKey>,
    server_point: Option<p256::PublicKey>,
    profile: Option<u16>,
    master: Option<MasterSecret>,
    write: Option<Protection>,
    read: Option<Protection>,
    expected_finished: [u8; 12],
    wait: Wait,
    last_flight: Vec<Vec<u8>>,
    pub keying_material: Option<Zeroizing<Vec<u8>>>,
    pub hello_verify_requests: u32,
}

impl Client {
    pub fn new(options: Options, identity: Arc<Identity>, seed: u8) -> Self {
        let mut entropy = Entropy::Seeded {
            seed: Zeroizing::new([seed ^ 0x5a; 32]),
            counter: 0,
        };
        let random = *entropy.array::<32>().unwrap();
        Self {
            options,
            identity,
            expected_server: None,
            entropy,
            random,
            hello_seq: 0,
            hello: Vec::new(),
            reassembler: Reassembler::new(Limits {
                max_message: 1 << 16,
                max_fragments: 64,
                max_buffered: 1 << 17,
                ahead: 8,
            }),
            seqs: Sequences::default(),
            transcript: Sha256::new(),
            server_random: [0; 32],
            server_key: None,
            server_point: None,
            profile: None,
            master: None,
            write: None,
            read: None,
            expected_finished: [0; 12],
            wait: Wait::ServerFlight,
            last_flight: Vec::new(),
            keying_material: None,
            hello_verify_requests: 0,
        }
    }

    /// Refuse a server whose certificate is not this one.
    pub fn expect_server(&mut self, f: Fingerprint) {
        self.expected_server = Some(f);
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.identity.fingerprint()
    }

    pub fn connected(&self) -> bool {
        self.wait == Wait::Done
    }

    pub fn profile(&self) -> Option<u16> {
        self.profile
    }

    /// The datagrams of the last flight we sent, byte for byte.
    pub fn last_flight(&self) -> &[Vec<u8>] {
        &self.last_flight
    }

    fn send(&mut self, f: &Flight) -> Vec<Vec<u8>> {
        let write = if f.finished.is_some() {
            self.write.as_ref()
        } else {
            None
        };
        let mut grams = flight::render(f, self.options.mtu, &mut self.seqs, write).unwrap();
        if f.messages.first().map(|m| m.msg_type) == Some(message::CLIENT_HELLO) {
            for g in &mut grams {
                set_record_version(g, self.options.record_version);
            }
        }
        self.last_flight = grams.clone();
        grams
    }

    fn hello_flight(&mut self, cookie: &[u8]) -> Vec<Vec<u8>> {
        self.hello = self.options.hello_body(&self.random, cookie);
        let m = OutMessage {
            msg_type: message::CLIENT_HELLO,
            seq: self.hello_seq,
            body: self.hello.clone(),
        };
        self.transcript = Sha256::new();
        self.transcript.update(m.whole().unwrap());
        self.send(&Flight {
            messages: vec![m],
            finished: None,
        })
    }

    /// The first flight.
    pub fn start(&mut self) -> Vec<Vec<u8>> {
        self.hello_flight(&[])
    }

    /// One datagram from the server; returns what to send back.
    pub fn handle(&mut self, datagram: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let mut out = Vec::new();
        for rec in record::records(datagram) {
            let plain: Vec<u8> = match rec.epoch {
                0 => rec.body.to_vec(),
                1 => match &self.read {
                    Some(r) => r.open(&rec).ok_or("bad record")?.to_vec(),
                    None => continue,
                },
                _ => continue,
            };
            match rec.content_type {
                record::ALERT => return Err(format!("alert {plain:?}")),
                record::CHANGE_CIPHER_SPEC => {
                    if let Some(m) = &self.master {
                        let keys = KeyBlock::derive(m, &self.random, &self.server_random).unwrap();
                        self.read = Some(
                            Protection::new(keys.server_key().unwrap(), keys.server_iv().unwrap())
                                .unwrap(),
                        );
                    }
                }
                record::HANDSHAKE => {
                    for f in message::fragments(&plain) {
                        let epoch = rec.epoch;
                        match self
                            .reassembler
                            .push(epoch, &f)
                            .map_err(|e| format!("{e}"))?
                        {
                            Push::Stored => {
                                while let Some(m) = self.reassembler.pop() {
                                    out.extend(self.message(m)?);
                                }
                            }
                            Push::Old { .. } | Push::Dropped => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    fn message(&mut self, m: Message) -> Result<Vec<Vec<u8>>, String> {
        let whole = OutMessage {
            msg_type: m.msg_type,
            seq: m.seq,
            body: m.body.clone(),
        }
        .whole()
        .unwrap();
        match (self.wait, m.msg_type) {
            (Wait::ServerFlight, message::HELLO_VERIFY_REQUEST) => {
                let mut r = Reader::new(&m.body);
                r.u16().ok_or("hvr")?;
                let cookie = r.vec8().ok_or("hvr")?.to_vec();
                self.hello_verify_requests += 1;
                let next = m.seq.checked_add(1).ok_or("seq")?;
                self.hello_seq = next;
                self.reassembler.restart_at(next);
                Ok(self.hello_flight(&cookie))
            }
            (Wait::ServerFlight, message::SERVER_HELLO) => {
                self.transcript.update(&whole);
                let mut r = Reader::new(&m.body);
                if r.u16() != Some(record::DTLS_1_2) {
                    return Err("server version".into());
                }
                self.server_random = r.array::<32>().ok_or("random")?;
                if self.server_random[24..31] == *b"DOWNGRD" {
                    return Err("downgrade sentinel".into());
                }
                r.vec8().ok_or("session")?;
                if r.u16() != Some(message::SUITE) || r.u8() != Some(0) {
                    return Err("suite".into());
                }
                let mut exts = Reader::new(r.vec16().ok_or("exts")?);
                while !exts.is_empty() {
                    let kind = exts.u16().ok_or("ext")?;
                    let data = exts.vec16().ok_or("ext")?;
                    if kind == ext::USE_SRTP {
                        let mut d = Reader::new(data);
                        let p = d.vec16().ok_or("srtp")?;
                        if p.len() != 2 || d.vec8() != Some(&[][..]) {
                            return Err("srtp answer".into());
                        }
                        self.profile = Some(u16::from_be_bytes([p[0], p[1]]));
                    }
                }
                Ok(Vec::new())
            }
            (Wait::ServerFlight, message::CERTIFICATE) => {
                self.transcript.update(&whole);
                let leaf = message::certificate_leaf(&m.body, 4).map_err(|e| e.to_string())?;
                if self
                    .expected_server
                    .is_some_and(|f| Fingerprint::of_certificate(leaf) != f)
                {
                    return Err("server fingerprint".into());
                }
                self.server_key = Some(identity::peer_key(leaf).map_err(|e| e.to_string())?);
                Ok(Vec::new())
            }
            (Wait::ServerFlight, message::SERVER_KEY_EXCHANGE) => {
                self.transcript.update(&whole);
                let mut r = Reader::new(&m.body);
                let params_len = 4 + 65;
                let params = m.body.get(..params_len).ok_or("ske")?.to_vec();
                if r.u8() != Some(message::NAMED_CURVE) || r.u16() != Some(message::SECP256R1) {
                    return Err("curve".into());
                }
                let point = r.vec8().ok_or("point")?;
                self.server_point =
                    Some(p256::PublicKey::from_sec1_bytes(point).map_err(|_| "point")?);
                if r.u16() != Some(message::ECDSA_P256_SHA256) {
                    return Err("ske scheme".into());
                }
                let sig = r.vec16().ok_or("sig")?;
                let digest = crypto::sha256(&[&self.random, &self.server_random, &params]);
                identity::verify_der(self.server_key.as_ref().ok_or("no key")?, &digest, sig)
                    .map_err(|_| "ske signature")?;
                Ok(Vec::new())
            }
            (Wait::ServerFlight, message::CERTIFICATE_REQUEST) => {
                self.transcript.update(&whole);
                Ok(Vec::new())
            }
            (Wait::ServerFlight, message::SERVER_HELLO_DONE) => {
                self.transcript.update(&whole);
                self.flight5()
            }
            (Wait::Finished, message::FINISHED) => {
                if m.epoch != 1 || m.body != self.expected_finished {
                    return Err("server finished".into());
                }
                self.wait = Wait::Done;
                Ok(Vec::new())
            }
            _ => Err(format!("unexpected message {}", m.msg_type)),
        }
    }

    fn flight5(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let fault = self.options.fault;
        let server_point = self.server_point.ok_or("no key exchange")?;
        if self.hello_seq > u16::MAX - 5 {
            return Err("seq".into());
        }
        let mut seq = self.hello_seq + 1;
        let mut next = || {
            let s = seq;
            seq += 1;
            s
        };
        let mut cert_body = Vec::new();
        if fault == Fault::NoCertificate {
            put_vec24(&mut cert_body, &[]).unwrap();
        } else {
            let mut list = Vec::new();
            let der = self
                .options
                .certificate
                .clone()
                .unwrap_or_else(|| self.identity.certificate_der().to_vec());
            put_vec24(&mut list, &der).unwrap();
            put_vec24(&mut cert_body, &list).unwrap();
        }
        let cert = OutMessage {
            msg_type: message::CERTIFICATE,
            seq: next(),
            body: cert_body,
        };
        let ecdh = identity::signing_key(&mut self.entropy).unwrap();
        let ecdh = p256::SecretKey::from_bytes(&ecdh.to_bytes()).unwrap();
        let point = ecdh
            .public_key()
            .to_sec1_point(fault == Fault::CompressedPoint);
        let mut encoded = point.as_bytes().to_vec();
        match fault {
            // Flipping y's low bit gives y ± 1, which is not on the curve
            // with this x (outside a negligible case).
            Fault::OffCurvePoint => encoded[64] ^= 1,
            // All ones is above p (p = 2^256 - 2^224 + 2^192 + 2^96 - 1).
            Fault::CoordinateAboveP => encoded[1..33].fill(0xff),
            _ => {}
        }
        let mut cke_body = Vec::new();
        put_vec8(&mut cke_body, &encoded).unwrap();
        let cke = OutMessage {
            msg_type: message::CLIENT_KEY_EXCHANGE,
            seq: next(),
            body: cke_body,
        };
        self.transcript.update(cert.whole().unwrap());
        self.transcript.update(cke.whole().unwrap());
        let session_hash: [u8; 32] = self.transcript.clone().finalize().into();
        let shared = p256::ecdh::diffie_hellman(ecdh.to_nonzero_scalar(), server_point.as_affine());
        let master =
            crypto::extended_master_secret(shared.raw_secret_bytes(), &session_hash).unwrap();
        let signer = if fault == Fault::WrongSigningKey {
            identity::signing_key(&mut self.entropy).unwrap()
        } else {
            self.identity.key().clone()
        };
        let sig = identity::sign_der(&signer, &session_hash).unwrap();
        let mut cv_body = Vec::new();
        let scheme = if fault == Fault::VerifyScheme {
            0x0503
        } else {
            message::ECDSA_P256_SHA256
        };
        put_u16(&mut cv_body, scheme);
        put_vec16(&mut cv_body, &sig).unwrap();
        let cv = OutMessage {
            msg_type: message::CERTIFICATE_VERIFY,
            seq: next(),
            body: cv_body,
        };
        self.transcript.update(cv.whole().unwrap());
        let hash: [u8; 32] = self.transcript.clone().finalize().into();
        let mut vd = crypto::finished(&master, b"client finished", &hash).unwrap();
        if fault == Fault::BadFinished {
            vd[0] ^= 1;
        }
        let fin = OutMessage {
            msg_type: message::FINISHED,
            seq: next(),
            body: vd.to_vec(),
        };
        self.transcript.update(fin.whole().unwrap());
        let hash: [u8; 32] = self.transcript.clone().finalize().into();
        self.expected_finished = crypto::finished(&master, b"server finished", &hash).unwrap();
        let keys = KeyBlock::derive(&master, &self.random, &self.server_random).unwrap();
        self.write =
            Some(Protection::new(keys.client_key().unwrap(), keys.client_iv().unwrap()).unwrap());
        if let Some(p) = self.profile {
            let len = if p == 7 { 56 } else { 60 };
            let mut km = Zeroizing::new(vec![0u8; len]);
            crypto::export(
                &master,
                b"EXTRACTOR-dtls_srtp",
                &self.random,
                &self.server_random,
                &mut km,
            )
            .unwrap();
            self.keying_material = Some(km);
        }
        self.master = Some(master);
        self.wait = Wait::Finished;
        let mut grams = self.send(&Flight {
            messages: vec![cert, cke, cv],
            finished: Some(fin),
        });
        if fault == Fault::NoChangeCipherSpec {
            // Rebuild without the CCS record.
            grams = grams
                .into_iter()
                .map(|g| {
                    let mut keep = Vec::new();
                    for r in record::records(&g) {
                        if r.content_type != record::CHANGE_CIPHER_SPEC {
                            let mut one = Vec::new();
                            record::put_record(&mut one, r.content_type, r.epoch, r.seq, r.body)
                                .unwrap();
                            keep.extend(one);
                        }
                    }
                    keep
                })
                .collect();
            self.last_flight = grams.clone();
        }
        Ok(grams)
    }

    /// An epoch 1 record from the client, after flight 5.
    pub fn seal(&mut self, content_type: u8, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let seq = self.seqs.take(1).expect("sequence");
        self.write
            .as_ref()
            .expect("keys")
            .seal(&mut out, content_type, 1, seq, body)
            .expect("seal");
        out
    }

    /// The epoch 1 handshake fragment of a message, sealed.
    pub fn seal_handshake(&mut self, msg_type: u8, seq: u16, body: &[u8]) -> Vec<u8> {
        let m = OutMessage {
            msg_type,
            seq,
            body: body.to_vec(),
        };
        let whole = m.whole().unwrap();
        self.seal(record::HANDSHAKE, &whole)
    }

    /// The message_seq the client's Finished used.
    pub fn finished_seq(&self) -> u16 {
        self.hello_seq.saturating_add(4)
    }

    /// Whether flight 5 has been built, so [`Client::seal`] has keys.
    pub fn has_keys(&self) -> bool {
        self.write.is_some()
    }
}

/// The W0 ClientHello captures: directory, file stem, and the profile the
/// server must choose.
pub const CAPTURES: &[(&str, &str, crate::SrtpProfile)] = {
    use crate::SrtpProfile::{AeadAes128Gcm as Gcm, Aes128CmHmacSha1_80 as Cm80};
    &[
        ("chrome-157-desktop-macos", "clienthello", Gcm),
        ("chrome-157-desktop-macos", "clienthello-over-tcp", Gcm),
        ("safari-26.6.2-macos", "clienthello", Gcm),
        ("safari-26.6.2-macos", "clienthello-over-tcp", Gcm),
        ("safari-27-ios", "clienthello", Gcm),
        ("obs-32.0.4-macos", "clienthello", Cm80),
        ("ffmpeg-8.1-reference", "clienthello-dtls-active", Cm80),
        (
            "vanadium-157-android-grapheneos",
            "clienthello-over-tcp",
            Gcm,
        ),
    ]
};

fn repo_file(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// A capture's datagrams. The hex files separate datagrams with `--`
/// lines; each size is checked against the capture's summary.
pub fn capture(dir: &str, stem: &str) -> Vec<Vec<u8>> {
    let base = format!("fixtures/whip/w0/{dir}/{stem}");
    let hex = std::fs::read_to_string(repo_file(&format!("{base}.hex"))).expect("capture");
    let summary =
        std::fs::read_to_string(repo_file(&format!("{base}-summary.txt"))).expect("summary");
    let grams: Vec<Vec<u8>> = hex
        .split("\n--\n")
        .map(|part| unhex(&part.split_whitespace().collect::<String>()))
        .collect();
    let sizes: Vec<usize> = summary
        .lines()
        .filter_map(|l| l.strip_prefix("datagram "))
        .map(|l| {
            let n = l.split(": ").nth(1).expect("size");
            n.trim_end_matches(" bytes").parse().expect("size")
        })
        .collect();
    assert_eq!(
        grams.iter().map(Vec::len).collect::<Vec<_>>(),
        sizes,
        "{base}: datagram sizes match the summary"
    );
    grams
}

/// The reassembled ClientHello body of a capture.
pub fn capture_hello(dir: &str, stem: &str) -> Vec<u8> {
    let mut r = Reassembler::new(Limits {
        max_message: 1 << 16,
        max_fragments: 64,
        max_buffered: 1 << 17,
        ahead: 1,
    });
    for g in capture(dir, stem) {
        for rec in record::records(&g) {
            for f in message::fragments(rec.body) {
                r.push(0, &f).expect("fragment");
            }
        }
    }
    let m = r.pop().expect("complete hello");
    assert_eq!(m.msg_type, message::CLIENT_HELLO);
    m.body
}

pub fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "even hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
