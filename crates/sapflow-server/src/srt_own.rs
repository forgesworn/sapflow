//! Our own SRT listener. Admission persistence runs in bounded asynchronous
//! tasks; the UDP owner continues handling timers and established publishers.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use sapflow_srt::{CloseReason, Config, ConnId, Event, Listener, RejectCode};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::App;
use crate::publish::{Publication, PublicationLoss};
use crate::publish_sessions::ScopedLease;
use crate::srt::{IDLE, Refused, admit, feed};

const QUEUE: usize = 16_384;
struct Payload {
    bytes: Vec<u8>,
    received: Instant,
}
struct Publisher {
    tx: mpsc::Sender<Payload>,
    scope: Option<ScopedLease>,
    loss: PublicationLoss,
}
impl Publisher {
    fn closed(self, reason: Option<CloseReason>, dropped: u64) {
        // `lost` also counts recovered loss. Only irrecoverable drops, or
        // an abnormal transport timeout, make this prefix incomplete.
        if incomplete_close(reason, dropped) {
            self.loss.mark_incomplete();
        }
        if let Some(scope) = self.scope {
            scope.protocol_stopped();
        }
    }

    fn enqueue(&self, payload: Payload) -> bool {
        match self.tx.try_send(payload) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.loss.mark_incomplete();
                false
            }
            // End/revoke may already have closed admission normally.
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}
fn incomplete_close(reason: Option<CloseReason>, dropped: u64) -> bool {
    dropped != 0 || matches!(reason, Some(CloseReason::Timeout))
}
struct Engine {
    listener: Listener,
    publishers: HashMap<ConnId, Publisher>,
}
impl Drop for Engine {
    fn drop(&mut self) {
        // Also covers listener failure/cancellation before the normal ended
        // message. Native protocol state is destroyed before its receipt says
        // stopped, and dropping senders lets independent media owners finish.
        for (id, publisher) in &self.publishers {
            publisher.loss.mark_incomplete();
            self.listener.close(*id, Instant::now());
            if let Some(scope) = &publisher.scope {
                scope.protocol_stopped();
            }
        }
    }
}

/// Receive buffer asked for per allowed publisher. Every SRT publisher shares
/// one UDP socket, so the OS default (under 1 MB on macOS, 208 KB on Linux)
/// overflows in bursts with tens of streams and drops datagrams before SRT
/// sees them.
const RCVBUF_PER_PUBLISHER: usize = 512 * 1024;
const RCVBUF_MIN: usize = 4 * 1024 * 1024;
const RCVBUF_MAX: usize = 64 * 1024 * 1024;

fn wanted_recv_buffer(max_publishers: usize) -> usize {
    max_publishers
        .saturating_mul(RCVBUF_PER_PUBLISHER)
        .clamp(RCVBUF_MIN, RCVBUF_MAX)
}

/// Binds the SRT socket with a receive buffer of up to `want` bytes. macOS
/// refuses a size above its limit, so halve until the OS accepts one (Linux
/// accepts and caps silently). Returns the socket and the size the OS reports.
fn bind_udp(addr: SocketAddr, want: usize) -> std::io::Result<(UdpSocket, usize)> {
    let std_socket = std::net::UdpSocket::bind(addr)?;
    let mut size = want;
    while size >= 64 * 1024 {
        if rustix::net::sockopt::set_socket_recv_buffer_size(&std_socket, size).is_ok() {
            break;
        }
        size /= 2;
    }
    let got = rustix::net::sockopt::socket_recv_buffer_size(&std_socket)?;
    std_socket.set_nonblocking(true)?;
    Ok((UdpSocket::from_std(std_socket)?, got))
}

pub async fn serve(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let want = wanted_recv_buffer(app.config.max_publishers);
    let (socket, recv_buffer) = bind_udp(addr, want)?;
    if recv_buffer < want {
        tracing::warn!(
            requested = want,
            granted = recv_buffer,
            "SRT receive buffer below the requested size; raise the OS limit (Linux net.core.rmem_max, macOS kern.ipc.maxsockbuf) if many publishers share this server"
        );
    }
    let encryption = if app.publish_sessions.is_some()
        || (app.config.srt_crypto.is_some() && !app.config.srt_allow_plaintext)
    {
        "required"
    } else if app.config.srt_crypto.is_some() {
        "optional"
    } else {
        "off"
    };
    tracing::info!(%addr,implementation="own",recv_buffer,scoped=app.publish_sessions.is_some(),encryption,"SRT listening");
    let mut engine = Engine {
        listener: Listener::new(listener_config(&app.config), Instant::now()),
        publishers: HashMap::new(),
    };
    let mut admissions = JoinSet::new();
    let (ended_tx, mut ended_rx) = mpsc::channel::<ConnId>(app.config.max_publishers.max(1));
    let mut buf = vec![0u8; 65_536];
    let timer = tokio::time::sleep(std::time::Duration::ZERO);
    tokio::pin!(timer);
    let mut armed = None;
    loop {
        let Engine {
            listener,
            publishers,
        } = &mut engine;
        let deadline = listener.poll_timeout();
        if deadline != armed {
            if let Some(d) = deadline {
                timer.as_mut().reset(d.into());
            }
            armed = deadline;
        }
        tokio::select! {
            result=socket.recv_from(&mut buf)=>{
                if let Ok((n,peer))=result && let Some(datagram)=buf.get(..n) {listener.handle_datagram(Instant::now(),peer,datagram);}
            }
            ()=&mut timer,if armed.is_some()=>{armed=None;listener.handle_timeout(Instant::now());}
            Some(id)=ended_rx.recv()=>{
                listener.close(id,Instant::now());
                if let Some(publisher)=publishers.remove(&id) && let Some(scope)=publisher.scope {scope.protocol_stopped();}
            }
            Some(result)=admissions.join_next(),if !admissions.is_empty()=>{
                if let Ok((id,peer,result))=result {complete_admission(&app,listener,publishers,&ended_tx,id,peer,result);}
            }
        }
        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect {
                    id,
                    peer,
                    stream_id,
                } => {
                    let active = publishers.len() + admissions.len();
                    if active >= app.config.max_publishers {
                        listener.reject(id, RejectCode::http(402));
                        continue;
                    }
                    let app = app.clone();
                    admissions.spawn(async move {
                        let result = admit(
                            &app,
                            &String::from_utf8_lossy(&stream_id),
                            peer.ip(),
                            active,
                        )
                        .await;
                        (id, peer, result)
                    });
                }
                Event::Data { id, payload } => {
                    if let Some(publisher) = publishers.get(&id)
                        && !publisher.enqueue(Payload {
                            bytes: payload,
                            received: Instant::now(),
                        })
                    {
                        tracing::warn!("SRT publisher queue is full or gone");
                        listener.close(id, Instant::now());
                        if let Some(publisher) = publishers.remove(&id)
                            && let Some(scope) = publisher.scope
                        {
                            scope.protocol_stopped();
                        }
                    }
                }
                Event::Closed { id, reason, stats } => {
                    if let Some(publisher) = publishers.remove(&id) {
                        publisher.closed(reason, stats.dropped);
                    }
                    if let Some(reason) = reason {
                        tracing::info!(
                            reason = match reason {
                                CloseReason::Shutdown => "shutdown",
                                CloseReason::Timeout => "timeout",
                            },
                            received = stats.received,
                            retransmitted = stats.retransmitted,
                            lost = stats.lost,
                            dropped = stats.dropped,
                            "SRT connection closed"
                        );
                    }
                }
            }
        }
        while let Some((to, datagram)) = listener.poll_transmit() {
            if socket.send_to(&datagram, to).await.is_err() {
                tracing::debug!("SRT send failed");
            }
        }
    }
}

fn complete_admission(
    app: &Arc<App>,
    listener: &mut Listener,
    publishers: &mut HashMap<ConnId, Publisher>,
    ended: &mpsc::Sender<ConnId>,
    id: ConnId,
    peer: SocketAddr,
    result: Result<Publication, Refused>,
) {
    let publication = match result {
        Ok(publication) => publication,
        Err(error) => {
            listener.reject(id, reject_code(error));
            return;
        }
    };
    let scope = publication.scoped_lease();
    let (accepted, capacity) = if let Some(scope) = &scope {
        let crypto = scope.crypto();
        let capacity = scope
            .limits()
            .map(|(limits, _)| (limits.max_queue_bytes / 1500) as usize);
        match capacity.and_then(|capacity| {
            scope.with_open(|| {
                let accepted = listener.accept_with_crypto(id, Instant::now(), crypto);
                if accepted {
                    scope.accepted();
                }
                (accepted, capacity)
            })
        }) {
            Ok(result) => result,
            Err(_) => {
                listener.reject(id, RejectCode::http(403));
                scope.protocol_stopped();
                return;
            }
        }
    } else {
        (listener.accept(id, Instant::now()), QUEUE)
    };
    if !accepted {
        if let Some(scope) = &scope {
            scope.protocol_stopped();
        }
        app.auth_throttle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failed(peer.ip(), Instant::now());
        app.log_refusal(
            "srt",
            &"passphrase, key length or admission handshake refused",
        );
        return;
    }
    let (tx, rx) = mpsc::channel(capacity.clamp(1, QUEUE));
    let loss = publication.loss_notice();
    publishers.insert(id, Publisher { tx, scope, loss });
    tracing::info!(
        latency_ms = listener.latency(id).map(|d| d.as_millis() as u64),
        "SRT publisher accepted"
    );
    tokio::spawn(publish(id, publication, rx, ended.clone()));
}

fn listener_config(config: &crate::config::Config) -> Config {
    Config {
        max_connections: config.max_publishers.saturating_mul(2).max(4),
        require_encryption: config.control.is_some()
            || (config.srt_crypto.is_some() && !config.srt_allow_plaintext),
        crypto: config.srt_crypto.clone(),
        allow_plaintext: config.srt_allow_plaintext,
        ..Config::default()
    }
}
fn reject_code(reason: Refused) -> RejectCode {
    RejectCode::http(reason.status())
}

async fn cancelled(cancel: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    let Some(receiver) = cancel else {
        return std::future::pending().await;
    };
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
fn reject_stale(publication: &Publication, received: Instant, scoped: bool) -> bool {
    if scoped && received.elapsed() > std::time::Duration::from_secs(1) {
        publication.loss_notice().mark_incomplete();
        true
    } else {
        false
    }
}

async fn publish(
    id: ConnId,
    mut publication: Publication,
    mut rx: mpsc::Receiver<Payload>,
    ended: mpsc::Sender<ConnId>,
) {
    let scope = publication.scoped_lease();
    let mut cancel = scope.as_ref().map(ScopedLease::cancellation);
    loop {
        tokio::select! {
            biased;
            ()=cancelled(&mut cancel)=>break,
            result=tokio::time::timeout(IDLE,rx.recv())=>match result {
                Ok(Some(data))=>{
                    if reject_stale(&publication, data.received, scope.is_some()){tracing::warn!("SRT scoped media queue exceeded one second");break}
                    if let Err(error)=feed(&mut publication,&data.bytes){tracing::warn!(%error,"dropping SRT publisher");break}
                }
                Ok(None)=>break,
                Err(_)=>{tracing::info!("SRT publisher idle");break}
            }
        }
    }
    publication.end();
    let _ = ended.send(id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    use sapflow_srt::seq::Seq;
    use sapflow_srt::{Caller, CallerConfig};
    use std::time::Duration;

    #[tokio::test]
    async fn queue_full_marks_gap_but_closed_worker_and_graceful_shutdown_do_not() {
        use crate::publish::loss_tests::Fixture;
        for cause in [
            "full", "closed", "shutdown", "dropped", "timeout", "age", "engine",
        ] {
            let mut fixture = Fixture::new();
            let mut publication = fixture.begin();
            Fixture::feed(&mut publication);
            let (tx, rx) = mpsc::channel(1);
            let publisher = Publisher {
                tx,
                scope: None,
                loss: publication.loss_notice(),
            };
            let incomplete = !matches!(cause, "closed" | "shutdown");
            match cause {
                "full" => {
                    assert!(publisher.enqueue(Payload {
                        bytes: vec![0],
                        received: Instant::now()
                    }));
                    assert!(!publisher.enqueue(Payload {
                        bytes: vec![1],
                        received: Instant::now()
                    }));
                }
                "closed" => {
                    drop(rx);
                    assert!(!publisher.enqueue(Payload {
                        bytes: vec![0],
                        received: Instant::now()
                    }));
                }
                "shutdown" => publisher.closed(Some(CloseReason::Shutdown), 0),
                "dropped" => publisher.closed(Some(CloseReason::Shutdown), 1),
                "timeout" => publisher.closed(Some(CloseReason::Timeout), 0),
                "age" => {
                    let old = Instant::now() - Duration::from_secs(2);
                    assert!(!reject_stale(&publication, Instant::now(), true));
                    assert!(!reject_stale(&publication, old, false));
                    assert!(reject_stale(&publication, old, true));
                }
                _ => {
                    let mut engine = Engine {
                        listener: Listener::new(Config::default(), Instant::now()),
                        publishers: HashMap::new(),
                    };
                    // Engine Drop must report failure before closing the sender.
                    engine.publishers.insert(ConnId(1), publisher);
                    drop(engine);
                }
            }
            publication.end();
            fixture
                .completion(if incomplete {
                    "incomplete"
                } else {
                    "finalised"
                })
                .await;
        }
    }

    #[test]
    fn configured_origin_accepts_matching_crypto_and_refuses_downgrade() {
        // Optional encryption also admits plaintext (profile 1), never a
        // wrong passphrase or key length.
        for (cipher, encryption) in [
            ("aes128", "required"),
            ("aes256", "required"),
            ("aes128", "optional"),
            ("aes256", "optional"),
        ] {
            for profile in 0..4 {
                let config = crate::config::Config::load(
                    |key| match key {
                        "SAPFLOW_OPEN_PUBLISH" => Some("1".into()),
                        "SAPFLOW_SRT_PASSPHRASE" => Some("transport test secret".into()),
                        "SAPFLOW_SRT_CIPHER" => Some(cipher.into()),
                        "SAPFLOW_SRT_ENCRYPTION" => Some(encryption.into()),
                        _ => None,
                    },
                    [],
                )
                .expect("origin config");
                let start = Instant::now();
                let mut listener = Listener::new(listener_config(&config), start);
                let size = if cipher == "aes128" {
                    KeySize::Aes128
                } else {
                    KeySize::Aes256
                };
                let crypto = match profile {
                    1 => None,
                    2 => Some(
                        CryptoConfig::new(b"wrong transport secret".to_vec(), size)
                            .expect("secret"),
                    ),
                    3 => Some(
                        CryptoConfig::new(
                            b"transport test secret".to_vec(),
                            if size == KeySize::Aes128 {
                                KeySize::Aes256
                            } else {
                                KeySize::Aes128
                            },
                        )
                        .expect("secret"),
                    ),
                    _ => config.srt_crypto.clone(),
                };
                let peer = "192.0.2.1:9000".parse().expect("peer");
                let source = "192.0.2.2:9001".parse().expect("source");
                let mut caller = Caller::with_ids(
                    CallerConfig {
                        crypto,
                        ..CallerConfig::default()
                    },
                    peer,
                    11,
                    Seq::new(100),
                    start,
                )
                .expect("caller");
                let mut accepted = false;
                let mut sent = false;
                let mut received = Vec::new();
                for ms in (0..1000).step_by(10) {
                    let now = start + Duration::from_millis(ms);
                    caller.handle_timeout(now);
                    listener.handle_timeout(now);
                    for _ in 0..32 {
                        let Some((_, bytes)) = caller.poll_transmit(now) else {
                            break;
                        };
                        listener.handle_datagram(now, source, &bytes);
                    }
                    for _ in 0..32 {
                        let Some(event) = listener.poll_event() else {
                            break;
                        };
                        match event {
                            Event::Connect { id, .. } => accepted |= listener.accept(id, now),
                            Event::Data { payload, .. } => received.extend(payload),
                            Event::Closed { .. } => {}
                        }
                    }
                    for _ in 0..32 {
                        let Some((_, bytes)) = listener.poll_transmit() else {
                            break;
                        };
                        caller.handle_datagram(now, peer, &bytes);
                    }
                    if caller.is_connected() && !sent {
                        caller.enqueue(now, vec![0x47; 188]).expect("media");
                        sent = true;
                    }
                }
                let admitted = profile == 0 || (profile == 1 && encryption == "optional");
                assert_eq!(accepted, admitted, "{cipher} {encryption} {profile}");
                if admitted {
                    assert_eq!(received, vec![0x47; 188]);
                } else {
                    assert!(caller.is_closed());
                    assert!(received.is_empty());
                    assert_eq!(listener.connections(), 0);
                }
                caller.stop(start + Duration::from_secs(1));
            }
        }
    }
}

#[cfg(test)]
mod recv_buffer_tests {
    use super::*;

    #[test]
    fn wanted_buffer_scales_with_publishers_within_bounds() {
        assert_eq!(wanted_recv_buffer(1), RCVBUF_MIN);
        assert_eq!(wanted_recv_buffer(16), 8 * 1024 * 1024);
        assert_eq!(wanted_recv_buffer(50), 25 * 1024 * 1024);
        assert_eq!(wanted_recv_buffer(usize::MAX), RCVBUF_MAX);
    }

    #[tokio::test]
    async fn bind_raises_the_receive_buffer_above_the_default() -> std::io::Result<()> {
        let plain = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let default = rustix::net::sockopt::socket_recv_buffer_size(&plain)?;
        let (_socket, got) = bind_udp(SocketAddr::from(([127, 0, 0, 1], 0)), 4 * 1024 * 1024)?;
        assert!(got > default, "granted {got}, default {default}");
        Ok(())
    }
}
