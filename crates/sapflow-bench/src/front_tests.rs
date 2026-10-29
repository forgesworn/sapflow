//! End to end: the viewers against an in-process TLS front. A throwaway CA
//! signs the front's certificate, the front serves a small fixed LL-HLS
//! stream over HTTP/2 or HTTP/1.1 as ALPN picks, and records what arrived:
//! which HTTP version, how many connections, how many requests at once on
//! one connection.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::ServerConfig;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::*;

/// How long the front holds a blocking playlist reload, as a real one waits
/// for the next part. The viewer asks for the part at the same moment, so the
/// two overlap.
const HOLD: Duration = Duration::from_millis(200);

const MASTER: &str = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2628000\nstream.m3u8\n";
const MEDIA: &str = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:3\n#EXTINF:1.0,\nseg3.mp4\n\
                     #EXT-X-PART:DURATION=0.2,URI=\"part1.mp4\"\n\
                     #EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part2.mp4\"\n";

/// A CA and, for 127.0.0.1, a certificate it signed.
struct Pki {
    ca_pem: String,
    server: ServerConfig,
}

fn pki(alpn: &[&[u8]]) -> Pki {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "sapflow-bench test CA");
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("CA key"))
        .expect("CA certificate");
    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .expect("leaf params")
        .signed_by(&leaf_key, &ca)
        .expect("leaf certificate");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
        )
        .expect("server certificate");
    server.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Pki {
        ca_pem: ca.pem(),
        server,
    }
}

/// What the front saw.
#[derive(Default)]
struct Seen {
    /// TLS connections that completed their handshake.
    connections: AtomicUsize,
    /// Most requests in flight at once on any one connection.
    max_in_flight: AtomicUsize,
    versions: Mutex<BTreeSet<&'static str>>,
}

impl Seen {
    fn versions(&self) -> Vec<&'static str> {
        let v = self.versions.lock().unwrap_or_else(|e| e.into_inner());
        v.iter().copied().collect()
    }
}

async fn handle(
    req: Request<Incoming>,
    seen: Arc<Seen>,
    in_flight: Arc<AtomicUsize>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    seen.max_in_flight.fetch_max(now, Ordering::SeqCst);
    seen.versions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(version_label(req.version()));
    let path = req.uri().path();
    let body = if path.ends_with("/index.m3u8") {
        Bytes::from_static(MASTER.as_bytes())
    } else if path.ends_with("/stream.m3u8") {
        if req.uri().query().is_some_and(|q| q.contains("_HLS_msn")) {
            tokio::time::sleep(HOLD).await;
        }
        Bytes::from_static(MEDIA.as_bytes())
    } else {
        Bytes::from(vec![7u8; 20_000])
    };
    in_flight.fetch_sub(1, Ordering::SeqCst);
    Ok(Response::new(Full::new(body)))
}

async fn serve(listener: TcpListener, tls: ServerConfig, seen: Arc<Seen>) {
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let acceptor = acceptor.clone();
        let seen = seen.clone();
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(tcp).await else {
                return;
            };
            seen.connections.fetch_add(1, Ordering::SeqCst);
            let h2 = stream.get_ref().1.alpn_protocol() == Some(b"h2");
            let in_flight = Arc::new(AtomicUsize::new(0));
            let service = service_fn(move |req| handle(req, seen.clone(), in_flight.clone()));
            let io = TokioIo::new(stream);
            if h2 {
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await;
            } else {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            }
        });
    }
}

/// Runs `viewers` viewers for a second against a front offering `alpn`,
/// trusting `trust_pem` (the front's own CA unless a test says otherwise).
async fn run_against(
    alpn: &[&[u8]],
    http2: bool,
    viewers: usize,
    trust_pem: impl FnOnce(&Pki) -> String,
) -> (Stats, Arc<Seen>) {
    let (out, seen) = run_cohorts(alpn, http2, viewers, None, trust_pem).await;
    (out.normal, seen)
}

/// As `run_against`, with `slow` of the viewers paced to `rate` bytes a second.
async fn run_cohorts(
    alpn: &[&[u8]],
    http2: bool,
    viewers: usize,
    slow: Option<(usize, u64)>,
    trust_pem: impl FnOnce(&Pki) -> String,
) -> (Outcome, Arc<Seen>) {
    let pki = pki(alpn);
    let trusted = trust_pem(&pki);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind");
    let port = listener.local_addr().expect("address").port();
    let seen = Arc::new(Seen::default());
    let front = tokio::spawn(serve(listener, pki.server, seen.clone()));
    let ca = std::env::temp_dir().join(format!(
        "sapflow-bench-test-{}-{port}.pem",
        std::process::id()
    ));
    std::fs::write(&ca, trusted).expect("write the CA file");
    let a = Args {
        url: format!("https://127.0.0.1:{port}/live/event-t/index.m3u8"),
        viewers,
        secs: 1,
        ramp: 0,
        full: None,
        ca: Some(ca.to_string_lossy().into_owned()),
        http2,
        slow: slow.map_or(0, |(n, _)| n),
        slow_rate: slow.map(|(_, rate)| rate),
    };
    let stats = run(&a).await;
    let _ = std::fs::remove_file(&ca);
    front.abort();
    (stats.expect("the run starts"), seen)
}

fn own_ca(p: &Pki) -> String {
    p.ca_pem.clone()
}

#[test]
fn reads_a_ca_file() {
    let roots = tls::roots_from_pem(pki(&[]).ca_pem.as_bytes()).expect("the CA parses");
    assert_eq!(roots.len(), 1);
}

#[tokio::test]
async fn http2_gives_each_viewer_one_connection_for_all_its_requests() {
    let (st, seen) = run_against(&[b"h2"], true, 3, own_ca).await;
    assert_eq!(st.errors, 0, "{:?}", st.error_kinds);
    assert!(st.requests >= 15, "viewers ran: {} requests", st.requests);
    // The viewers followed the stream: blocking reloads and parts both got through.
    assert!(!st.playlist_updates.is_empty() && !st.parts.is_empty());
    assert_eq!(protocols(&st), format!("HTTP/2 {}", st.requests));
    assert_eq!(seen.versions(), ["HTTP/2"]);
    // One connection per viewer, as the front counts them and as the report does.
    assert_eq!(seen.connections.load(Ordering::SeqCst), 3);
    assert_eq!(st.connections.len(), 3);
    // The blocking reload and the part fetch shared a connection at the same time.
    assert!(seen.max_in_flight.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn https_without_http2_speaks_http11_and_needs_a_second_connection() {
    let (st, seen) = run_against(&[b"h2", b"http/1.1"], false, 3, own_ca).await;
    assert_eq!(st.errors, 0, "{:?}", st.error_kinds);
    assert_eq!(protocols(&st), format!("HTTP/1.1 {}", st.requests));
    assert_eq!(seen.versions(), ["HTTP/1.1"]);
    // HTTP/1.1 cannot overlap a reload and a part on one connection.
    assert!(
        st.connections.len() > 3,
        "{} connections",
        st.connections.len()
    );
    assert_eq!(seen.max_in_flight.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http2_is_required_not_assumed() {
    let (st, _) = run_against(&[b"http/1.1"], true, 2, own_ca).await;
    assert_eq!(st.errors, 2, "{:?}", st.error_kinds);
    assert!(st.versions.is_empty(), "nothing is served over a fallback");
    assert!(
        st.error_kinds.keys().all(|k| k.contains("client error")),
        "{:?}",
        st.error_kinds
    );
}

#[tokio::test]
async fn a_certificate_from_an_untrusted_ca_is_refused() {
    let other = pki(&[]).ca_pem;
    let (st, seen) = run_against(&[b"h2"], true, 2, |_| other).await;
    assert_eq!(st.errors, 2, "{:?}", st.error_kinds);
    assert!(st.versions.is_empty());
    assert_eq!(seen.connections.load(Ordering::SeqCst), 0);
    assert!(
        st.error_kinds
            .keys()
            .all(|k| k.contains("invalid peer certificate")),
        "{:?}",
        st.error_kinds
    );
}

#[tokio::test]
async fn a_missing_ca_file_is_an_error_not_a_panic() {
    let a = Args {
        url: "https://127.0.0.1:9/live/x/index.m3u8".into(),
        viewers: 1,
        secs: 1,
        ramp: 0,
        full: None,
        ca: Some("/nonexistent/ca.pem".into()),
        http2: true,
        slow: 0,
        slow_rate: None,
    };
    let e = run(&a).await.err().expect("no such file");
    assert!(e.contains("--ca"), "{e}");
}

#[tokio::test]
async fn slow_viewers_read_paced_and_are_counted_apart_from_the_others() {
    // The front sends 20 000 bytes for a part or segment: 0.2 s at this rate.
    let (out, seen) = run_cohorts(&[b"h2"], true, 3, Some((1, 100_000)), own_ca).await;
    let (normal, slow) = (out.normal, out.slow);
    assert_eq!(normal.errors, 0, "{:?}", normal.error_kinds);
    assert_eq!(slow.errors, 0, "{:?}", slow.error_kinds);
    // One connection per viewer: two ordinary, one slow, none shared.
    assert_eq!(seen.connections.load(Ordering::SeqCst), 3);
    assert_eq!((normal.connections.len(), slow.connections.len()), (2, 1));
    assert!(normal.requests > 0 && slow.requests > 0);
    // The slow viewer got its media, no faster than the rate allows.
    assert!(!slow.segment.is_empty() && !slow.parts.is_empty());
    assert!(
        slow.segment
            .iter()
            .all(|t| *t >= Duration::from_millis(200)),
        "{:?}",
        slow.segment
    );
    // The other viewers are measured on their own, with no pace set for them.
    assert!(!normal.segment.is_empty() && !normal.parts.is_empty());
}
