//! A listener that holds the passphrase itself, as an application with one
//! passphrase per stream does: `Config::app_keys`, then `check_key` and
//! `accept_with_key` for each caller. Callers without the passphrase, or
//! with none, are refused; there is no plaintext fallback.
//!
//! `SRT_APP_PASSPHRASE='10 to 79 bytes' cargo run -p sapflow-srt --example listen-app-key`
//! then, for example, `srt-live-transmit file://con
//! 'srt://127.0.0.1:9000?streamid=demo&passphrase=...&pbkeylen=32'`
//! (`pbkeylen=16` works too). The passphrase is never printed.
use sapflow_srt::crypto::{CryptoConfig, KeySize};
use sapflow_srt::{Config, Event, Listener, RejectCode};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let passphrase = std::env::var("SRT_APP_PASSPHRASE")?.into_bytes();
    // The caller's key length is used; this one is not consulted.
    let key = CryptoConfig::new(passphrase, KeySize::Aes256)?;
    let socket = UdpSocket::bind("0.0.0.0:9000")?;
    let config = Config {
        app_keys: true,
        ..Config::default()
    };
    let mut listener = Listener::new(config, Instant::now());
    let mut buf = [0u8; 2048];
    let mut received: HashMap<u32, (usize, Vec<u8>)> = HashMap::new();

    loop {
        let now = Instant::now();
        let wait = listener
            .poll_timeout()
            .map_or(Duration::from_millis(100), |at| {
                at.saturating_duration_since(now)
            })
            .max(Duration::from_millis(1));
        socket.set_read_timeout(Some(wait))?;
        match socket.recv_from(&mut buf) {
            Ok((n, peer)) => {
                if let Some(datagram) = buf.get(..n) {
                    listener.handle_datagram(Instant::now(), peer, datagram);
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
        listener.handle_timeout(Instant::now());

        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect {
                    id,
                    peer,
                    stream_id,
                } => {
                    // Check the stream id here as well; it is untrusted.
                    let check = listener.check_key(id, &key);
                    println!(
                        "{peer} wants {:?}: key check {check:?}",
                        String::from_utf8_lossy(&stream_id)
                    );
                    match check {
                        Ok(_) => {
                            let accepted = listener.accept_with_key(id, Instant::now(), &key);
                            println!("{id:?} accepted: {accepted}");
                        }
                        Err(e) => {
                            listener.reject(id, e.reject_code().unwrap_or(RejectCode::BADSECRET));
                        }
                    }
                }
                Event::Data { id, payload, .. } => {
                    let (bytes, first) = received.entry(id.0).or_default();
                    if first.is_empty() {
                        first.extend(payload.iter().take(16));
                    }
                    *bytes += payload.len();
                }
                Event::Closed { id, reason, stats } => {
                    let (bytes, first) = received.remove(&id.0).unwrap_or_default();
                    println!(
                        "{id:?} closed: {reason:?}; {bytes} bytes delivered, {} packets \
                         decrypted, {} refused, first bytes {:?}",
                        stats.decrypted,
                        stats.encrypted,
                        String::from_utf8_lossy(&first)
                    );
                }
            }
        }
        while let Some((to, datagram)) = listener.poll_transmit() {
            socket.send_to(&datagram, to)?;
        }
    }
}
