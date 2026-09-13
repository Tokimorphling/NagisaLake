//! Scheme validation and additive private-CA TLS configuration.
use crate::TransportError;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::sync::Arc;
use tokio_tungstenite::{Connector, tungstenite::http::Uri};

/// TLS settings for a `wss://` Hub url.
///
/// Empty is the common case: a Hub behind a public certificate needs nothing
/// here, because the bundled webpki root store already trusts its issuer.
#[derive(Debug, Clone, Default)]
pub struct WorkerTlsConfig {
    /// PEM-encoded CA certificates to trust *in addition to* the built-in
    /// roots, for a Hub whose certificate comes from a private CA.
    ///
    /// These have to be certificate authorities. A self-signed server leaf
    /// without `basicConstraints: CA:TRUE` is not a usable trust anchor and
    /// webpki rejects the chain as having an unknown issuer.
    pub extra_root_certificates: Vec<Vec<u8>>,
}

/// Transport-level scheme of a Hub url.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectScheme {
    /// Cleartext WebSocket.
    Plain,
    /// WebSocket over TLS.
    Tls,
}

/// Classifies a Hub url, rejecting anything the worker cannot dial.
///
/// Worth doing before a connection is attempted: `ws`/`wss` are the only
/// schemes tungstenite dials, and it reports the rest as a generic url error
/// on every reconnect. An `https://` paste is a config mistake, and saying so
/// once at startup beats an obscure failure looping forever.
pub fn connect_scheme(url: &str) -> Result<ConnectScheme, TransportError> {
    let uri = url
        .trim()
        .parse::<Uri>()
        .map_err(|_| TransportError::UnsupportedUrl(url.trim().into()))?;
    match uri.scheme_str() {
        Some("ws") => Ok(ConnectScheme::Plain),
        Some("wss") => Ok(ConnectScheme::Tls),
        _ => Err(TransportError::UnsupportedUrl(url.trim().into())),
    }
}

/// Builds the TLS client config for a `wss://` Hub.
///
/// Always supplied rather than letting tungstenite construct its own, for one
/// reason: its default calls `ClientConfig::builder()`, which panics outright
/// when two crypto providers are compiled in and neither is installed as the
/// process default. That is one S3-SDK-shaped dependency away in a workspace
/// like this one, and it would surface as a crash on the first connection. The
/// provider is therefore named here, and the roots this returns are the same
/// public set tungstenite would have used, plus any private CA.
pub(super) fn tls_connector(config: &WorkerTlsConfig) -> Result<Connector, TransportError> {
    Ok(Connector::Rustls(Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| TransportError::Tls(error.to_string()))?
        .with_root_certificates(root_store(&config.extra_root_certificates)?)
        .with_no_client_auth(),
    )))
}

/// The public webpki roots plus every CA found in `bundles`.
///
/// Additive on purpose. A fleet is rarely uniform — one Hub behind a corporate
/// CA and another behind a public certificate is the normal case — so replacing
/// the public set would break the hubs that were working.
pub(super) fn root_store(bundles: &[Vec<u8>]) -> Result<rustls::RootCertStore, TransportError> {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    for pem in bundles {
        let mut added = 0usize;
        for certificate in CertificateDer::pem_slice_iter(pem) {
            let certificate =
                certificate.map_err(|error| TransportError::Tls(format!("{error:?}")))?;
            roots
                .add(certificate)
                .map_err(|error| TransportError::Tls(error.to_string()))?;
            added += 1;
        }
        // A file of the wrong kind parses to nothing at all rather than
        // failing. Silently trusting only the public roots would turn a
        // deployment mistake into a handshake failure much later, with nothing
        // pointing back here.
        if added == 0 {
            return Err(TransportError::Tls(
                "a configured CA bundle contains no certificates".into(),
            ));
        }
    }
    Ok(roots)
}
