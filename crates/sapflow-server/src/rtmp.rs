//! RTMP ingest (OBS's default, Larix's too). Publish only: no playback.
//!
//! The handshake and chunk protocol are ours (`sapflow_rtmp`,
//! sans-IO); this is the socket loop and the policy. Credentials ride in
//! the stream key's query (`<eventId>?user=..&pass=..`) and are checked
//! when the encoder asks to publish, before any media is accepted.

use std::hash::{BuildHasher, RandomState};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sapflow_core::PublishRequest;
use sapflow_rtmp::{Connection, Event};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

use crate::App;
use crate::publish::{Publication, Refusal};
use crate::srt::IDLE;
use crate::stream::{Source, SourceKind};

/// Unauthenticated connections get this long, in total, to finish the
/// handshake and ask to publish.
const SETUP: Duration = Duration::from_secs(10);

pub async fn listen(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "RTMP listening");
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let (sock, peer) = listener.accept().await?;
        // Connections, not publishers: a flood of idle sockets is bounded
        // by SETUP and this cap.
        if active.load(Ordering::Relaxed) >= app.config.max_publishers * 4 {
            drop(sock);
            continue;
        }
        let app = app.clone();
        let slot = Slot::take(&active);
        tokio::spawn(async move {
            // Held until the task ends, even if it panics.
            let _slot = slot;
            if let Err(e) = connection(&app, sock, peer.ip()).await {
                tracing::info!(error = %e, "RTMP connection ended");
            }
        });
    }
}

/// One counted connection; released on drop, so a panicking task frees it.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(active: &Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        Self(active.clone())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
enum End {
    Io(std::io::Error),
    Protocol(String),
    Idle,
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Idle => f.write_str("idle"),
        }
    }
}

impl From<std::io::Error> for End {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

async fn read_some(sock: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> Result<usize, End> {
    match tokio::time::timeout_at(deadline, sock.read(buf)).await {
        Ok(Ok(0)) => Err(End::Io(std::io::ErrorKind::UnexpectedEof.into())),
        Ok(Ok(n)) => Ok(n),
        Ok(Err(e)) => Err(End::Io(e)),
        Err(_) => Err(End::Idle),
    }
}

async fn connection(app: &Arc<App>, mut sock: TcpStream, from: IpAddr) -> Result<(), End> {
    let _ = sock.set_nodelay(true);
    let mut buf = vec![0u8; 64 * 1024];
    // S1 and S2's filler: unpredictable is nice, not needed.
    let mut rtmp = Connection::new(RandomState::new().hash_one(from));
    let mut conn = Conn {
        app,
        from,
        publication: None,
    };
    let setup_until = Instant::now() + SETUP;
    let result = loop {
        let mut finished = false;
        let mut failed = None;
        loop {
            match rtmp.poll() {
                Ok(Some(ev)) => match conn.event(&mut rtmp, ev) {
                    Ok(done) => {
                        if done {
                            finished = true;
                            break;
                        }
                    }
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    failed = Some(End::Protocol(e.to_string()));
                    break;
                }
            }
        }
        // Replies first (a refusal is sent before closing).
        let out = rtmp.take_output();
        if !out.is_empty()
            && let Err(e) = sock.write_all(&out).await
        {
            break Err(End::Io(e));
        }
        if let Some(e) = failed {
            break Err(e);
        }
        if finished {
            break Ok(());
        }
        let deadline = if conn.publication.is_some() {
            Instant::now() + IDLE
        } else {
            setup_until
        };
        match read_some(&mut sock, &mut buf, deadline).await {
            Ok(n) => {
                if let Some(p) = &mut conn.publication
                    && let Err(e) = p.received(n)
                {
                    break Err(End::Protocol(e.to_string()));
                }
                if let Err(e) = rtmp.feed(&buf[..n]) {
                    break Err(End::Protocol(e.to_string()));
                }
            }
            Err(e) => break Err(e),
        }
    };
    if let Some(p) = conn.publication.take() {
        p.end();
    }
    result
}

struct Conn<'a> {
    app: &'a Arc<App>,
    from: IpAddr,
    publication: Option<Publication>,
}

impl Conn<'_> {
    /// Handles one session event; returns whether the connection is done
    /// (the publisher finished, or was refused).
    fn event(&mut self, rtmp: &mut Connection, ev: Event) -> Result<bool, End> {
        match ev {
            Event::Publish { app, key } => {
                let begun = PublishRequest::parse_rtmp(&app, &key)
                    .map_err(Refusal::Request)
                    .and_then(|req| Publication::begin(self.app, &req, SourceKind::Flv, self.from));
                match begun {
                    Ok(p) => {
                        self.publication = Some(p);
                        rtmp.accept_publish();
                        Ok(false)
                    }
                    Err(r) => {
                        self.app.log_refusal("rtmp", &r);
                        let (code, text) = match r {
                            Refusal::AlreadyLive => ("NetStream.Publish.BadName", "already live"),
                            _ => ("NetStream.Publish.Denied", "refused"),
                        };
                        // Send the refusal, then close.
                        rtmp.refuse_publish(code, text);
                        Ok(true)
                    }
                }
            }
            Event::Metadata { audio } => {
                if let Some(p) = &mut self.publication {
                    let _ = p.push(|src, _| {
                        if let Source::Flv(f) = src {
                            f.expect_audio(audio);
                        }
                        Ok(Default::default())
                    });
                }
                Ok(false)
            }
            Event::Video { timestamp, data } => {
                let Some(p) = &mut self.publication else {
                    return Ok(false);
                };
                p.push(|src, now| match src {
                    Source::Flv(f) => f.video(timestamp, &data, now),
                    Source::Ts(_) => Ok(Default::default()),
                })
                .map_err(|e| End::Protocol(e.to_string()))?;
                Ok(false)
            }
            Event::Audio { timestamp, data } => {
                if let Some(p) = &mut self.publication {
                    let _ = p.push(|src, _| {
                        if let Source::Flv(f) = src {
                            f.audio(timestamp, &data);
                        }
                        Ok(Default::default())
                    });
                }
                Ok(false)
            }
            Event::Finished => Ok(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panicking_connection_frees_its_slot() {
        let active = Arc::new(AtomicUsize::new(0));
        let slot = Slot::take(&active);
        assert_eq!(active.load(Ordering::Relaxed), 1);
        let task = tokio::spawn(async move {
            let _slot = slot;
            panic!("hostile input");
        });
        assert!(task.await.is_err());
        assert_eq!(active.load(Ordering::Relaxed), 0);
    }
}
