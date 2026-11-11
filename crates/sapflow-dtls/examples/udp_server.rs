//! Test only: the sans-IO server on a UDP socket, for black-box runs
//! against `openssl s_client -dtls1_2` (acceptance A2.1). Serves one peer,
//! prints the negotiated profile and the exported keying material, answers
//! `close_notify` and exits.
//!
//! ```text
//! cargo run -p sapflow-dtls --features testing --example udp_server -- \
//!     --bind 127.0.0.1:47010 --expect <client sha-256 fingerprint> \
//!     [--seed <64 hex>] [--mtu <bytes>] [--hello-verify] \
//!     [--drop-first-flight] [--drop-final-flight] [--record <file>]
//! ```
//!
//! `--seed` makes the identity and the server's randomness reproducible, so
//! a `--record`ed run replays offline byte for byte. `--drop-first-flight`
//! discards our first flight once, to exercise both sides' retransmission;
//! `--drop-final-flight` discards our ChangeCipherSpec and Finished once, so
//! the client must retransmit its flight and we must answer it.
//! Bind to loopback only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sapflow_dtls::{Config, Event, Fingerprint, Identity, Server, State};
use std::io::Write;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut bind = String::from("127.0.0.1:47010");
    let mut expect = None;
    let mut seed = None;
    let mut config = Config::default();
    let mut drop_first = false;
    let mut drop_final = false;
    let mut record = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bind" => bind = args.next().expect("address"),
            "--expect" => {
                expect = Some(Fingerprint::parse_sdp(&args.next().expect("fp")).expect("fp"))
            }
            "--seed" => {
                let s: [u8; 32] = unhex(&args.next().expect("seed")).try_into().expect("32");
                seed = Some(s);
            }
            "--mtu" => config.mtu = args.next().expect("mtu").parse().expect("mtu"),
            "--hello-verify" => config.hello_verify = true,
            "--drop-first-flight" => drop_first = true,
            "--drop-final-flight" => drop_final = true,
            "--record" => record = Some(std::fs::File::create(args.next().expect("file")).unwrap()),
            other => panic!("unknown argument {other}"),
        }
    }
    let expect = expect.expect("--expect is required");
    assert!(
        bind.starts_with("127.0.0.1:") || bind.starts_with("[::1]:"),
        "loopback only"
    );
    let now_unix = Duration::from_secs(1_793_000_000);
    let identity = Arc::new(match seed {
        Some(s) => Identity::generate_seeded(s, now_unix).unwrap(),
        None => Identity::generate(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        )
        .unwrap(),
    });
    let mut server = match seed {
        Some(s) => Server::new_seeded(config, identity.clone(), &[expect], s).unwrap(),
        None => Server::new(config, identity.clone(), &[expect]).unwrap(),
    };
    println!("server fingerprint: sha-256 {}", identity.fingerprint());
    let socket = UdpSocket::bind(&bind).unwrap();
    println!("listening on {bind}");
    std::io::stdout().flush().unwrap();
    let start = Instant::now();
    let mut log = |dir: &str, bytes: &[u8]| {
        if let Some(f) = record.as_mut() {
            let ms = start.elapsed().as_millis();
            writeln!(f, "{dir} {ms} {}", hex(bytes)).unwrap();
        }
    };
    let mut peer = None;
    let mut buf = [0u8; 65536];
    let mut connected_at = None;
    loop {
        let wait = server
            .poll_timeout()
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(200))
            .max(Duration::from_millis(1));
        socket.set_read_timeout(Some(wait)).unwrap();
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                if *peer.get_or_insert(from) != from {
                    continue;
                }
                log("C", &buf[..n]);
                if let Err(e) = server.handle_datagram(Instant::now(), &buf[..n]) {
                    println!("handshake error: {e:?}");
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if server.poll_timeout().is_some_and(|t| t <= Instant::now()) {
                    log("T", &[]);
                    if let Err(e) = server.handle_timeout(Instant::now()) {
                        println!("handshake error: {e:?}");
                    }
                }
            }
            Err(e) => panic!("recv: {e}"),
        }
        // Our final flight goes out in the same call that connects.
        if drop_final && server.state() == State::Connected {
            println!("dropping our final flight");
            drop_final = false;
            while let Some(d) = server.poll_transmit() {
                log("D", &d);
            }
        }
        while let Some(d) = server.poll_transmit() {
            if drop_first {
                // Drop the whole first flight: everything queued now.
                println!("dropping our first flight");
                drop_first = false;
                log("D", &d);
                while let Some(rest) = server.poll_transmit() {
                    log("D", &rest);
                }
                break;
            }
            log("S", &d);
            if let Some(p) = peer {
                socket.send_to(&d, p).unwrap();
            }
        }
        while let Some(ev) = server.poll_event() {
            match ev {
                Event::Connected(c) => {
                    println!("profile: {:?} (0x{:04x})", c.profile, c.profile.id());
                    println!("peer fingerprint: sha-256 {}", c.peer_fingerprint);
                    println!(
                        "keying material: {}",
                        hex(c.keying_material.as_bytes()).to_uppercase()
                    );
                    connected_at = Some(Instant::now());
                }
                Event::Closed => println!("closed by peer"),
            }
        }
        std::io::stdout().flush().unwrap();
        match server.state() {
            State::Closed | State::Failed(_) => {
                println!("state: {:?}", server.state());
                println!("stats: {:?}", server.stats());
                break;
            }
            _ => {}
        }
        if connected_at.is_some_and(|t| t.elapsed() > Duration::from_secs(10))
            || start.elapsed() > Duration::from_secs(60)
        {
            println!("giving up: state {:?}", server.state());
            break;
        }
    }
}
