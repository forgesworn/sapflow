//! Plain-RTMP black-box interoperability driver. Production native applications
//! supply TLS themselves; this example deliberately makes no RTMPS claim.
use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use sapflow_rtmp::client::{Publisher, PublisherConfig, PublisherState};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err("usage: rtmp-publish <socket-address> <app> <key> <input.flv>".into());
    }
    let address: SocketAddr = args[1].parse()?;
    let started = Instant::now();
    let mut config = PublisherConfig::new(
        args[2].clone(),
        args[3].clone(),
        format!("rtmp://{address}/{}", args[2]),
    );
    config.max_media_bytes = 4 * 1024 * 1024;
    config.max_output_bytes = 8 * 1024 * 1024;
    let mut publisher = Publisher::new(config, 1, 0)?;
    let mut socket = loop {
        match TcpStream::connect_timeout(&address, Duration::from_millis(250)) {
            Ok(socket) => break socket,
            Err(error)
                if started.elapsed() < Duration::from_secs(3)
                    && matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
                    ) =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.into()),
        }
    };
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(10)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    while publisher.state() != PublisherState::Publishing {
        exchange(&mut publisher, &mut socket, started)?;
    }
    let mut file = std::fs::File::open(&args[4])?;
    let mut header = [0u8; 9];
    file.read_exact(&mut header)?;
    if &header[..3] != b"FLV" || header[3] != 1 || header[5..] != [0, 0, 0, 9] {
        return Err("unsupported FLV header".into());
    }
    let media_started = Instant::now();
    let mut previous_size = [0u8; 4];
    file.read_exact(&mut previous_size)?;
    if previous_size != [0; 4] {
        return Err("invalid initial FLV previous size".into());
    }
    let mut video = 0u64;
    let mut audio = 0u64;
    loop {
        let mut tag = [0u8; 11];
        match file.read(&mut tag[..1])? {
            0 => break,
            1 => file.read_exact(&mut tag[1..])?,
            _ => return Err("invalid file read".into()),
        }
        let size = usize::try_from(u32::from_be_bytes([0, tag[1], tag[2], tag[3]]))?;
        if size > 4 * 1024 * 1024 || tag[8..] != [0, 0, 0] {
            return Err("FLV tag bound/stream id refused".into());
        }
        let timestamp = u32::from_be_bytes([tag[7], tag[4], tag[5], tag[6]]);
        let mut body = vec![0u8; size];
        file.read_exact(&mut body)?;
        file.read_exact(&mut previous_size)?;
        if u32::from_be_bytes(previous_size) != 11 + size as u32 {
            return Err("invalid FLV previous tag size".into());
        }
        while media_started.elapsed() < Duration::from_millis(u64::from(timestamp)) {
            exchange(&mut publisher, &mut socket, started)?;
        }
        match (tag[0], body.as_slice()) {
            (9, [frame, 0, _, _, _, data @ ..]) if frame & 15 == 7 => {
                publisher.send_avc_config_at(timestamp, data)?
            }
            (9, [frame, 1, a, b, c, data @ ..]) if frame & 15 == 7 => {
                let offset = i32::from_be_bytes([if a & 0x80 != 0 { 0xff } else { 0 }, *a, *b, *c]);
                publisher.send_avc(timestamp, offset, frame >> 4 == 1, data)?;
                video += 1;
            }
            (8, [format, 0, data @ ..]) if format >> 4 == 10 => {
                publisher.send_aac_config_at(timestamp, data)?
            }
            (8, [format, 1, data @ ..]) if format >> 4 == 10 => {
                publisher.send_aac(timestamp, data)?;
                audio += 1;
            }
            _ => {}
        }
        exchange(&mut publisher, &mut socket, started)?;
    }
    publisher.finish()?;
    while publisher.queued_output_bytes() != 0 {
        let output = publisher.take_output();
        socket.write_all(&output)?;
        if publisher.queued_output_bytes() != 0 {
            exchange(&mut publisher, &mut socket, started)?;
        }
    }
    socket.shutdown(std::net::Shutdown::Write)?;
    println!(
        "server_accepted=true video_samples={video} audio_samples={audio} peer_window_bytes={} peer_ack_sequence={} outstanding_wire_bytes={} media_delivery_confirmed=false",
        publisher.peer_output_window().unwrap_or(0),
        publisher.peer_acknowledgement(),
        publisher.outstanding_output_bytes()
    );
    Ok(())
}

fn exchange(
    publisher: &mut Publisher,
    socket: &mut TcpStream,
    started: Instant,
) -> Result<(), Box<dyn Error>> {
    socket.write_all(&publisher.take_output())?;
    let mut bytes = [0u8; 64 * 1024];
    let now_ms = started.elapsed().as_millis().try_into()?;
    match socket.read(&mut bytes) {
        Ok(0) => return Err("origin disconnected".into()),
        Ok(count) => publisher.feed(&bytes[..count], now_ms)?,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            publisher.tick(now_ms)?
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
