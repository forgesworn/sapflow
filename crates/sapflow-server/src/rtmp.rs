//! RTMP ingest (OBS's default, Larix's too). Publish only: no playback.
//!
//! The handshake and chunk protocol are ours (`sapflow_rtmp`,
//! sans-IO); this is the socket loop and the policy. Credentials ride in
//! the stream key's query (`<eventId>?user=..&pass=..`) and are checked
//! when the encoder asks to publish, before any media is accepted: against
//! the configured publisher, or by asking the app
//! (`SAPFLOW_PUBLISH_AUTH_URL`) while this connection waits.
//!
//! A second listener (`SAPFLOW_RTMP_PROXY_ADDR`) sits behind a TLS front for
//! RTMPS. Each connection on it must come from the trusted front, checked
//! before a byte is read, and start with a PROXY protocol v2 header, parsed
//! with `sapflow_core::proxy_protocol`. The header's source is the
//! publisher's address for the failed-attempt throttle and the app's publish
//! check. Without it every RTMPS publisher would share the front's address,
//! and one encoder retrying a wrong key could throttle them all.
//! The plain listener never reads a header, since a header can only be
//! believed from a configured front.

use std::hash::{BuildHasher, RandomState};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sapflow_core::PublishRequest;
use sapflow_core::proxy_protocol::{self, Command, Parsed};
use sapflow_rtmp::{Connection, Event};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

use crate::App;
use crate::publish::{Publication, Refusal};
use crate::recorder::IncompleteReason;
use crate::srt::IDLE;
use crate::stream::{Source, SourceKind};

/// Unauthenticated connections get this long, in total, to finish the
/// handshake and ask to publish.
const SETUP: Duration = Duration::from_secs(10);

/// How a listener learns the publisher's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Front {
    /// The TCP peer is the publisher (`SAPFLOW_RTMP_ADDR`).
    Direct,
    /// Every connection comes from this front and starts with a PROXY
    /// protocol v2 header naming the publisher (`SAPFLOW_RTMP_PROXY_ADDR`).
    Proxied(IpAddr),
}

/// Listens on `addr`. `active` counts unfinished connections, and is shared
/// by the plain and proxied listeners so that together they keep one cap.
pub async fn listen(
    app: Arc<App>,
    addr: SocketAddr,
    front: Front,
    active: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    match front {
        Front::Direct => tracing::info!(%addr, "RTMP listening"),
        Front::Proxied(trusted) => {
            tracing::info!(%addr, %trusted, "RTMP listening behind a PROXY protocol v2 front");
        }
    }
    serve(app, listener, front, active).await
}

async fn serve(
    app: Arc<App>,
    listener: impl crate::accept::Accept,
    front: Front,
    active: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let mut warnings = crate::accept::Warnings::default();
    loop {
        let (mut sock, peer) = crate::accept::next(&listener, "RTMP", &mut warnings).await;
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
            // The header, the handshake and the publish request share it.
            let setup_until = Instant::now() + SETUP;
            let result = match front {
                Front::Direct => {
                    connection(&app, sock, peer.ip().to_canonical(), &[], setup_until).await
                }
                Front::Proxied(trusted) => {
                    match proxy_header(&mut sock, peer, trusted, setup_until).await {
                        Ok((from, rest)) => connection(&app, sock, from, &rest, setup_until).await,
                        Err(e) => Err(e),
                    }
                }
            };
            match result {
                Ok(()) => {}
                // The front's address, not a publisher's: logged so that a
                // wrong SAPFLOW_RTMP_TRUSTED_PROXY says what the peer was.
                Err(e @ End::Proxy(proxy_protocol::Error::UntrustedPeer)) => {
                    tracing::info!(error = %e, %peer, "RTMP connection ended");
                }
                Err(e) => tracing::info!(error = %e, "RTMP connection ended"),
            }
        });
    }
}

/// Reads the PROXY protocol v2 header that starts a connection on the
/// proxied listener, by `deadline`. Returns the publisher's address, with an
/// IPv4-mapped address as IPv4, and the bytes read past the header, which
/// are the start of the RTMP handshake. A peer other than `trusted` is
/// refused before a byte is read. LOCAL, the front's own connection such as
/// a health check, is refused once its header is in: there is no publisher
/// behind it, and taking the front as one would put its checks under the
/// throttle. Nothing here holds more than a header (at most
/// `proxy_protocol::MAX_HEADER_LEN` bytes) and one read past it.
async fn proxy_header(
    sock: &mut TcpStream,
    peer: SocketAddr,
    trusted: IpAddr,
    deadline: Instant,
) -> Result<(IpAddr, Vec<u8>), End> {
    if !proxy_protocol::is_trusted_proxy(peer.ip(), trusted) {
        return Err(End::Proxy(proxy_protocol::Error::UntrustedPeer));
    }
    let mut buf = Vec::with_capacity(proxy_protocol::MAX_HEADER_LEN);
    let mut chunk = [0u8; 1024];
    loop {
        match proxy_protocol::parse(&buf) {
            Ok(Parsed::Complete { header, consumed }) => {
                return match header.command() {
                    Command::Proxy { .. } => Ok((
                        header.client_address(peer).ip(),
                        buf.get(consumed..).unwrap_or_default().to_vec(),
                    )),
                    // LOCAL, and any command a later parser adds: no
                    // publisher behind it that we know how to name.
                    _ => Err(End::ProxyLocal),
                };
            }
            Ok(_) => {}
            Err(e) => return Err(End::Proxy(e)),
        }
        let n = read_some(sock, &mut chunk, deadline).await?;
        buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
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
    /// A refused PROXY header, or a peer that is not the trusted front.
    Proxy(proxy_protocol::Error),
    /// A PROXY LOCAL header: the front's own connection, with no publisher.
    ProxyLocal,
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Idle => f.write_str("idle"),
            Self::Proxy(e) => write!(f, "PROXY protocol: {e}"),
            Self::ProxyLocal => f.write_str("PROXY LOCAL: the front's own connection"),
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

/// One RTMP connection from `from`. `initial` is what came before the
/// first read, after a PROXY header in the same segment; the handshake and
/// the publish request must finish by `setup_until`.
async fn connection(
    app: &Arc<App>,
    mut sock: TcpStream,
    from: IpAddr,
    initial: &[u8],
    setup_until: Instant,
) -> Result<(), End> {
    let _ = sock.set_nodelay(true);
    let mut buf = vec![0u8; 64 * 1024];
    // S1 and S2's filler: unpredictable is nice, not needed.
    let mut rtmp = Connection::new(RandomState::new().hash_one(from));
    if !initial.is_empty() {
        rtmp.feed(initial)
            .map_err(|e| End::Protocol(e.to_string()))?;
    }
    let mut conn = Conn {
        app,
        from,
        publication: None,
    };
    let mut protocol_failed = false;
    let result = loop {
        let mut finished = false;
        let mut failed = None;
        loop {
            match rtmp.poll() {
                Ok(Some(ev)) => match conn.event(&mut rtmp, ev).await {
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
            p.loss_notice().mark_incomplete(IncompleteReason::Protocol);
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
    /// A publish request may wait on the app (`SAPFLOW_PUBLISH_AUTH_URL`),
    /// bounded by its own timeout; nothing else here awaits.
    async fn event(&mut self, rtmp: &mut Connection, ev: Event) -> Result<bool, End> {
        match ev {
            Event::Publish { app, key } => {
                let begun = match PublishRequest::parse_rtmp(&app, &key) {
                    Err(e) => Err(Refusal::Request(e)),
                    Ok(req) => {
                        Publication::begin_authorised(self.app, &req, SourceKind::Flv, self.from)
                            .await
                    }
                };
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
                            Refusal::PresentationLimit => {
                                ("NetStream.Publish.Denied", "presentation limit")
                            }
                            Refusal::MemoryLimit => {
                                ("NetStream.Publish.Denied", "media memory limit")
                            }
                            Refusal::Stopping => ("NetStream.Publish.Denied", "server stopping"),
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
                        Source::Ts(_) | Source::Es(_) => Ok(Default::default()),
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
                    Source::Ts(_) | Source::Es(_) => Ok(Default::default()),
                })
                .map_err(|e| End::Protocol(e.to_string()))?;
                Ok(false)
            }
            Event::Audio { timestamp, data } => {
                if let Some(p) = &mut self.publication {
                    p.push(|src, now| match src {
                        Source::Flv(f) => f.audio_at(timestamp, &data, now),
                        Source::Ts(_) | Source::Es(_) => Ok(Default::default()),
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
            let server = tokio::spawn(async move {
                connection(&app, server, peer.ip(), &[], Instant::now() + SETUP).await
            });
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

    /// An accept error (out of file descriptors, or a connection reset
    /// before it was taken) is not the end of the RTMP listener: the next
    /// encoder is still served.
    #[tokio::test]
    async fn an_accept_error_does_not_end_the_listener() {
        let fixture = crate::publish::loss_tests::Fixture::new();
        let inner = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("owned loopback listener");
        let address = inner.local_addr().expect("owned address");
        let listener = crate::accept::testing::Flaky::new(inner, 2);
        let server = tokio::spawn(serve(
            fixture.app.clone(),
            listener,
            Front::Direct,
            Arc::new(AtomicUsize::new(0)),
        ));
        let mut socket = TcpStream::connect(address).await.expect("connect");
        let mut c0c1 = vec![3u8];
        c0c1.resize(1 + 1536, 0);
        socket.write_all(&c0c1).await.expect("C0 and C1");
        let mut s0 = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(3), socket.read_exact(&mut s0)).await;
        assert!(
            matches!(read, Ok(Ok(_))) && s0[0] == 3,
            "the handshake was answered after two failed accepts"
        );
        assert!(!server.is_finished(), "the listener is still running");
        server.abort();
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

    async fn negotiate_capacity_fixture(
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
                    .await
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

    #[tokio::test]
    async fn setup_slots_remain_independent_and_publication_capacity_is_visible_on_the_wire() {
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
                .await
                .0
                .is_ok()
        );
        assert!(
            negotiate_capacity_fixture(&mut second, "event-two")
                .await
                .0
                .is_ok()
        );
        let (result, wire, done) = negotiate_capacity_fixture(&mut refused, "event-three").await;
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

    #[tokio::test]
    async fn app_authorisation_decides_an_rtmp_publish_on_the_wire() {
        use crate::publish_auth::stub::{self, Answer};
        for (answer, admitted) in [
            (Answer::Status(200), true),
            (Answer::Status(401), false),
            (Answer::Status(503), false),
        ] {
            let stub = stub::start(answer).await;
            let mut fixture = crate::publish::loss_tests::Fixture::new();
            let app = Arc::get_mut(&mut fixture.app).expect("sole App");
            app.config.recordings = None;
            app.config.publish_auth = Some(stub.endpoint.clone());
            let mut conn = Conn {
                app: &fixture.app,
                from: "127.0.0.1".parse().expect("loopback"),
                publication: None,
            };
            let (result, _, done) =
                negotiate_capacity_fixture(&mut conn, "event-one?user=encoder&pass=gig-secret")
                    .await;
            assert_eq!(result.is_ok(), admitted);
            assert_eq!(conn.publication.is_some(), admitted);
            if !admitted {
                assert!(done);
                assert!(
                    matches!(result, Err(sapflow_rtmp::client::ClientError::Refused(code))
                    if code == "NetStream.Publish.Denied")
                );
            }
            let sent = stub.last();
            assert_eq!(
                (
                    &sent["user"],
                    &sent["password"],
                    &sent["path"],
                    &sent["protocol"]
                ),
                (
                    &"encoder".into(),
                    &"gig-secret".into(),
                    &"live/event-one".into(),
                    &"rtmp".into()
                )
            );
            if let Some(publication) = conn.publication.take() {
                publication.end();
            }
        }
    }

    #[tokio::test]
    async fn a_live_publisher_is_not_taken_over_on_the_wire_until_it_goes_quiet() {
        let mut fixture = crate::publish::loss_tests::Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.recordings = None;
        app.streams = Arc::new(
            crate::stream::Registry::new(4)
                .with_takeover_silence(std::time::Duration::from_secs(3)),
        );
        let from = "127.0.0.1".parse().expect("loopback");
        let mut first = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        assert!(
            negotiate_capacity_fixture(&mut first, "event-first")
                .await
                .0
                .is_ok()
        );
        // The same stream key again while the first is live: refused.
        let mut second = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        let (result, wire, done) = negotiate_capacity_fixture(&mut second, "event-first").await;
        assert!(
            matches!(result, Err(sapflow_rtmp::client::ClientError::Refused(code)) if code == "NetStream.Publish.BadName")
        );
        assert!(done && second.publication.is_none());
        assert!(
            wire.windows(b"already live".len())
                .any(|bytes| bytes == b"already live")
        );
        // Once the first has sent nothing for the silence, it is taken over.
        fixture
            .app
            .streams
            .get("event-first")
            .expect("live stream")
            .session()
            .backdate_input(std::time::Duration::from_secs(4));
        let mut third = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        assert!(
            negotiate_capacity_fixture(&mut third, "event-first")
                .await
                .0
                .is_ok()
        );
        third.publication.take().expect("took over").end();
        if let Some(publication) = first.publication.take() {
            publication.end();
        }
    }

    #[tokio::test]
    async fn resident_capacity_refusal_is_visible_on_the_actual_rtmp_wire() {
        let mut fixture = crate::publish::loss_tests::Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.recordings = None;
        app.streams = Arc::new(crate::stream::Registry::new(1));
        let from = "127.0.0.1".parse().expect("loopback");
        let mut first = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        assert!(
            negotiate_capacity_fixture(&mut first, "event-first")
                .await
                .0
                .is_ok()
        );
        let mut refused = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        let (result, wire, done) = negotiate_capacity_fixture(&mut refused, "event-next").await;
        assert!(
            matches!(result, Err(sapflow_rtmp::client::ClientError::Refused(code)) if code == "NetStream.Publish.Denied")
        );
        assert!(done && refused.publication.is_none());
        assert!(
            wire.windows(b"presentation limit".len())
                .any(|bytes| bytes == b"presentation limit")
        );
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 1);
        first.publication.take().expect("accepted first").end();
        let mut recovered = Conn {
            app: &fixture.app,
            from,
            publication: None,
        };
        assert!(
            negotiate_capacity_fixture(&mut recovered, "event-next")
                .await
                .0
                .is_ok()
        );
        recovered
            .publication
            .take()
            .expect("accepted after final release")
            .end();
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 0);
        assert!(fixture.callbacks.is_empty());
    }

    /// A PROXY protocol v2 header for a TCP/IPv4 connection from `source`
    /// to a front on 192.0.2.1:443.
    fn proxy_v2(source: SocketAddr) -> Vec<u8> {
        let SocketAddr::V4(v4) = source else {
            panic!("an IPv4 fixture")
        };
        let mut header = proxy_protocol::SIGNATURE.to_vec();
        header.extend_from_slice(&[0x21, 0x11, 0, 12]);
        header.extend_from_slice(&v4.ip().octets());
        header.extend_from_slice(&[192, 0, 2, 1]);
        header.extend_from_slice(&v4.port().to_be_bytes());
        header.extend_from_slice(&443u16.to_be_bytes());
        header
    }

    /// LOCAL: the front's own connection, with no addresses.
    fn proxy_v2_local() -> Vec<u8> {
        let mut header = proxy_protocol::SIGNATURE.to_vec();
        header.extend_from_slice(&[0x20, 0x00, 0, 0]);
        header
    }

    fn c0c1() -> Vec<u8> {
        let mut bytes = vec![3u8];
        bytes.resize(1 + 1536, 0);
        bytes
    }

    async fn proxied_listener(
        app: Arc<App>,
        trusted: &str,
    ) -> (SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("owned loopback listener");
        let address = listener.local_addr().expect("owned address");
        let front = Front::Proxied(trusted.parse().expect("front address"));
        let server = tokio::spawn(serve(app, listener, front, Arc::new(AtomicUsize::new(0))));
        (address, server)
    }

    /// Writes `prefix` and the client's first bytes in one segment, then
    /// runs the client over the socket until it is publishing.
    async fn publish_through(socket: &mut TcpStream, prefix: Vec<u8>, key: &str) -> bool {
        let mut client = Publisher::new(
            PublisherConfig::new("live".into(), key.into(), "rtmp://localhost/live".into()),
            11,
            0,
        )
        .expect("bounded client");
        let mut first = prefix;
        first.extend_from_slice(&client.take_output());
        socket.write_all(&first).await.expect("header and C0 C1");
        let mut buf = vec![0u8; 64 * 1024];
        for step in 0..64 {
            let n = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buf))
                .await
                .expect("the server answers")
                .expect("read");
            if n == 0 || client.feed(&buf[..n], step).is_err() {
                return false;
            }
            let out = client.take_output();
            if !out.is_empty() {
                socket.write_all(&out).await.expect("client bytes");
            }
            if client.state() == PublisherState::Publishing {
                return true;
            }
        }
        panic!("the handshake did not finish");
    }

    /// RTMPS through a front: the app's publish check sees the publisher's
    /// own address from the header, not the front's. The header arrives in
    /// two writes, and the handshake's first bytes in the same segment as
    /// its end.
    #[tokio::test]
    async fn a_proxied_publisher_is_known_by_the_address_in_its_header() {
        use crate::publish_auth::stub::{self, Answer};
        let stub = stub::start(Answer::Status(200)).await;
        let mut fixture = crate::publish::loss_tests::Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.recordings = None;
        app.config.publish_auth = Some(stub.endpoint.clone());
        let (address, server) = proxied_listener(fixture.app.clone(), "127.0.0.1").await;
        let mut socket = TcpStream::connect(address).await.expect("connect");
        let header = proxy_v2("203.0.113.7:50000".parse().expect("source"));
        let (start, rest) = header.split_at(7);
        socket
            .write_all(start)
            .await
            .expect("first part of the header");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let published = publish_through(
            &mut socket,
            rest.to_vec(),
            "event-one?user=encoder&pass=gig-secret",
        )
        .await;
        assert!(published);
        let sent = stub.last();
        assert_eq!(sent["ip"], "203.0.113.7");
        assert_eq!(sent["path"], "live/event-one");
        drop(socket);
        server.abort();
    }

    /// The failed-attempt throttle keys on the header's address: one
    /// throttled publisher behind the front does not lock out another, and
    /// the throttled one is refused before the app is asked.
    #[tokio::test]
    async fn the_throttle_keys_on_the_address_in_the_header() {
        use crate::publish_auth::stub::{self, Answer};
        let stub = stub::start(Answer::Status(200)).await;
        let mut fixture = crate::publish::loss_tests::Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.recordings = None;
        app.config.publish_auth = Some(stub.endpoint.clone());
        let noisy: SocketAddr = "203.0.113.7:50000".parse().expect("source");
        {
            let mut throttle = fixture.app.auth_throttle.lock().expect("throttle");
            for _ in 0..10 {
                throttle.failed(noisy.ip(), Instant::now().into_std());
            }
        }
        let (address, server) = proxied_listener(fixture.app.clone(), "127.0.0.1").await;
        let key = "event-one?user=encoder&pass=gig-secret";
        let mut socket = TcpStream::connect(address).await.expect("connect");
        assert!(
            !publish_through(&mut socket, proxy_v2(noisy), key).await,
            "the throttled address is refused"
        );
        assert_eq!(stub.count(), 0, "refused before the app is asked");
        let mut socket = TcpStream::connect(address).await.expect("connect");
        let other = "198.51.100.9:50001".parse().expect("source");
        assert!(
            publish_through(&mut socket, proxy_v2(other), key).await,
            "another publisher behind the same front is not throttled"
        );
        assert_eq!(stub.last()["ip"], "198.51.100.9");
        drop(socket);
        server.abort();
    }

    /// The proxied listener answers no handshake for a peer that is not the
    /// front, for LOCAL, for a connection without a header and for a version
    /// 1 header: each is closed with nothing sent back.
    #[tokio::test]
    async fn the_proxied_listener_answers_only_a_header_from_its_front() {
        let fixture = crate::publish::loss_tests::Fixture::new();
        let source: SocketAddr = "203.0.113.7:50000".parse().expect("source");
        let with = |mut prefix: Vec<u8>| {
            prefix.extend_from_slice(&c0c1());
            prefix
        };
        for (trusted, first, why) in [
            (
                "192.0.2.99",
                with(proxy_v2(source)),
                "a peer that is not the front",
            ),
            ("127.0.0.1", with(proxy_v2_local()), "LOCAL"),
            ("127.0.0.1", c0c1(), "plain RTMP without a header"),
            (
                "127.0.0.1",
                with(b"PROXY TCP4 203.0.113.7 192.0.2.1 50000 443\r\n".to_vec()),
                "a version 1 header",
            ),
        ] {
            let (address, server) = proxied_listener(fixture.app.clone(), trusted).await;
            let mut socket = TcpStream::connect(address).await.expect("connect");
            // The server may close first; what it sends back is the test.
            let _ = socket.write_all(&first).await;
            let mut s0 = [0u8; 1];
            let read = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut s0))
                .await
                .expect("closed promptly");
            assert!(
                matches!(read, Ok(0) | Err(_)),
                "{why}: nothing is answered, got {read:?}"
            );
            server.abort();
        }
    }

    /// A header that never completes ends the connection at its deadline.
    #[tokio::test]
    async fn an_unfinished_header_ends_at_the_setup_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("owned loopback listener");
        let address = listener.local_addr().expect("owned address");
        let mut client = TcpStream::connect(address).await.expect("connect");
        let (mut server, peer) = listener.accept().await.expect("accept");
        let header = proxy_v2("203.0.113.7:50000".parse().expect("source"));
        client
            .write_all(&header[..20])
            .await
            .expect("part of a header");
        let deadline = Instant::now() + Duration::from_millis(200);
        let trusted = "127.0.0.1".parse().expect("front");
        let result = proxy_header(&mut server, peer, trusted, deadline).await;
        assert!(matches!(result, Err(End::Idle)), "{result:?}");
    }
}
