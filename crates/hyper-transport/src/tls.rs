//! Mutual TLS 1.3 for the transport's connections (mantle note 32 T49; node.md §3.2, §3.6;
//! focal `transport.rs` `server_tls`, `client_tls`).
//!
//! Both sides present a certificate from the deployment's authority and verify the other's against
//! it. TLS 1.3 only, as QUIC requires (RFC 9001 §4.2). 0-RTT is off on both sides: "Disabling 0-RTT
//! entirely is the most effective defense against replay attack", and a first message may be a
//! mutation (node.md §3.2). Connections from one configuration resume TLS sessions without early
//! data, which skips the certificate chain with no replay exposure.

use hyper_quic::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use hyper_quic::rustls::{self, RootCertStore, server::WebPkiClientVerifier};
use hyper_quic::{ClientConfig, ServerConfig, TransportConfig};

pub use hyper_quic::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::Refusal;

/// The application protocol both sides negotiate (RFC 7301): a peer that speaks another is
/// refused at the handshake.
pub const ALPN: &[u8] = b"hyper-transport/1";

/// A node's credentials: its certificate chain, its private key, and the authority's roots its
/// peers' certificates are verified against.
pub struct Credentials {
    /// The node's chain, its own certificate first.
    pub chain: Vec<CertificateDer<'static>>,
    /// The node's private key.
    pub key: PrivateKeyDer<'static>,
    /// The roots peers' certificates chain to.
    pub roots: Vec<CertificateDer<'static>>,
}

fn provider() -> &'static rustls::crypto::CryptoProvider {
    &rustls::crypto::aws_lc_rs::DEFAULT_PROVIDER
}

fn roots(certificates: &[CertificateDer<'static>]) -> Result<RootCertStore, Refusal> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates {
        roots
            .add(certificate.clone())
            .map_err(|_| Refusal::Configuration)?;
    }
    if roots.is_empty() {
        return Err(Refusal::Configuration);
    }
    Ok(roots)
}

/// The server side: present `credentials` and require a client certificate under its roots.
pub fn server(
    credentials: &Credentials,
    transport: TransportConfig,
) -> Result<ServerConfig, Refusal> {
    let verifier =
        WebPkiClientVerifier::builder_with_provider(roots(&credentials.roots)?, provider())
            .build()
            .map_err(|_| Refusal::Configuration)?;
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Refusal::Configuration)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(credentials.chain.clone(), credentials.key.clone_key())
        .map_err(|_| Refusal::Configuration)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.max_early_data_size = 0;
    let crypto = QuicServerConfig::try_from(tls).map_err(|_| Refusal::Configuration)?;
    let mut config = ServerConfig::with_crypto(Box::new(crypto));
    config.transport_config(transport);
    Ok(config)
}

/// The client side: present `credentials` and verify the server's certificate under its roots.
pub fn client(
    credentials: &Credentials,
    transport: TransportConfig,
) -> Result<ClientConfig, Refusal> {
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Refusal::Configuration)?
        .with_root_certificates(roots(&credentials.roots)?)
        .with_client_auth_cert(credentials.chain.clone(), credentials.key.clone_key())
        .map_err(|_| Refusal::Configuration)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.enable_early_data = false;
    let crypto = QuicClientConfig::try_from(tls).map_err(|_| Refusal::Configuration)?;
    let mut config = ClientConfig::new(Box::new(crypto));
    config.transport_config(transport);
    Ok(config)
}
