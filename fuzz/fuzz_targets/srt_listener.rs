//! srt-protocol's listener state machine (what srt-tokio's listener task
//! runs): datagrams from a few peers, access-control answers and timer ticks,
//! with every packet it sends re-encoded. Like a real client, each peer echoes
//! the SYN cookie it was sent, so handshakes can complete; an opened
//! connection then gets that peer's later datagrams (data, ACK, NAK, ...), as
//! srt-tokio delegates them, which covers SRT after the handshake.
#![no_main]
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use srt_protocol::connection::{self, DuplexConnection};
use srt_protocol::listener::{
    AccessControlResponse, Action, Input, MultiplexListener, ResultOf,
};
use srt_protocol::packet::{
    ControlPacket, ControlTypes, Packet, ReceivePacketError, RejectReason, ServerRejectReason,
    ShakeType,
};
use srt_protocol::settings::ConnInitSettings;

fn encode(p: &Packet) {
    let mut out = BytesMut::new();
    p.serialize(&mut out);
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

/// Runs an open connection on one input until it waits.
fn drive(conn: &mut DuplexConnection, now: Instant, mut input: connection::Input) {
    for _ in 0..256 {
        input = match conn.handle_input(now, input) {
            connection::Action::SendPacket((p, _)) => {
                encode(&p);
                connection::Input::PacketSent
            }
            connection::Action::ReleaseData(_) => connection::Input::DataReleased,
            connection::Action::UpdateStatistics(_) => connection::Input::StatisticsUpdated,
            connection::Action::WaitForData(_) | connection::Action::Close => return,
        };
    }
}

fuzz_target!(|data: &[u8]| {
    let start = Instant::now();
    let local: SocketAddr = "127.0.0.1:8890".parse().expect("addr");
    let peers: [SocketAddr; 3] = [
        "192.0.2.1:5000".parse().expect("addr"),
        "192.0.2.2:5000".parse().expect("addr"),
        "[2001:db8::1]:5000".parse().expect("addr"),
    ];
    let mut listener = MultiplexListener::new(start, local, ConnInitSettings::default());
    let mut cookies: HashMap<SocketAddr, i32> = HashMap::new();
    let mut open: HashMap<SocketAddr, DuplexConnection> = HashMap::new();
    let mut now = start;
    let mut rest = data;
    // Each record: control byte, time step, length (2 bytes), datagram.
    while rest.len() >= 4 {
        let (ctl, step) = (rest[0], rest[1]);
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]])).min(rest.len() - 4);
        let datagram = &rest[4..4 + len];
        rest = &rest[4 + len..];
        now += Duration::from_millis(u64::from(step) * 10);
        let peer = peers[usize::from(ctl % 3)];
        if ctl & 0x80 != 0 {
            for conn in open.values_mut() {
                drive(conn, now, connection::Input::Timer);
            }
        }
        let mut input = if ctl & 0x80 != 0 {
            Input::Timer
        } else {
            let mut buf = datagram;
            match Packet::parse(&mut buf, peer.is_ipv6()) {
                Ok(mut p) => {
                    // A client echoes the cookie from the induction response.
                    if ctl & 0x40 == 0
                        && let Some(h) = handshake(&mut p)
                        && h.shake_type == ShakeType::Conclusion
                        && let Some(&c) = cookies.get(&peer)
                    {
                        h.syn_cookie = c;
                    }
                    Input::Packet(Ok((p, peer)))
                }
                Err(e) => Input::Packet(Err(ReceivePacketError::Parse(e))),
            }
        };
        for _ in 0..64 {
            let next = match listener.handle_input(now, input) {
                Action::SendPacket((mut p, a)) => {
                    encode(&p);
                    if let Some(h) = handshake(&mut p)
                        && h.shake_type == ShakeType::Induction
                    {
                        cookies.insert(a, h.syn_cookie);
                    }
                    Input::Success(ResultOf::SendPacket(a))
                }
                Action::RequestAccess(id, _) => {
                    let response = match ctl & 0x30 {
                        0x00 | 0x10 => AccessControlResponse::Accepted(None),
                        0x20 => AccessControlResponse::Rejected(RejectReason::Server(
                            ServerRejectReason::Unauthorized,
                        )),
                        _ => AccessControlResponse::Dropped,
                    };
                    Input::AccessResponse(Some((id, response)))
                }
                Action::RejectConnection(id, p) => {
                    if let Some((p, _)) = p {
                        encode(&p);
                    }
                    Input::Success(ResultOf::RejectConnection(id))
                }
                Action::OpenConnection(id, b) => {
                    let (packet, conn) = *b;
                    if let Some((p, _)) = &packet {
                        encode(p);
                    }
                    open.insert(id.0, DuplexConnection::new(conn));
                    Input::Success(ResultOf::OpenConnection(id))
                }
                Action::DelegatePacket(id, packet) => {
                    if let Some(conn) = open.get_mut(&id.0) {
                        drive(conn, now, connection::Input::Packet(Ok(packet)));
                    }
                    Input::Success(ResultOf::DelegatePacket(id))
                }
                Action::DropConnection(id) => {
                    open.remove(&id.0);
                    Input::Success(ResultOf::DropConnection(id))
                }
                Action::UpdateStatistics(_) => Input::Success(ResultOf::UpdateStatistics),
                Action::WaitForInput | Action::Close => break,
            };
            input = next;
        }
    }
});
