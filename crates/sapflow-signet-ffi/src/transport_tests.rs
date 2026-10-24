//! Actual production client-builder transport, generated literal-loopback peers.
//! No relay/signer/account/certificate store or global TLS policy is changed.
use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    },
};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}
async fn receive(frames: Vec<Message>) -> Response<Vec<u8>> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("own loopback");
    let url = Url::parse(&format!(
        "ws://{}/",
        listener.local_addr().expect("address")
    ))
    .expect("URL");
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("connection");
        let mut socket = tokio_tungstenite::accept_async(socket)
            .await
            .expect("generated handshake");
        for frame in frames {
            if socket.send(frame).await.is_err() {
                break;
            }
        }
        let _ = socket.close(None).await;
    });
    let builder = client_builder();
    assert_eq!(builder.max_relays.map(NonZeroUsize::get), Some(4));
    assert_eq!(builder.notification_channel_size.get(), 16);
    let (_, mut incoming) = builder
        .websocket_transport
        .connect(&url, None)
        .await
        .map_err(|_| SignetError::Unavailable)?;
    let message = tokio::time::timeout(Duration::from_secs(2), incoming.next())
        .await
        .expect("bounded read");
    let result = match message {
        Some(Ok(message)) => message
            .as_text()
            .map(|text| text.as_bytes().to_vec())
            .ok_or(SignetError::Unavailable),
        _ => Err(SignetError::Unavailable),
    };
    drop(incoming);
    tokio::time::timeout(Duration::from_secs(2), peer)
        .await
        .expect("peer cleanup")
        .expect("peer task");
    result
}
#[test]
fn configured_production_path_accepts_normal_and_exact_bound_messages() {
    runtime().block_on(async {
        assert_eq!(
            receive(vec![Message::Text("generated".into())])
                .await
                .expect("normal"),
            b"generated"
        );
        assert_eq!(
            receive(vec![Message::Binary(vec![0x42; 16_384].into())])
                .await
                .expect("exact bound")
                .len(),
            16_384
        );
    });
}
#[test]
fn fragmented_total_message_limit_applies_even_when_each_frame_fits() {
    runtime().block_on(async {
        let first = Message::Frame(Frame::message(
            vec![b'x'; 8_192],
            OpCode::Data(Data::Text),
            false,
        ));
        let last = Message::Frame(Frame::message(
            vec![b'x'; 8_193],
            OpCode::Data(Data::Continue),
            true,
        ));
        assert!(receive(vec![first, last]).await.is_err());
    });
}
#[test]
fn oversized_frame_is_rejected_from_header_without_waiting_for_payload() {
    runtime().block_on(async {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("own loopback");
        let url = Url::parse(&format!(
            "ws://{}/",
            listener.local_addr().expect("address")
        ))
        .expect("URL");
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("connection");
            let mut socket = tokio_tungstenite::accept_async(socket)
                .await
                .expect("handshake");
            // An unmasked final binary frame advertising 16,385 bytes, with NO
            // payload. Rejection must happen before assembling that payload.
            socket
                .get_mut()
                .write_all(&[0x82, 126, 0x40, 0x01])
                .await
                .expect("header");
            let _ = tokio::time::timeout(Duration::from_secs(2), socket.next()).await;
        });
        let (outgoing, mut incoming) = client_builder()
            .websocket_transport
            .connect(&url, None)
            .await
            .expect("connected");
        let response = tokio::time::timeout(Duration::from_secs(1), incoming.next())
            .await
            .expect("reject header promptly");
        assert!(matches!(response, Some(Err(_))));
        drop((outgoing, incoming));
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .expect("peer cleanup")
            .expect("peer task");
    });
}
#[test]
fn configured_path_keeps_normal_tls_validation_and_rejects_self_signed_peer() {
    runtime().block_on(async {
        use tokio_rustls::{
            TlsAcceptor,
            rustls::{
                ServerConfig,
                pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
            },
        };
        // Generated specifically for this test, never a user or service key.
        let cert = CertificateDer::from(
            include_bytes!("../tests/fixtures/generated-local-cert.der").to_vec(),
        );
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            include_bytes!("../tests/fixtures/generated-local-key.der").to_vec(),
        ));
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("generated certificate");
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("own loopback");
        let url = Url::parse(&format!(
            "wss://{}/",
            listener.local_addr().expect("address")
        ))
        .expect("URL");
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("connection");
            let _ = TlsAcceptor::from(Arc::new(config)).accept(socket).await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client_builder().websocket_transport.connect(&url, None),
        )
        .await
        .expect("bounded TLS failure");
        assert!(
            result.is_err(),
            "generated self-signed certificate must not be trusted"
        );
        tokio::time::timeout(Duration::from_secs(2), peer)
            .await
            .expect("TLS cleanup")
            .expect("peer task");
    });
}

#[test]
fn configured_write_bound_refuses_oversized_work_without_changing_legacy_defaults() {
    // These remain upstream defaults; only the auth builder opts in.
    assert_eq!(
        ClientBuilder::default().notification_channel_size.get(),
        4096
    );
    assert_eq!(WebSocketConfig::default().max_message_size, Some(64 << 20));
    runtime().block_on(async {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("own loopback");
        let url = Url::parse(&format!(
            "ws://{}/",
            listener.local_addr().expect("address")
        ))
        .expect("URL");
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("connection");
            let mut socket = tokio_tungstenite::accept_async(socket)
                .await
                .expect("handshake");
            let message = socket.next().await;
            assert!(!matches!(message, Some(Ok(Message::Binary(_)))));
        });
        let (mut outgoing, incoming) = client_builder()
            .websocket_transport
            .connect(&url, None)
            .await
            .expect("connected");
        // Message type inferred from the actual configured SDK sink, rather
        // than bypassing it through the fixture's tungstenite implementation.
        let message = vec![b'x'; 32_769];
        let message = async_wsocket::Message::Binary(message);
        assert!(outgoing.send(message).await.is_err());
        drop((outgoing, incoming));
        tokio::time::timeout(Duration::from_secs(2), peer)
            .await
            .expect("peer cleanup")
            .expect("peer task");
    });
}
