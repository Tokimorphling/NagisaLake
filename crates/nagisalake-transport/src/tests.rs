use crate::{
    bridge::bridge as tungstenite_bridge,
    tls::{root_store, tls_connector},
    *,
};
use nagisalake_protocol::{HubMessage, Ping, Pong, WorkerMessage};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::PrivateKeyDer;
use std::sync::Arc;
use tokilake_core::tunnel::TunnelStream;
use tokilake_smux::{Config as SmuxConfig, Session as SmuxSession};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    net::TcpListener,
};
use tokio_tungstenite::tungstenite::{
    handshake::server::{ErrorResponse, Request, Response},
    http::header::SEC_WEBSOCKET_PROTOCOL,
};

struct TestStream(DuplexStream);

impl TunnelStream for TestStream {
    async fn read(
        &mut self,
        buffer: &mut [u8],
    ) -> Result<usize, tokilake_core::error::TunnelError> {
        Ok(AsyncReadExt::read(&mut self.0, buffer).await?)
    }

    async fn write(&mut self, buffer: &[u8]) -> Result<usize, tokilake_core::error::TunnelError> {
        Ok(AsyncWriteExt::write(&mut self.0, buffer).await?)
    }

    async fn flush(&mut self) -> Result<(), tokilake_core::error::TunnelError> {
        Ok(AsyncWriteExt::flush(&mut self.0).await?)
    }

    async fn close(&mut self) -> Result<(), tokilake_core::error::TunnelError> {
        Ok(AsyncWriteExt::shutdown(&mut self.0).await?)
    }
}

#[tokio::test]
async fn typed_control_messages_round_trip_over_a_tunnel_stream() {
    let (left, right) = tokio::io::duplex(16 * 1024);
    let mut worker = WorkerControl::new(TestStream(left), 1024).unwrap();
    let mut hub = HubControl::new(TestStream(right), 1024).unwrap();

    worker
        .send(&WorkerMessage::Pong(Pong {
            nonce: "one".into(),
        }))
        .await
        .unwrap();
    assert_eq!(
        hub.receive().await.unwrap(),
        Some(WorkerMessage::Pong(Pong {
            nonce: "one".into(),
        }))
    );

    hub.send(&HubMessage::Ping(Ping {
        nonce: "two".into(),
    }))
    .await
    .unwrap();
    assert_eq!(
        worker.receive().await.unwrap(),
        Some(HubMessage::Ping(Ping {
            nonce: "two".into(),
        }))
    );
}

#[test]
fn only_websocket_schemes_are_accepted() {
    assert_eq!(
        connect_scheme("ws://127.0.0.1:9091/v1/worker/connect").unwrap(),
        ConnectScheme::Plain
    );
    assert_eq!(
        connect_scheme("  wss://hub.example.com/v1/worker/connect  ").unwrap(),
        ConnectScheme::Tls
    );
    // The mistakes worth catching at startup: the scheme of the Hub's web
    // UI, the scheme of its API, and a url with no scheme at all.
    for url in [
        "https://hub.example.com/v1/worker/connect",
        "http://127.0.0.1:9091/v1/worker/connect",
        "hub.example.com/v1/worker/connect",
        "wss//hub.example.com",
    ] {
        assert!(
            matches!(connect_scheme(url), Err(TransportError::UnsupportedUrl(_))),
            "{url} should not be dialable"
        );
    }
}

#[test]
fn a_ca_bundle_that_decodes_to_nothing_is_rejected() {
    // Pointing at the wrong file is the likely mistake, and it decodes to
    // an empty set rather than an error. Accepting it would leave the
    // worker trusting only the public roots and blaming the certificate.
    let Err(error) = tls_connector(&WorkerTlsConfig {
        extra_root_certificates: vec![b"-----BEGIN CERTIFICATE-----\n".to_vec()],
    }) else {
        panic!("a bundle with no certificates must not build a connector");
    };
    assert!(matches!(error, TransportError::Tls(_)), "{error:?}");
}

#[test]
fn a_wss_config_is_built_even_with_no_private_ca() {
    // Not delegating to tungstenite's default is the point: its builder
    // panics when two crypto providers are compiled in, which the Hub's own
    // dependency graph already does.
    assert!(tls_connector(&WorkerTlsConfig::default()).is_ok());
    assert_eq!(
        root_store(&[]).unwrap().len(),
        webpki_roots::TLS_SERVER_ROOTS.len()
    );
}

#[test]
fn a_private_ca_joins_the_public_roots_instead_of_replacing_them() {
    let authority = TestAuthority::new();
    let roots = root_store(&[authority.ca_pem.clone().into_bytes()]).unwrap();
    // A fleet where one Hub uses a private CA and another a public issuer
    // has to keep verifying both.
    assert_eq!(roots.len(), webpki_roots::TLS_SERVER_ROOTS.len() + 1);
    assert!(
        tls_connector(&WorkerTlsConfig {
            extra_root_certificates: vec![authority.ca_pem.into_bytes()],
        })
        .is_ok()
    );
}

#[tokio::test]
async fn a_worker_completes_a_wss_handshake_against_a_private_ca() {
    let authority = TestAuthority::new();
    let hub = authority.serve().await;

    let mut transport = WorkerTransport::connect(WorkerConnectConfig {
        url: format!("wss://localhost:{}/v1/worker/connect", hub.port),
        tls: WorkerTlsConfig {
            extra_root_certificates: vec![authority.ca_pem.clone().into_bytes()],
        },
        ..WorkerConnectConfig::new("wss://replaced", "test-token")
    })
    .await
    .expect("the private CA should verify the hub certificate");

    // Prove the tunnel carries protocol frames, not just that TLS agreed:
    // the smux control stream is open on both sides past the handshake.
    transport
        .control_mut()
        .send(&WorkerMessage::Pong(Pong {
            nonce: "tls".into(),
        }))
        .await
        .unwrap();
    assert_eq!(
        hub.received.await.unwrap(),
        Some(WorkerMessage::Pong(Pong {
            nonce: "tls".into(),
        }))
    );
}

#[tokio::test]
async fn a_wss_handshake_fails_when_the_ca_is_not_trusted() {
    let authority = TestAuthority::new();
    let hub = authority.serve().await;

    // Same server, no extra root: the public store cannot vouch for this
    // certificate, so verification has to fail rather than fall through.
    let Err(error) = WorkerTransport::connect(WorkerConnectConfig {
        url: format!("wss://localhost:{}/v1/worker/connect", hub.port),
        ..WorkerConnectConfig::new("wss://replaced", "test-token")
    })
    .await
    else {
        panic!("an untrusted certificate must not be accepted");
    };
    assert!(
        matches!(&error, TransportError::WebSocket(source)
            if source.to_string().contains("certificate")),
        "expected certificate verification to fail, got {error:?}"
    );
}

#[tokio::test]
async fn extra_roots_on_a_cleartext_url_are_a_configuration_error() {
    let Err(error) = WorkerTransport::connect(WorkerConnectConfig {
        url: "ws://127.0.0.1:9091/v1/worker/connect".into(),
        tls: WorkerTlsConfig {
            extra_root_certificates: vec![TestAuthority::new().ca_pem.into_bytes()],
        },
        ..WorkerConnectConfig::new("ws://replaced", "test-token")
    })
    .await
    else {
        panic!("trust material on a ws:// url secures nothing");
    };
    assert!(
        matches!(error, TransportError::InvalidConfig(_)),
        "{error:?}"
    );
}

/// A throwaway CA and the `localhost` server certificate it issued.
///
/// Generated per test rather than committed: no private key lives in the
/// repository and there is nothing to expire and break the suite later.
struct TestAuthority {
    ca_pem: String,
    server: Arc<rustls::ServerConfig>,
}

/// A one-shot TLS + WebSocket + smux listener standing in for the Hub.
struct HubStub {
    port:     u16,
    received: tokio::task::JoinHandle<Option<WorkerMessage>>,
}

impl TestAuthority {
    fn new() -> Self {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "nagisalake test ca");
        // `CA:TRUE` plus `keyCertSign` is what makes this a usable trust
        // anchor. Without them webpki rejects the chain it signs, which is
        // the trap a self-signed server certificate falls into.
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let ca_pem = ca.pem();
        let issuer = Issuer::new(ca_params, ca_key);

        let server_key = KeyPair::generate().unwrap();
        let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        server_params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        server_params.use_authority_key_identifier_extension = true;
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server = server_params.signed_by(&server_key, &issuer).unwrap();

        // Same explicit provider as the client, for the same reason: under
        // Match the provider used by the S3 and HTTP clients explicitly.
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![server.der().clone()],
            PrivateKeyDer::Pkcs8(server_key.serialize_der().into()),
        )
        .unwrap();
        Self {
            ca_pem,
            server: Arc::new(config),
        }
    }

    async fn serve(&self) -> HubStub {
        // Port 0 so parallel tests never collide, and loopback so nothing
        // is reachable off the machine. The worker still dials `localhost`
        // — the name the certificate is issued for.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tokio_rustls::TlsAcceptor::from(self.server.clone());
        let received = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.ok()?;
            let tls = acceptor.accept(socket).await.ok()?;
            let websocket = tokio_tungstenite::accept_hdr_async(tls, negotiate_subprotocol)
                .await
                .ok()?;
            let (io, _bridge) = tungstenite_bridge(websocket);
            let mut session = SmuxSession::server(io, SmuxConfig::default());
            let stream = session.accept().await?;
            let mut control = HubControl::new(stream, DEFAULT_MAX_CONTROL_FRAME_BYTES).ok()?;
            control.receive().await.ok()?
        });
        HubStub { port, received }
    }
}

/// Echoes the Tokilake subprotocol back, as the real Hub handler does.
///
/// The worker drops any connection whose subprotocol was not confirmed, so a
/// stub that skipped this would fail for the wrong reason.
// The `Err` type is tungstenite's `ErrorResponse`, fixed by the `Callback`
// trait, so there is nothing here to make smaller.
#[allow(clippy::result_large_err)]
fn negotiate_subprotocol(
    _request: &Request,
    mut response: Response,
) -> Result<Response, ErrorResponse> {
    response.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        TOKILAKE_SUBPROTOCOL.parse().unwrap(),
    );
    Ok(response)
}
