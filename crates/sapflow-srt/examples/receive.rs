//! Lab receiver for independent encrypted interoperability, not a server.
//! receive <bind ip:port> <output file> <expected bytes> [latency ms]
//! Optional SAPFLOW_SRT_PASSPHRASE and SAPFLOW_SRT_KEY_BITS=128|256.
use sapflow_srt::crypto::{CryptoConfig, KeySize};
use sapflow_srt::{Config, Event, Listener};
use std::{
    error::Error,
    fs::File,
    io::{ErrorKind, Write},
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (Some(bind), Some(path), Some(expected)) = (args.first(), args.get(1), args.get(2)) else {
        return Err("receive <bind> <output> <expected bytes> [latency ms]".into());
    };
    let bind: SocketAddr = bind.parse()?;
    let expected: usize = expected.parse()?;
    let mut config = Config::default();
    if let Some(ms) = args.get(3) {
        config.latency = Duration::from_millis(ms.parse()?);
    }
    if let Ok(secret) = std::env::var("SAPFLOW_SRT_PASSPHRASE") {
        let size = match std::env::var("SAPFLOW_SRT_KEY_BITS").as_deref() {
            Ok("256") => KeySize::Aes256,
            Ok("128") | Err(_) => KeySize::Aes128,
            _ => return Err("key bits must be 128 or 256".into()),
        };
        config.crypto = Some(CryptoConfig::new(secret.into_bytes(), size)?);
        config.require_encryption = true;
    }
    let socket = UdpSocket::bind(bind)?;
    socket.set_read_timeout(Some(Duration::from_millis(5)))?;
    let mut output = File::create(path)?;
    let start = Instant::now();
    let mut listener = Listener::new(config, start);
    let mut packet = [0; 2048];
    let mut bytes = 0;
    let mut connection = None;
    println!("listening {}", socket.local_addr()?);
    std::io::stdout().flush()?;
    while start.elapsed() < Duration::from_secs(20) {
        let now = Instant::now();
        listener.handle_timeout(now);
        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect { id, stream_id, .. } => {
                    println!("stream_id={}", String::from_utf8_lossy(&stream_id));
                    if listener.accept(id, now) {
                        connection = Some(id);
                    }
                }
                Event::Data { payload, .. } => {
                    output.write_all(&payload)?;
                    bytes += payload.len();
                }
                Event::Closed { .. } => {}
            }
        }
        if bytes >= expected {
            let stats = connection.and_then(|id| listener.connection_stats(id));
            println!("received_bytes={bytes} stats={stats:?}");
            if let Some(id) = connection {
                listener.close(id, now);
            }
            while let Some((peer, b)) = listener.poll_transmit() {
                socket.send_to(&b, peer)?;
            }
            return Ok(());
        }
        while let Some((peer, b)) = listener.poll_transmit() {
            socket.send_to(&b, peer)?;
        }
        match socket.recv_from(&mut packet) {
            Ok((n, peer)) => {
                listener.handle_datagram(Instant::now(), peer, packet.get(..n).unwrap_or_default())
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(format!(
        "receiver timeout bytes={bytes} rejected={}",
        listener.stats().rejected
    )
    .into())
}
