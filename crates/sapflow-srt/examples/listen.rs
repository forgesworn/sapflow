//! The smallest SRT listener: accept every caller and print each payload's
//! size. `cargo run -p sapflow-srt --example listen` then, for example,
//! `ffmpeg -re -f lavfi -i testsrc2 -c:v libx264 -f mpegts 'srt://127.0.0.1:9000'`
//! (an ffmpeg built with libsrt), or pipe MPEG-TS into libsrt's
//! `srt-live-transmit file://con 'srt://127.0.0.1:9000?streamid=demo'`.
//!
//! The listener is sans-IO: this loop owns the socket and the clock, feeds
//! it datagrams and timeouts, and sends what it asks to send.
use sapflow_srt::{Config, Event, Listener};
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

fn main() -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:9000")?;
    let mut listener = Listener::new(Config::default(), Instant::now());
    // An SRT datagram is at most 1,500 bytes on an ordinary network.
    let mut buf = [0u8; 2048];

    loop {
        // Wait for a datagram or until the listener next needs the time.
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
            Err(e) => return Err(e),
        }
        listener.handle_timeout(Instant::now());

        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect {
                    id,
                    peer,
                    stream_id,
                } => {
                    // The stream id is untrusted input: check it here, and
                    // call `listener.reject(id, code)` to refuse a caller.
                    println!("{peer} wants {:?}", String::from_utf8_lossy(&stream_id));
                    listener.accept(id, Instant::now());
                }
                Event::Data { id, payload, .. } => println!("{id:?}: {} bytes", payload.len()),
                Event::Closed { id, reason, .. } => println!("{id:?} closed: {reason:?}"),
            }
        }
        while let Some((to, datagram)) = listener.poll_transmit() {
            socket.send_to(&datagram, to)?;
        }
    }
}
