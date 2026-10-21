//! SRT through our own `sapflow-srt`: one UDP
//! socket, one task driving the sans-IO listener, and a task per publisher
//! that feeds its media to the muxer, so a slow or failing muxer holds up
//! only its own stream.
//!
//! The listener sets no timer while there are no connections, and the
//! socket's own address is never asked for per packet.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use sapflow_srt::{CloseReason, Config, ConnId, Event, Listener, RejectCode};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::App;
use crate::publish::Publication;
use crate::srt::{IDLE, Refused, admit, feed};

/// Payloads queued for a publisher's task. More than the largest receive
/// buffer, so a SHUTDOWN that delivers everything held at once fits.
const QUEUE: usize = 16_384;

pub async fn serve(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let socket = UdpSocket::bind(addr).await?;
    tracing::info!(%addr, implementation = "own", encrypted = app.config.srt_crypto.is_some(), "SRT listening");
    let config = listener_config(&app.config);
    let mut listener = Listener::new(config, Instant::now());
    // Publisher tasks that ended (idle, over the bitrate cap, bad media)
    // and whose SRT connection should be shut.
    let (ended_tx, mut ended_rx) = mpsc::unbounded_channel::<ConnId>();
    let mut publishers: HashMap<ConnId, mpsc::Sender<Vec<u8>>> = HashMap::new();
    // Big enough for any datagram, so none is truncated into a valid one.
    let mut buf = vec![0u8; 65_536];
    // One timer, moved only when the listener's deadline changes.
    let timer = tokio::time::sleep(std::time::Duration::ZERO);
    tokio::pin!(timer);
    let mut armed: Option<Instant> = None;
    loop {
        let deadline = listener.poll_timeout();
        if deadline != armed {
            if let Some(d) = deadline {
                timer.as_mut().reset(d.into());
            }
            armed = deadline;
        }
        tokio::select! {
            r = socket.recv_from(&mut buf) => {
                // A failed receive (an ICMP error from an earlier send, on
                // some systems) is not the listener's end.
                if let Ok((n, peer)) = r
                    && let Some(datagram) = buf.get(..n)
                {
                    listener.handle_datagram(Instant::now(), peer, datagram);
                }
            }
            () = &mut timer, if armed.is_some() => {
                armed = None;
                listener.handle_timeout(Instant::now());
            }
            Some(id) = ended_rx.recv() => {
                publishers.remove(&id);
                listener.close(id, Instant::now());
            }
        }
        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect {
                    id,
                    peer,
                    stream_id,
                } => {
                    let stream_id = String::from_utf8_lossy(&stream_id);
                    match admit(&app, &stream_id, peer.ip(), publishers.len()) {
                        Ok(publication) => {
                            if !listener.accept(id, Instant::now()) {
                                // Wrong secret/cipher is terminal. Count it
                                // as a failed login before releasing the claim;
                                // no publisher queue/task or media is created.
                                app.auth_throttle
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .failed(peer.ip(), Instant::now());
                                app.log_refusal("srt", &"wrong passphrase or key length");
                                drop(publication);
                                continue;
                            }
                            let (tx, rx) = mpsc::channel(QUEUE);
                            publishers.insert(id, tx);
                            tracing::info!(
                                latency_ms = listener.latency(id).map(|d| d.as_millis() as u64),
                                "SRT publisher accepted"
                            );
                            tokio::spawn(publish(id, publication, rx, ended_tx.clone()));
                        }
                        Err(r) => {
                            listener.reject(id, reject_code(r));
                        }
                    }
                }
                Event::Data { id, payload } => {
                    if let Some(tx) = publishers.get(&id)
                        && tx.try_send(payload).is_err()
                    {
                        // The task ended, or its media is not keeping up.
                        tracing::warn!("SRT publisher's queue is full or gone; dropping it");
                        publishers.remove(&id);
                        listener.close(id, Instant::now());
                    }
                }
                Event::Closed { id, reason, stats } => {
                    // Dropping the sender ends the publisher's task once it
                    // has fed what is queued.
                    publishers.remove(&id);
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
            if let Err(e) = socket.send_to(&datagram, to).await {
                tracing::debug!(error = %e, "SRT send failed");
            }
        }
    }
}

fn listener_config(config: &crate::config::Config) -> Config {
    Config {
        // Room for publishers plus callers being turned away. Configured KM
        // and publish authentication are independent admission requirements.
        max_connections: config.max_publishers.saturating_mul(2).max(4),
        require_encryption: config.srt_crypto.is_some(),
        crypto: config.srt_crypto.clone(),
        ..Config::default()
    }
}

fn reject_code(r: Refused) -> RejectCode {
    RejectCode::http(r.status())
}

/// One publisher: payloads in order until the connection closes, the
/// publisher goes quiet for [`IDLE`], or its media is refused.
async fn publish(
    id: ConnId,
    mut publication: Publication,
    mut rx: mpsc::Receiver<Vec<u8>>,
    ended: mpsc::UnboundedSender<ConnId>,
) {
    loop {
        match tokio::time::timeout(IDLE, rx.recv()).await {
            Ok(Some(data)) => {
                if let Err(e) = feed(&mut publication, &data) {
                    tracing::warn!(error = %e, "dropping SRT publisher");
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                tracing::info!("SRT publisher idle, dropping");
                break;
            }
        }
    }
    publication.end();
    let _ = ended.send(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    use sapflow_srt::seq::Seq;
    use sapflow_srt::{Caller, CallerConfig};
    use std::time::Duration;

    #[test]
    fn configured_origin_accepts_matching_crypto_and_refuses_downgrade() {
        for cipher in ["aes128", "aes256"] {
            for profile in 0..4 {
                let config = crate::config::Config::load(
                    |key| match key {
                        "SAPFLOW_OPEN_PUBLISH" => Some("1".into()),
                        "SAPFLOW_SRT_PASSPHRASE" => Some("transport test secret".into()),
                        "SAPFLOW_SRT_CIPHER" => Some(cipher.into()),
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
                assert_eq!(accepted, profile == 0);
                if profile == 0 {
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
