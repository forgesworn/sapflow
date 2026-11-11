//! Test only: a WHIP endpoint on loopback for real-browser runs (W3a gate
//! 2). It serves a publish page and a plain-HTTP `POST /whip` for the
//! offer, and one UDP socket that drives the sans-IO [`Session`]. It logs
//! milestones, never addresses:
//!
//! - ICE nominated;
//! - DTLS connected (with the SRTP profile);
//! - first SRTP packet decrypted;
//! - first H.264 keyframe access unit;
//! - Opus packets received;
//! - feedback sent.
//!
//! It exits 0 once the page reports done and every milestone was reached,
//! and 1 otherwise (or at `--deadline`).
//!
//! ```text
//! cargo run -p sapflow-whip --features testing --example whip_harness -- \
//!     --http 127.0.0.1:47201 --udp 127.0.0.1:47202 [--advertise <ip:port>] \
//!     [--deadline <seconds>] [--record <dir>]
//! ```
//!
//! The session, its credentials and our certificate are seeded (fixed test
//! randomness), so a `--record`ed run replays offline byte for byte: the
//! record keeps the offer without candidates or addresses, every DTLS
//! datagram in and out in order with the timer firings between them, the
//! peer's leaf certificate, and the first SRTP and SRTCP packets. Bind to
//! loopback only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sapflow_dtls::message;
use sapflow_dtls::record;
use sapflow_whip::ice::{Kind, classify};
use sapflow_whip::resource::{PublicUrl, SessionId};
use sapflow_whip::sdp::{self, AnswerConfig, Candidate, Offer};
use sapflow_whip::testing::{HARNESS_SEED, hex, server_identity};
use sapflow_whip::{Credentials, Event, Session, SessionConfig, State, Transport, Tuple};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The session seed, shared with the replay test.
const SEED: [u8; 32] = HARNESS_SEED;

const PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>WHIP harness</title>
<script>
// Publishes fake camera and microphone (RFC 9725), then reports transport
// state and sender counters, never candidates or addresses.
const post = (p, o) => fetch(p, { method: 'POST', body: JSON.stringify(o) });
(async () => {
  const out = { errors: [], ua: navigator.userAgent };
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: { width: 640, height: 360 } });
    const pc = new RTCPeerConnection();
    stream.getTracks().forEach((t) => pc.addTransceiver(t, { direction: 'sendonly', streams: [stream] }));
    await pc.setLocalDescription(await pc.createOffer());
    await Promise.race([new Promise((r) => { pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r(); }), new Promise((r) => setTimeout(r, 1500))]);
    const res = await fetch('/whip/live/harness', { method: 'POST', headers: { 'Content-Type': 'application/sdp', Authorization: 'Bearer harness' }, body: pc.localDescription.sdp });
    out.status = res.status;
    out.location = !!res.headers.get('Location');
    await pc.setRemoteDescription({ type: 'answer', sdp: await res.text() });
    for (let i = 1; i <= 10; i++) {
      await new Promise((r) => setTimeout(r, 1000));
      const st = [...(await pc.getStats()).values()];
      const tr = st.find((s) => s.type === 'transport') || {};
      const v = st.find((s) => s.type === 'outbound-rtp' && s.kind === 'video') || {};
      const a = st.find((s) => s.type === 'outbound-rtp' && s.kind === 'audio') || {};
      const rv = st.find((s) => s.type === 'remote-inbound-rtp' && s.kind === 'video') || {};
      await post('/result', { t: i, ice: pc.iceConnectionState, dtls: tr.dtlsState, role: tr.dtlsRole, tls: tr.tlsVersion,
        cipher: tr.dtlsCipher, srtp: tr.srtpCipher, video_bytes: v.bytesSent, frames: v.framesEncoded, keyframes: v.keyFramesEncoded,
        pli: v.pliCount, fir: v.firCount, nack: v.nackCount, target: v.targetBitrate, audio_bytes: a.bytesSent,
        rr_rtt: rv.roundTripTime, rr_lost: rv.packetsLost });
    }
  } catch (e) { out.errors.push(String(e && e.stack || e)); }
  await post('/done', out);
})();
</script>
"#;

/// Milestones, in the order the brief lists them.
const MILESTONES: [&str; 6] = [
    "ICE nominated",
    "DTLS connected",
    "first SRTP packet decrypted",
    "first H.264 keyframe access unit",
    "Opus packets received",
    "feedback sent",
];

enum FromHttp {
    Session(Box<Session>, String),
    Result(String),
    Done(String),
}

struct Args {
    http: SocketAddr,
    udp: SocketAddr,
    advertise: SocketAddr,
    deadline: Duration,
    record: Option<PathBuf>,
}

fn args() -> Args {
    let mut a = std::env::args().skip(1);
    let mut http: SocketAddr = "127.0.0.1:47201".parse().unwrap();
    let mut udp: SocketAddr = "127.0.0.1:47202".parse().unwrap();
    let mut advertise = None;
    let mut deadline = Duration::from_secs(40);
    let mut record = None;
    while let Some(x) = a.next() {
        match x.as_str() {
            "--http" => http = a.next().unwrap().parse().unwrap(),
            "--udp" => udp = a.next().unwrap().parse().unwrap(),
            "--advertise" => advertise = Some(a.next().unwrap().parse().unwrap()),
            "--deadline" => deadline = Duration::from_secs(a.next().unwrap().parse().unwrap()),
            "--record" => record = Some(PathBuf::from(a.next().unwrap())),
            other => panic!("unknown argument {other}"),
        }
    }
    Args {
        http,
        udp,
        advertise: advertise.unwrap_or(udp),
        deadline,
        record,
    }
}

/// The offer as kept: no candidates, and documentation addresses on the
/// lines that carry one.
fn scrub_offer(offer: &str) -> String {
    let mut out = String::new();
    for line in offer.lines() {
        if line.starts_with("a=candidate:") || line.starts_with("a=end-of-candidates") {
            continue;
        }
        let kept = if line.starts_with("c=IN IP4 ") {
            "c=IN IP4 0.0.0.0".to_owned()
        } else if line.starts_with("c=IN IP6 ") {
            "c=IN IP6 ::".to_owned()
        } else if line.starts_with("a=rtcp:") {
            "a=rtcp:9 IN IP4 0.0.0.0".to_owned()
        } else if let Some(rest) = line.strip_prefix("o=") {
            let f: Vec<&str> = rest.split(' ').collect();
            match f.as_slice() {
                [user, id, ver, ..] => format!("o={user} {id} {ver} IN IP4 203.0.113.1"),
                _ => line.to_owned(),
            }
        } else {
            line.to_owned()
        };
        out.push_str(&kept);
        out.push_str("\r\n");
    }
    out
}

fn http_thread(listener: TcpListener, udp_advertise: SocketAddr, tx: mpsc::Sender<FromHttp>) {
    let base = PublicUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Err(e) = serve(stream, &base, udp_advertise, &tx) {
            eprintln!("http: {e}");
        }
    }
}

fn serve(
    mut stream: TcpStream,
    base: &PublicUrl,
    advertise: SocketAddr,
    tx: &mpsc::Sender<FromHttp>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut r = BufReader::new(stream.try_clone()?);
    let mut start = String::new();
    r.read_line(&mut start)?;
    let mut len = 0usize;
    let mut content_type = String::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "content-type" => content_type = v.trim().to_owned(),
                _ => {}
            }
        }
    }
    let mut body = vec![0; len.min(64 * 1024)];
    r.read_exact(&mut body)?;
    let mut parts = start.split(' ');
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let reply = |s: &mut TcpStream, status: &str, headers: &str, body: &[u8]| {
        let _ = write!(
            s,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        );
        let _ = s.write_all(body);
    };
    match (method, path) {
        ("GET", "/") => reply(
            &mut stream,
            "200 OK",
            "Content-Type: text/html\r\n",
            PAGE.as_bytes(),
        ),
        ("POST", p) if sdp_path(p) => {
            if !sapflow_whip::resource::is_sdp_content_type(&content_type) {
                reply(&mut stream, "415 Unsupported Media Type", "", b"");
                return Ok(());
            }
            match answer(&body, advertise) {
                Ok((answer, session, kept)) => {
                    let id = SessionId::generate_seeded(SEED).unwrap();
                    let headers = format!(
                        "Content-Type: application/sdp\r\nLocation: {}\r\n",
                        base.location(&id)
                    );
                    reply(&mut stream, "201 Created", &headers, answer.as_bytes());
                    let _ = tx.send(FromHttp::Session(Box::new(session), kept));
                }
                Err(r) => {
                    let status = format!("{} Refused", r.status());
                    eprintln!("refused: {}", r.reason());
                    reply(
                        &mut stream,
                        &status,
                        "Content-Type: text/plain\r\n",
                        r.reason().as_bytes(),
                    );
                }
            }
        }
        ("POST", "/result") => {
            reply(&mut stream, "204 No Content", "", b"");
            let _ = tx.send(FromHttp::Result(
                String::from_utf8_lossy(&body).into_owned(),
            ));
        }
        ("POST", "/done") => {
            reply(&mut stream, "204 No Content", "", b"");
            let _ = tx.send(FromHttp::Done(String::from_utf8_lossy(&body).into_owned()));
        }
        _ => reply(&mut stream, "404 Not Found", "", b""),
    }
    Ok(())
}

fn sdp_path(p: &str) -> bool {
    sapflow_whip::resource::stream_path(p).is_some()
}

fn answer(
    body: &[u8],
    advertise: SocketAddr,
) -> Result<(String, Session, String), sapflow_whip::resource::Refusal> {
    let offer = Offer::parse(body).map_err(sdp::Refusal::from)?;
    let creds = Credentials::generate_seeded(SEED).unwrap();
    let identity = server_identity();
    let candidates = [Candidate {
        transport: Transport::Udp,
        address: advertise,
    }];
    let (text, negotiated) = sdp::answer(
        &offer,
        &AnswerConfig {
            credentials: &creds,
            fingerprint: identity.fingerprint(),
            candidates: &candidates,
            opus_max_bitrate: 128_000,
            origin: 1,
        },
    )
    .map_err(sdp::Refusal::from)?;
    let session = Session::new_seeded(
        SessionConfig::default(),
        identity,
        creds,
        negotiated,
        Instant::now(),
        SEED,
    )
    .expect("session");
    let kept = scrub_offer(&String::from_utf8_lossy(body));
    Ok((text, session, kept))
}

/// What the run keeps for the offline replay.
#[derive(Default)]
struct Record {
    /// `C`, `S` and `T` lines: DTLS in, DTLS out, timer firings.
    dtls: String,
    srtp: Vec<Vec<u8>>,
    srtcp: Vec<Vec<u8>>,
    /// The peer's Certificate message, by fragment offset.
    certificate: Vec<u8>,
    certificate_len: Option<usize>,
}

impl Record {
    fn client_dtls(&mut self, d: &[u8]) {
        let _ = writeln!(self.dtls, "C {}", hex(d));
        for rec in record::records(d) {
            if rec.epoch != 0 || rec.content_type != record::HANDSHAKE {
                continue;
            }
            for f in message::fragments(rec.body) {
                if f.msg_type != message::CERTIFICATE {
                    continue;
                }
                let total = f.length as usize;
                let buf = &mut self.certificate;
                if buf.len() < total {
                    buf.resize(total, 0);
                }
                let at = f.offset as usize;
                if let Some(dst) = buf.get_mut(at..at + f.data.len()) {
                    dst.copy_from_slice(f.data);
                }
                self.certificate_len = Some(total);
            }
        }
    }
}

fn main() {
    let a = args();
    let listener = TcpListener::bind(a.http).expect("http bind");
    let udp = UdpSocket::bind(a.udp).expect("udp bind");
    println!("harness: http and udp bound on loopback");
    let (tx, rx) = mpsc::channel();
    let advertise = a.advertise;
    std::thread::spawn(move || http_thread(listener, advertise, tx));

    let start = Instant::now();
    let mut session: Option<Session> = None;
    let mut posted: Option<Instant> = None;
    let mut reached: Vec<(&str, u128, String)> = Vec::new();
    let mut rec = Record::default();
    let mut kept_offer = String::new();
    let mut counts = (0u64, 0u64, 0u64, 0u64); // access units, keyframes, audio, lost
    // RTP datagrams by payload type, read from the clear header.
    let mut by_pt: std::collections::BTreeMap<u8, u64> = std::collections::BTreeMap::new();
    let mut done = None;
    let mut buf = [0u8; 2048];
    let reach = |reached: &mut Vec<(&'static str, u128, String)>,
                 what: &'static str,
                 posted: Option<Instant>,
                 note: String| {
        if !reached.iter().any(|(w, ..)| *w == what) {
            let ms = posted.map_or(0, |p| p.elapsed().as_millis());
            println!("milestone: {what} at +{ms} ms {note}");
            reached.push((what, ms, note));
        }
    };
    while start.elapsed() < a.deadline {
        while let Ok(m) = rx.try_recv() {
            match m {
                FromHttp::Session(s, kept) => {
                    println!("offer answered: 201");
                    session = Some(*s);
                    posted = Some(Instant::now());
                    kept_offer = kept;
                }
                FromHttp::Result(r) => println!("page: {r}"),
                FromHttp::Done(d) => {
                    println!("page done: {d}");
                    done = Some(d);
                }
            }
        }
        if done.is_some() {
            break;
        }
        let wait = session
            .as_ref()
            .and_then(Session::poll_timeout)
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(50))
            .clamp(Duration::from_millis(1), Duration::from_millis(50));
        udp.set_read_timeout(Some(wait)).unwrap();
        let got = udp.recv_from(&mut buf);
        let Some(s) = session.as_mut() else { continue };
        let now = Instant::now();
        match got {
            Ok((n, from)) => {
                let d = &buf[..n];
                let tuple = Tuple::udp(from);
                let selected = s.selected() == Some(tuple);
                let kind = classify(d);
                let dropped = s.stats().dtls_dropped;
                if kind == Kind::Rtp
                    && let Some(b) = d.get(1)
                {
                    *by_pt.entry(b & 0x7f).or_default() += 1;
                }
                if selected && kind == Kind::Rtp && rec.srtp.len() < 8 {
                    rec.srtp.push(d.to_vec());
                }
                if selected && kind == Kind::Rtcp && rec.srtcp.len() < 2 {
                    rec.srtcp.push(d.to_vec());
                }
                s.handle_datagram(now, tuple, d);
                // DTLS the session fed or held (held datagrams are fed at
                // nomination, in arrival order), as DTLS saw it.
                if kind == Kind::Dtls && s.stats().dtls_dropped == dropped {
                    rec.client_dtls(d);
                }
            }
            Err(_) => {
                if s.poll_timeout().is_some_and(|t| t <= now) {
                    let before = s.stats().dtls.retransmissions;
                    s.handle_timeout(now);
                    if s.stats().dtls.retransmissions > before {
                        let _ = writeln!(rec.dtls, "T");
                    }
                }
            }
        }
        let st = s.stats();
        while let Some(t) = s.poll_transmit() {
            if classify(&t.bytes) == Kind::Dtls {
                let _ = writeln!(rec.dtls, "S {}", hex(&t.bytes));
            }
            let _ = udp.send_to(&t.bytes, t.tuple.remote);
        }
        while let Some(e) = s.poll_event() {
            match e {
                Event::Nominated => reach(&mut reached, MILESTONES[0], posted, String::new()),
                Event::Connected(p) => reach(
                    &mut reached,
                    MILESTONES[1],
                    posted,
                    format!("profile={p:?}"),
                ),
                Event::Media(sapflow_rtp::Event::AccessUnit { au, .. }) => {
                    counts.0 += 1;
                    if au.idr {
                        counts.1 += 1;
                        let note = format!(
                            "bytes={} sps={} pps={}",
                            au.data.len(),
                            au.sps.len(),
                            au.pps.len()
                        );
                        reach(&mut reached, MILESTONES[3], posted, note);
                    }
                }
                Event::Media(sapflow_rtp::Event::Audio { frame, .. }) => {
                    counts.2 += 1;
                    if frame.kind == sapflow_rtp::opus::FrameKind::Media {
                        let note = format!("stereo={} duration={}", frame.stereo, frame.duration);
                        reach(&mut reached, MILESTONES[4], posted, note);
                    }
                }
                Event::Media(sapflow_rtp::Event::Lost { count, .. }) => counts.3 += count,
                Event::Media(sapflow_rtp::Event::Aligned { alignment }) => {
                    println!("aligned: {alignment:?}");
                }
                Event::Media(_) => {}
                Event::Ended(r) => println!("session ended: {r:?}"),
            }
        }
        if st.srtp_packets > 0 {
            reach(&mut reached, MILESTONES[2], posted, String::new());
        }
        if st.feedback_sent > 0 {
            reach(
                &mut reached,
                MILESTONES[5],
                posted,
                format!("datagrams={}", st.feedback_sent),
            );
        }
    }
    let Some(s) = session.as_ref() else {
        println!("FAIL: no offer arrived");
        std::process::exit(1);
    };
    let st = s.stats();
    println!(
        "summary: state={:?} profile={:?} access_units={} keyframes={} audio_frames={} lost={} \
         srtp={} srtp_refused={} srtcp={} feedback={} unmatched={} refused={} dtls_held={} \
         dtls_dropped={} ice_successes={} nominations={} role_conflicts={} dtls_retransmissions={}",
        s.state(),
        s.profile(),
        counts.0,
        counts.1,
        counts.2,
        counts.3,
        st.srtp_packets,
        st.srtp_refused,
        st.srtcp_packets,
        st.feedback_sent,
        st.unmatched,
        st.refused,
        st.dtls_held,
        st.dtls_dropped,
        st.ice.successes,
        st.ice.nominations,
        st.ice.role_conflicts,
        st.dtls.retransmissions,
    );
    if let Some(rx) = s.receiver() {
        for kind in [sdp::MediaKind::Video, sdp::MediaKind::Audio] {
            if let Some(x) = s.stream(kind).and_then(|id| rx.stats(id)) {
                println!(
                    "stream {kind:?}: packets={} recovered={} duplicates={} late={} lost={} \
                     nack_packets={} nacked={} keyframe_requests={} frames={} incomplete={} \
                     skipped={} audio={} dtx={} loss_frames={} refused={} discontinuities={}",
                    x.packets,
                    x.recovered,
                    x.duplicates,
                    x.late,
                    x.lost,
                    x.nack_packets,
                    x.nacked,
                    x.keyframe_requests,
                    x.frames,
                    x.incomplete,
                    x.skipped,
                    x.audio_frames,
                    x.dtx_frames,
                    x.loss_frames,
                    x.refused,
                    x.discontinuities
                );
            }
        }
    }
    println!("rtp by payload type: {by_pt:?}");
    println!(
        "media before keys: {}, from unvalidated tuples: {}",
        st.media_before_keys, st.media_unvalidated
    );
    if let Some(dir) = &a.record {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("offer.sdp"), &kept_offer).unwrap();
        std::fs::write(dir.join("dtls.txt"), &rec.dtls).unwrap();
        let mut media = String::new();
        for p in &rec.srtp {
            let _ = writeln!(media, "srtp {}", hex(p));
        }
        for p in &rec.srtcp {
            let _ = writeln!(media, "srtcp {}", hex(p));
        }
        std::fs::write(dir.join("media.txt"), media).unwrap();
        match rec.certificate_len.map(|n| rec.certificate.get(..n)) {
            Some(Some(body)) => match message::certificate_leaf(body, 4) {
                Ok(leaf) => std::fs::write(dir.join("leaf.der.hex"), hex(leaf) + "\n").unwrap(),
                Err(e) => println!("no leaf: {e:?}"),
            },
            _ => println!("no certificate recorded"),
        }
        let profile = s.profile().map(|p| format!("{p:?}")).unwrap_or_default();
        let lines: String = reached
            .iter()
            .map(|(w, ms, note)| format!("{w}: +{ms} ms {note}\n"))
            .collect();
        std::fs::write(
            dir.join("summary.txt"),
            format!("profile {profile}\n{lines}"),
        )
        .unwrap();
        println!("recorded");
    }
    let missing: Vec<&str> = MILESTONES
        .iter()
        .copied()
        .filter(|m| !reached.iter().any(|(w, ..)| w == m))
        .collect();
    let connected = matches!(s.state(), State::Connected);
    if missing.is_empty() && connected && done.is_some() {
        println!("PASS: every milestone");
    } else {
        println!(
            "FAIL: missing {missing:?}, state {:?}, page done {}",
            s.state(),
            done.is_some()
        );
        std::process::exit(1);
    }
}
