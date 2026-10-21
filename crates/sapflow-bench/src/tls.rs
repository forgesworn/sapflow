//! TLS for the simulated viewers: rustls with the ring provider, trusting the
//! CAs in one PEM file, speaking HTTP/2 (ALPN `h2`) or HTTP/1.1 (ALPN
//! `http/1.1`) as asked, TLS 1.3 only.
//!
//! Every viewer builds its own client from the one shared [`ClientConfig`]:
//! one connection per viewer, as a player has. Over HTTP/2 that connection
//! carries all of the viewer's concurrent requests (the blocking playlist
//! reload and the part fetch) as streams, as Safari and hls.js do.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Empty;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::client::Resumption;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use rustls::{ClientConfig, RootCertStore};

pub type HttpsClient = Client<HttpsConnector<HttpConnector>, Empty<Bytes>>;

/// HTTP/2 receive windows. The 64 KiB default would make a viewer wait a
/// round trip for every 64 KiB of a segment, which a browser does not (Chrome
/// advertises megabytes), and that would show up as the front's delay.
const STREAM_WINDOW: u32 = 1 << 20;
const CONNECTION_WINDOW: u32 = 4 << 20;

/// The trust anchors in `pem`. Certificates that cannot serve as one are
/// skipped (a system bundle can hold odd ones); none usable is an error.
pub fn roots_from_pem(pem: &[u8]) -> Result<RootCertStore, String> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("not a PEM certificate file: {e}"))?;
    let mut roots = RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(certs);
    if added == 0 {
        return Err("no usable CA certificate in the file".into());
    }
    Ok(roots)
}

/// The viewers' TLS settings: trust `ca_pem` and nothing else.
pub fn client_config(ca_pem: &[u8]) -> Result<ClientConfig, String> {
    let roots = roots_from_pem(ca_pem)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    // Each simulated viewer is its own device: a full handshake each, with no
    // session resumption between them.
    config.resumption = Resumption::disabled();
    Ok(config)
}

/// One viewer's client. `http2`: ALPN offers `h2` only and the connection
/// speaks HTTP/2, so a front that cannot is an error rather than a quiet
/// fallback. Otherwise ALPN offers `http/1.1` only.
pub fn https_client(config: &ClientConfig, http2: bool) -> HttpsClient {
    let connector = HttpsConnectorBuilder::new()
        .with_tls_config(config.clone())
        .https_only();
    let mut client = Client::builder(TokioExecutor::new());
    if http2 {
        client
            .http2_only(true)
            .http2_initial_stream_window_size(STREAM_WINDOW)
            .http2_initial_connection_window_size(CONNECTION_WINDOW)
            .build(connector.enable_http2().build())
    } else {
        client
            .pool_max_idle_per_host(4)
            .build(connector.enable_http1().build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_files_without_certificates() {
        assert!(roots_from_pem(b"").is_err());
        assert!(roots_from_pem(b"hello\n").is_err());
        // A PEM file of another kind holds no certificate either.
        let key = b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n";
        assert!(roots_from_pem(key).is_err());
    }

    #[test]
    fn rejects_a_truncated_certificate() {
        let pem = b"-----BEGIN CERTIFICATE-----\nAAAA\n";
        assert!(roots_from_pem(pem).is_err());
    }
}
