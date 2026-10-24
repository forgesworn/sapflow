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
    let mut protocol_failed = false;
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
        // A failed reply write must not hide an earlier parser rejection.
        protocol_failed |= matches!(failed, Some(End::Protocol(_)));
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
        // The strict wire parser can reject media or metadata before a push
        // reaches ingest. Its already accepted recording prefix is incomplete.
        if protocol_failed || matches!(result, Err(End::Protocol(_))) {
            p.loss_notice().mark_incomplete();
        }
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
                            Refusal::PublisherLimit => {
                                ("NetStream.Publish.Denied", "publisher limit")
                            }
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
            Event::Tracks { tracks } => {
                if let Some(p) = &mut self.publication {
                    p.push(|src, now| match src {
                        Source::Flv(f) => f.declare_tracks(tracks, now),
                        Source::Ts(_) => Ok(Default::default()),
                    })
                    .map_err(|e| End::Protocol(e.to_string()))?;
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
                    p.push(|src, now| match src {
                        Source::Flv(f) => f.audio_at(timestamp, &data, now),
                        Source::Ts(_) => Ok(Default::default()),
                    })
                    .map_err(|e| End::Protocol(e.to_string()))?;
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
    use sapflow_hls::TrackSet;
    use sapflow_rtmp::amf0::{self, Value};
    use sapflow_rtmp::chunk::ChunkWriter;
    use sapflow_rtmp::client::{Publisher, PublisherConfig, PublisherState};

    #[tokio::test]
    async fn protocol_rejection_after_accepted_audio_preserves_an_incomplete_prefix() {
        for ending in ["finished", "partial-topology", "forbidden-video"] {
            let mut fixture = crate::publish::loss_tests::Fixture::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("owned loopback listener");
            let address = listener.local_addr().expect("owned address");
            let mut socket = TcpStream::connect(address).await.expect("connect fixture");
            let (server, peer) = listener.accept().await.expect("accept fixture");
            drop(listener);
            let app = fixture.app.clone();
            let server = tokio::spawn(async move { connection(&app, server, peer.ip()).await });
            let mut publisher = Publisher::new(
                PublisherConfig::new(
                    "live".into(),
                    "event-a".into(),
                    "rtmp://localhost/live".into(),
                ),
                1,
                0,
            )
            .expect("bounded publisher");
            publisher
                .declare_tracks(TrackSet::AudioOnly)
                .expect("AAC topology");
            let started = Instant::now();
            let mut bytes = [0; 16 * 1024];
            while publisher.state() != PublisherState::Publishing {
                socket
                    .write_all(&publisher.take_output())
                    .await
                    .expect("setup write");
                let count = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut bytes))
                    .await
                    .expect("bounded setup read")
                    .expect("setup read");
                assert!(count > 0);
                publisher
                    .feed(&bytes[..count], started.elapsed().as_millis() as u64)
                    .expect("actual production handshake");
            }
            publisher
                .send_aac_config(&[0x11, 0x88])
                .expect("48 kHz mono ASC");
            for frame in 0..120u32 {
                // Original placeholders check recording status, not AAC decode.
                publisher
                    .send_aac(
                        (u64::from(frame) * 1024 * 1000 / 48_000) as u32,
                        &[0x5a; 64],
                    )
                    .expect("bounded AAC frame");
            }
            socket
                .write_all(&publisher.take_output())
                .await
                .expect("accepted audio write");
            match ending {
                "finished" => {
                    publisher.finish().expect("normal finish");
                    socket
                        .write_all(&publisher.take_output())
                        .await
                        .expect("finish write");
                }
                _ => {
                    let mut bad = Vec::new();
                    let (kind, body) = if ending == "forbidden-video" {
                        (9, vec![0x17, 1, 0, 0, 0])
                    } else {
                        let mut body = Vec::new();
                        amf0::encode(&Value::String("onMetaData".into()), &mut body);
                        amf0::encode(
                            &Value::Object(vec![("hasAudio".into(), Value::Boolean(true))]),
                            &mut body,
                        );
                        (18, body)
                    };
                    assert!(body.len() < 128);
                    ChunkWriter::default().write(&mut bad, 19, kind, 1, 3000, &body);
                    socket
                        .write_all(&bad)
                        .await
                        .expect("strict parser rejection input");
                }
            }
            let result = tokio::time::timeout(Duration::from_secs(3), server)
                .await
                .expect("bounded connection exit")
                .expect("connection task");
            if ending == "finished" {
                assert!(result.is_ok());
                fixture.completion("finalised").await;
            } else {
                assert!(matches!(result, Err(End::Protocol(_))));
                fixture.completion("incomplete").await;
                assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
            }
        }
    }

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

    fn negotiate_capacity_fixture(
        conn: &mut Conn<'_>,
        key: &str,
    ) -> (Result<(), sapflow_rtmp::client::ClientError>, Vec<u8>, bool) {
        let mut server = Connection::new(7);
        let mut client = Publisher::new(
            PublisherConfig::new("live".into(), key.into(), "rtmp://localhost/live".into()),
            11,
            0,
        )
        .expect("bounded client");
        let mut wire = Vec::new();
        for step in 0..32 {
            server
                .feed(&client.take_output())
                .expect("generated client bytes");
            let mut done = false;
            while let Some(event) = server.poll().expect("actual server parser") {
                done = conn
                    .event(&mut server, event)
                    .expect("actual connection policy");
                if done {
                    break;
                }
            }
            let output = server.take_output();
            wire.extend_from_slice(&output);
            if let Err(error) = client.feed(&output, step) {
                return (Err(error), wire, done);
            }
            if client.state() == PublisherState::Publishing {
                return (Ok(()), wire, done);
            }
            assert!(
                !done,
                "refused onStatus must reach the client before closing"
            );
        }
        panic!("bounded 32-exchange handshake did not terminate");
    }

    #[test]
    fn setup_slots_remain_independent_and_publication_capacity_is_visible_on_the_wire() {
        let mut fixture = crate::publish::loss_tests::Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.recordings = None;
        app.config.max_publishers = 2;
        app.streams = Arc::new(crate::stream::Registry::default());
        let active = Arc::new(AtomicUsize::new(0));
        // Keep the existing 4N connection bookkeeping occupied. These slots
        // model accepted setup sockets; they must not reserve publication credit.
        let setup = (0..app.config.max_publishers * 4)
            .map(|_| Slot::take(&active))
            .collect::<Vec<_>>();
        assert_eq!(active.load(Ordering::Relaxed), 8);
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 0);
        let from = "127.0.0.1".parse().expect("loopback");
        let mut first = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        let mut second = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        let mut refused = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        assert!(
            negotiate_capacity_fixture(&mut first, "event-one")
                .0
                .is_ok()
        );
        assert!(
            negotiate_capacity_fixture(&mut second, "event-two")
                .0
                .is_ok()
        );
        let (result, wire, done) = negotiate_capacity_fixture(&mut refused, "event-three");
        assert!(
            matches!(result, Err(sapflow_rtmp::client::ClientError::Refused(code))
            if code == "NetStream.Publish.Denied")
        );
        assert!(done && refused.publication.is_none());
        assert!(
            wire.windows(b"publisher limit".len())
                .any(|s| s == b"publisher limit")
        );
        assert!(fixture.app.streams.get("event-three").is_none());
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 2);
        assert_eq!(active.load(Ordering::Relaxed), 8);
        first.publication.take().expect("first accepted").end();
        second.publication.take().expect("second accepted").end();
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 0);
        drop(setup);
        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert!(fixture.callbacks.is_empty());
    }
}
