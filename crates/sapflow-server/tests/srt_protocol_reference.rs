//! The reference half of the SRT differential test: replays each recorded
//! publish (`crates/sapflow-srt/tests/fixtures/*.trace`) through
//! `srt-protocol`, the implementation sapflow has used until now, run as a
//! black box through its public API, and checks the payloads it delivers
//! against the committed `*.expected` file. `sapflow-srt`'s own test
//! (`crates/sapflow-srt/tests/differential.rs`) checks our receiver against
//! the same file, so the two deliver the same byte stream.
//!
//! `SAPFLOW_REGENERATE=1 cargo test -p sapflow-server --test
//! srt_protocol_reference` rewrites the expected files from `srt-protocol`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use srt_protocol::connection::{self, DuplexConnection};
use srt_protocol::listener::{AccessControlResponse, Action, Input, MultiplexListener, ResultOf};
use srt_protocol::packet::{ControlPacket, ControlTypes, Packet, ShakeType};
use srt_protocol::settings::ConnInitSettings;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sapflow-srt/tests/fixtures")
}

/// Records: microseconds since the first (u32), length (u16), datagram,
/// all big-endian; each a datagram the caller sent.
fn read_trace(name: &str) -> Vec<(u64, Vec<u8>)> {
    let b = std::fs::read(fixtures().join(format!("{name}.trace"))).expect("trace");
    let mut out = Vec::new();
    let mut rest = b.as_slice();
    while rest.len() >= 6 {
        let t = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let len = usize::from(u16::from_be_bytes([rest[4], rest[5]]));
        out.push((u64::from(t), rest[6..6 + len].to_vec()));
        rest = &rest[6 + len..];
    }
    out
}

/// One line per delivered payload: its length and FNV-1a 64 hash.
fn summarise(payloads: &[Vec<u8>]) -> String {
    payloads
        .iter()
        .map(|p| {
            let h = p.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01B3)
            });
            format!("{} {h:016x}\n", p.len())
        })
        .collect()
}

fn handshake(p: &mut Packet) -> Option<&mut srt_protocol::packet::HandshakeControlInfo> {
    match p {
        Packet::Control(ControlPacket {
            control_type: ControlTypes::Handshake(h),
            ..
        }) => Some(h),
        _ => None,
    }
}

/// What `srt-protocol` delivers for the trace, the caller's datagrams fed at
/// their recorded times. Like a real caller, the conclusion carries the
/// cookie the listener handed out, and later packets its socket id.
fn srt_protocol_delivers(trace: &[(u64, Vec<u8>)]) -> Vec<Vec<u8>> {
    let start = Instant::now();
    let local: SocketAddr = "127.0.0.1:8890".parse().expect("addr");
    let peer: SocketAddr = "127.0.0.1:50000".parse().expect("addr");
    let mut listener = MultiplexListener::new(start, local, ConnInitSettings::default());
    let mut conn: Option<DuplexConnection> = None;
    let mut conn_timer: Option<Instant> = None;
    let mut cookie = None;
    let mut socket_id = None;
    let mut delivered = Vec::new();

    let drive = |conn: &mut DuplexConnection,
                 now: Instant,
                 mut input: connection::Input,
                 timer: &mut Option<Instant>,
                 out: &mut Vec<Vec<u8>>| {
        for _ in 0..100_000 {
            input = match conn.handle_input(now, input) {
                connection::Action::SendPacket(_) => connection::Input::PacketSent,
                connection::Action::ReleaseData((_, data)) => {
                    out.push(data.to_vec());
                    connection::Input::DataReleased
                }
                connection::Action::UpdateStatistics(_) => connection::Input::StatisticsUpdated,
                connection::Action::WaitForData(wait) => {
                    *timer = Some(now + wait);
                    return;
                }
                connection::Action::Close => {
                    *timer = None;
                    return;
                }
            };
        }
    };

    let end = trace.last().map_or(0, |(t, _)| *t) + 5_000_000;
    let mut i = 0;
    loop {
        let next_packet = trace.get(i).map(|(t, _)| start + Duration::from_micros(*t));
        let next = match (next_packet, conn_timer) {
            (Some(p), Some(c)) => p.min(c),
            (Some(p), None) => p,
            (None, Some(c)) => c,
            (None, None) => break,
        };
        if next > start + Duration::from_micros(end) {
            break;
        }
        let now = next;
        if conn_timer.is_some_and(|c| c <= now) && next_packet.is_none_or(|p| p > now) {
            if let Some(c) = &mut conn {
                drive(
                    c,
                    now,
                    connection::Input::Timer,
                    &mut conn_timer,
                    &mut delivered,
                );
            }
            continue;
        }
        let (_, datagram) = &trace[i];
        i += 1;
        let mut buf = datagram.as_slice();
        let Ok(mut p) = Packet::parse(&mut buf, false) else {
            continue;
        };
        if let Some(h) = handshake(&mut p) {
            if h.shake_type == ShakeType::Conclusion
                && let Some(c) = cookie
            {
                h.syn_cookie = c;
            }
        } else if let Some(id) = socket_id {
            p = match p {
                Packet::Data(mut d) => {
                    d.dest_sockid = id;
                    Packet::Data(d)
                }
                Packet::Control(mut c) => {
                    c.dest_sockid = id;
                    Packet::Control(c)
                }
            };
        }
        let mut input = Input::Packet(Ok((p, peer)));
        for _ in 0..1000 {
            input = match listener.handle_input(now, input) {
                Action::SendPacket((mut p, a)) => {
                    if let Some(h) = handshake(&mut p) {
                        match h.shake_type {
                            ShakeType::Induction => cookie = Some(h.syn_cookie),
                            ShakeType::Conclusion => socket_id = Some(h.socket_id),
                            _ => {}
                        }
                    }
                    Input::Success(ResultOf::SendPacket(a))
                }
                Action::RequestAccess(id, _) => {
                    Input::AccessResponse(Some((id, AccessControlResponse::Accepted(None))))
                }
                Action::RejectConnection(id, _) => Input::Success(ResultOf::RejectConnection(id)),
                Action::OpenConnection(id, b) => {
                    let (response, c) = *b;
                    if let Some((mut p, _)) = response
                        && let Some(h) = handshake(&mut p)
                    {
                        socket_id = Some(h.socket_id);
                    }
                    conn = Some(DuplexConnection::new(c));
                    conn_timer = Some(now);
                    Input::Success(ResultOf::OpenConnection(id))
                }
                Action::DelegatePacket(id, packet) => {
                    if let Some(c) = &mut conn {
                        drive(
                            c,
                            now,
                            connection::Input::Packet(Ok(packet)),
                            &mut conn_timer,
                            &mut delivered,
                        );
                    }
                    Input::Success(ResultOf::DelegatePacket(id))
                }
                Action::DropConnection(id) => Input::Success(ResultOf::DropConnection(id)),
                Action::UpdateStatistics(_) => Input::Success(ResultOf::UpdateStatistics),
                Action::WaitForInput | Action::Close => break,
            };
        }
    }
    delivered
}

fn check(name: &str) {
    let trace = read_trace(name);
    let delivered = srt_protocol_delivers(&trace);
    assert!(!delivered.is_empty(), "{name}: nothing delivered");
    let summary = summarise(&delivered);
    let path = fixtures().join(format!("{name}.expected"));
    if std::env::var_os("SAPFLOW_REGENERATE").is_some() {
        std::fs::write(&path, &summary).expect("write expected");
    }
    let expected = std::fs::read_to_string(&path).expect("expected file");
    assert_eq!(summary, expected, "{name}: srt-protocol's delivery changed");
}

#[test]
fn srt_live_transmit_publish() {
    check("srt-live-transmit-publish");
}

#[test]
fn ffmpeg_publish() {
    check("ffmpeg-publish");
}

#[test]
fn srt_live_transmit_lossy_200ms() {
    check("srt-live-transmit-lossy-200ms");
}
