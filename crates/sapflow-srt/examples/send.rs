//! Lab SRT file sender using our own sans-IO caller.
//! Run: cargo run -p sapflow-srt --example send -- 127.0.0.1:9000 publish:lab input.ts [bytes/sec] [latency-ms]
//! Reads bounded chunks; this measures transport interoperability, not live
//! capture timing or physical-device performance. Stream IDs travel in clear.
use std::error::Error;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use sapflow_srt::crypto::{CryptoConfig, KeySize};
use sapflow_srt::{Caller, CallerConfig, CallerEvent};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (Some(destination), Some(stream_id), Some(path)) = (args.first(), args.get(1), args.get(2))
    else {
        return Err("usage: send <ip:port> <stream-id> <input.ts> [SRT-bytes/sec] [latency-ms]; LAB ONLY; optional SAPFLOW_SRT_PASSPHRASE env".into());
    };
    let destination: SocketAddr = destination.parse()?;
    let mut config = CallerConfig {
        stream_id: stream_id.clone(),
        ..CallerConfig::default()
    };
    if let Some(bandwidth) = args.get(3) {
        config.send.bandwidth = bandwidth.parse()?;
    }
    if let Some(latency) = args.get(4) {
        config.latency = Duration::from_millis(latency.parse()?);
    }
    if let Ok(secret) = std::env::var("SAPFLOW_SRT_PASSPHRASE") {
        let size = match std::env::var("SAPFLOW_SRT_KEY_BITS").as_deref() {
            Ok("256") => KeySize::Aes256,
            Ok("128") | Err(_) => KeySize::Aes128,
            _ => return Err("key bits must be 128 or 256".into()),
        };
        let mut crypto = CryptoConfig::new(secret.into_bytes(), size)?;
        if let Ok(refresh) = std::env::var("SAPFLOW_SRT_REFRESH") {
            crypto.refresh_packets = refresh.parse()?;
            crypto.preannounce_packets = crypto.refresh_packets / 4;
        }
        config.crypto = Some(crypto);
    }
    let encrypted = config.crypto.is_some();
    let mut latency = config.latency;
    let socket = UdpSocket::bind(if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })?;
    let mut caller = Caller::new(config, destination, Instant::now())?;
    let mut file = File::open(path)?;
    let mut input = [0u8; 1316];
    let mut packet = [0u8; 2048];
    let mut eof = false;
    let mut drained_at = None;
    let deadline = Instant::now() + Duration::from_secs(120);
    eprintln!("LAB ONLY: encrypted={encrypted}; stream ID is visible on the network");
    loop {
        let now = Instant::now();
        if now >= deadline {
            caller.stop(now);
            return Err("lab sender exceeded 120-second run budget".into());
        }
        caller.handle_timeout(now);
        while let Some(event) = caller.poll_event() {
            match event {
                CallerEvent::Connected {
                    latency: negotiated_latency,
                    max_payload,
                    ..
                } => {
                    latency = negotiated_latency;
                    eprintln!(
                        "connected latency_ms={} payload_limit={max_payload}",
                        latency.as_millis()
                    );
                }
                CallerEvent::Closed { reason } => {
                    return Err(format!("SRT session closed: {reason:?}").into());
                }
            }
        }
        // Eight payloads at most are read ahead. Retained retransmissions
        // remain accounted for, so slow receivers cannot drive file growth.
        while caller.is_connected() && !eof && caller.stats().send.buffered_packets < 8 {
            let len = file.read(&mut input)?;
            if len == 0 {
                eof = true;
            } else {
                caller.enqueue(now, input.get(..len).unwrap_or_default().to_vec())?;
            }
        }
        while let Some((peer, bytes)) = caller.poll_transmit(Instant::now()) {
            socket.send_to(&bytes, peer)?;
        }
        if eof && caller.stats().send.buffered_packets == 0 {
            let drained = *drained_at.get_or_insert(now);
            if now.saturating_duration_since(drained) >= latency + Duration::from_millis(250) {
                caller.stop(now);
                if let Some((peer, bytes)) = caller.poll_transmit(now) {
                    socket.send_to(&bytes, peer)?;
                }
                let stats = caller.stats();
                println!(
                    "acknowledged_bytes={} acknowledged_packets={} retransmitted_packets={} rtt_us={} peak_buffered_bytes={}",
                    stats.send.acknowledged_bytes,
                    stats.send.acknowledged_packets,
                    stats.send.retransmitted_packets,
                    stats.send.rtt_us,
                    stats.send.peak_buffered_bytes
                );
                return Ok(());
            }
        }
        let wait = caller
            .poll_timeout()
            .map(|at| at.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(10))
            .clamp(Duration::from_micros(100), Duration::from_millis(10));
        socket.set_read_timeout(Some(wait))?;
        match socket.recv_from(&mut packet) {
            Ok((len, peer)) => {
                caller.handle_datagram(Instant::now(), peer, packet.get(..len).unwrap_or_default())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                caller.stop(Instant::now());
                return Err(error.into());
            }
        }
    }
}
