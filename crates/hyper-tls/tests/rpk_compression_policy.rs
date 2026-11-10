//! Certificate-type compression policy against unmodified rustls 0.23.45 peers.
//! Both implementations use the same Ed25519 fixtures, exact SPKI pins or the
//! fixture's real X509 roots, and their normal TLS 1.3 signature verification.
//! Byte/flight accounting includes encrypted TLS records, not plaintext crypto.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::disallowed_types,
    // This fixture's environment selects an expectation for a frozen source variant.
    clippy::disallowed_methods,
    missing_docs
)]

use hyper_measure::{alloc, cost};
use rustls_test::KeyType;
use std::io::{Read as _, Write as _};
use std::sync::Arc;

#[global_allocator]
static GLOBAL: alloc::Counting = alloc::Counting;
thread_local! {
    static SERVER_COMPRESSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CLIENT_COMPRESSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// The old/new trait adapters call the respective established verifier/codec;
// their differing ownership signatures are confined to this oracle fixture.
macro_rules! verification {
    ($tls:ident) => {
        #[derive(Debug)]
        pub(super) struct Pin {
            pub(super) key: Vec<u8>,
            pub(super) algorithms: $tls::crypto::WebPkiSupportedAlgorithms,
        }
        impl Pin {
            fn check(
                &self,
                key: &$tls::pki_types::CertificateDer<'_>,
                chain: &[$tls::pki_types::CertificateDer<'_>],
            ) -> Result<(), $tls::Error> {
                if key.as_ref() == self.key && chain.is_empty() {
                    Ok(())
                } else {
                    Err($tls::Error::InvalidCertificate(
                        $tls::CertificateError::ApplicationVerificationFailure,
                    ))
                }
            }
            fn signature(
                &self,
                message: &[u8],
                cert: &$tls::pki_types::CertificateDer<'_>,
                sig: &$tls::DigitallySignedStruct,
            ) -> Result<$tls::client::danger::HandshakeSignatureValid, $tls::Error> {
                $tls::crypto::verify_tls13_signature_with_raw_key(
                    message,
                    &$tls::pki_types::SubjectPublicKeyInfoDer::from(cert.as_ref()),
                    sig,
                    &self.algorithms,
                )
            }
        }
        impl $tls::client::danger::ServerCertVerifier for Pin {
            fn verify_server_cert(
                &self,
                key: &$tls::pki_types::CertificateDer<'_>,
                chain: &[$tls::pki_types::CertificateDer<'_>],
                _: &$tls::pki_types::ServerName<'_>,
                _: &[u8],
                _: $tls::pki_types::UnixTime,
            ) -> Result<$tls::client::danger::ServerCertVerified, $tls::Error> {
                self.check(key, chain)?;
                Ok($tls::client::danger::ServerCertVerified::assertion())
            }
            fn verify_tls12_signature(
                &self,
                _: &[u8],
                _: &$tls::pki_types::CertificateDer<'_>,
                _: &$tls::DigitallySignedStruct,
            ) -> Result<$tls::client::danger::HandshakeSignatureValid, $tls::Error> {
                Err($tls::Error::General("fixture is TLS 1.3 only".into()))
            }
            fn verify_tls13_signature(
                &self,
                message: &[u8],
                key: &$tls::pki_types::CertificateDer<'_>,
                sig: &$tls::DigitallySignedStruct,
            ) -> Result<$tls::client::danger::HandshakeSignatureValid, $tls::Error> {
                self.signature(message, key, sig)
            }
            fn supported_verify_schemes(&self) -> Vec<$tls::SignatureScheme> {
                self.algorithms.supported_schemes()
            }
            fn requires_raw_public_keys(&self) -> bool {
                true
            }
        }
        impl $tls::server::danger::ClientCertVerifier for Pin {
            fn root_hint_subjects(&self) -> &[$tls::DistinguishedName] {
                &[]
            }
            fn verify_client_cert(
                &self,
                key: &$tls::pki_types::CertificateDer<'_>,
                chain: &[$tls::pki_types::CertificateDer<'_>],
                _: $tls::pki_types::UnixTime,
            ) -> Result<$tls::server::danger::ClientCertVerified, $tls::Error> {
                self.check(key, chain)?;
                Ok($tls::server::danger::ClientCertVerified::assertion())
            }
            fn verify_tls12_signature(
                &self,
                _: &[u8],
                _: &$tls::pki_types::CertificateDer<'_>,
                _: &$tls::DigitallySignedStruct,
            ) -> Result<$tls::client::danger::HandshakeSignatureValid, $tls::Error> {
                Err($tls::Error::General("fixture is TLS 1.3 only".into()))
            }
            fn verify_tls13_signature(
                &self,
                message: &[u8],
                key: &$tls::pki_types::CertificateDer<'_>,
                sig: &$tls::DigitallySignedStruct,
            ) -> Result<$tls::client::danger::HandshakeSignatureValid, $tls::Error> {
                self.signature(message, key, sig)
            }
            fn supported_verify_schemes(&self) -> Vec<$tls::SignatureScheme> {
                self.algorithms.supported_schemes()
            }
            fn requires_raw_public_keys(&self) -> bool {
                true
            }
        }
        #[derive(Debug)]
        pub(super) struct Spy(pub(super) bool);
        impl $tls::compress::CertCompressor for Spy {
            fn compress(
                &self,
                input: Vec<u8>,
                level: $tls::compress::CompressionLevel,
            ) -> Result<Vec<u8>, $tls::compress::CompressionFailed> {
                if self.0 {
                    SERVER_COMPRESSIONS.with(|count| count.set(count.get() + 1));
                } else {
                    CLIENT_COMPRESSIONS.with(|count| count.set(count.get() + 1));
                }
                $tls::compress::BROTLI_COMPRESSOR.compress(input, level)
            }
            fn algorithm(&self) -> $tls::CertificateCompressionAlgorithm {
                $tls::CertificateCompressionAlgorithm::Brotli
            }
        }
        pub(super) static SERVER: Spy = Spy(true);
        pub(super) static CLIENT: Spy = Spy(false);
    };
}

mod new {
    use super::*;
    use hyper_tls as tls;
    verification!(tls);
    fn provider() -> &'static tls::crypto::CryptoProvider {
        static PROVIDER: std::sync::OnceLock<tls::crypto::CryptoProvider> =
            std::sync::OnceLock::new();
        PROVIDER.get_or_init(tls::crypto::aws_lc_rs::default_provider)
    }
    pub(super) fn client(
        server_rpk: bool,
        client_rpk: bool,
        offer: bool,
    ) -> rustls_test::TestClient {
        client_pinned(server_rpk, client_rpk, offer, false)
    }
    pub(super) fn client_pinned(
        server_rpk: bool,
        client_rpk: bool,
        offer: bool,
        reject: bool,
    ) -> rustls_test::TestClient {
        let kt = KeyType::Ed25519;
        let provider = provider();
        let builder = tls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&tls::version::TLS13])
            .unwrap();
        let builder = if server_rpk {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Box::new(Pin {
                    key: if reject {
                        kt.get_client_spki()
                    } else {
                        kt.get_spki()
                    }
                    .as_ref()
                    .to_vec(),
                    algorithms: provider.signature_verification_algorithms,
                }))
        } else {
            use tls::pki_types::pem::PemObject as _;
            let mut roots = tls::RootCertStore::empty();
            roots
                .add(
                    tls::pki_types::CertificateDer::from_pem_slice(include_bytes!(
                        "../test-ca/eddsa/ca.cert"
                    ))
                    .unwrap(),
                )
                .unwrap();
            builder.with_root_certificates(roots)
        };
        let mut config = if client_rpk {
            let signing = provider
                .key_provider
                .load_private_key(kt.get_client_key())
                .unwrap();
            builder.with_client_cert_resolver(Box::new(
                tls::client::AlwaysResolvesClientRawPublicKeys::new(tls::sign::CertifiedKey::new(
                    vec![tls::pki_types::CertificateDer::from(
                        kt.get_client_spki().as_ref().to_vec(),
                    )],
                    signing,
                )),
            ))
        } else {
            builder
                .with_client_auth_cert(kt.get_client_chain(), kt.get_client_key())
                .unwrap()
        };
        config.cert_compressors = vec![&CLIENT];
        if !offer {
            config.cert_decompressors.clear();
        }
        rustls_test::TestClient::new(config, "localhost".try_into().unwrap()).unwrap()
    }
    pub(super) fn server(
        server_rpk: bool,
        client_rpk: bool,
        offer: bool,
    ) -> rustls_test::TestServer {
        server_pinned(server_rpk, client_rpk, offer, false)
    }
    pub(super) fn server_pinned(
        server_rpk: bool,
        client_rpk: bool,
        offer: bool,
        reject: bool,
    ) -> rustls_test::TestServer {
        let kt = KeyType::Ed25519;
        let provider = provider();
        let verifier: Box<dyn tls::server::danger::ClientCertVerifier> = if client_rpk {
            Box::new(Pin {
                key: if reject {
                    kt.get_spki()
                } else {
                    kt.get_client_spki()
                }
                .as_ref()
                .to_vec(),
                algorithms: provider.signature_verification_algorithms,
            })
        } else {
            tls::server::WebPkiClientVerifier::builder_with_provider(
                rustls_test::get_client_root_store(kt),
                provider,
            )
            .build()
            .unwrap()
        };
        let builder = tls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&tls::version::TLS13])
            .unwrap()
            .with_client_cert_verifier(verifier);
        let mut config = if server_rpk {
            let signing = provider
                .key_provider
                .load_private_key(kt.get_key())
                .unwrap();
            builder.with_cert_resolver(Box::new(
                tls::server::AlwaysResolvesServerRawPublicKeys::new(tls::sign::CertifiedKey::new(
                    vec![tls::pki_types::CertificateDer::from(
                        kt.get_spki().as_ref().to_vec(),
                    )],
                    signing,
                )),
            ))
        } else {
            builder
                .with_single_cert(kt.get_chain(), kt.get_key())
                .unwrap()
        };
        config.cert_compressors = vec![&SERVER];
        if !offer {
            config.cert_decompressors.clear();
        }
        rustls_test::TestServer::new(config).unwrap()
    }
}

mod old {
    use super::*;
    use upstream_rustls as tls;
    verification!(tls);
    fn roots() -> tls::RootCertStore {
        use tls::pki_types::pem::PemObject as _;
        let mut roots = tls::RootCertStore::empty();
        roots
            .add(
                tls::pki_types::CertificateDer::from_pem_slice(include_bytes!(
                    "../test-ca/eddsa/ca.cert"
                ))
                .unwrap(),
            )
            .unwrap();
        roots
    }
    pub(super) fn client(server_rpk: bool, client_rpk: bool, offer: bool) -> tls::ClientConnection {
        let kt = KeyType::Ed25519;
        let provider = Arc::new(tls::crypto::aws_lc_rs::default_provider());
        let builder = tls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&tls::version::TLS13])
            .unwrap();
        let builder = if server_rpk {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Pin {
                    key: kt.get_spki().as_ref().to_vec(),
                    algorithms: provider.signature_verification_algorithms,
                }))
        } else {
            builder.with_root_certificates(roots())
        };
        let mut config = if client_rpk {
            let signing = provider
                .key_provider
                .load_private_key(kt.get_client_key())
                .unwrap();
            builder.with_client_cert_resolver(Arc::new(
                tls::client::AlwaysResolvesClientRawPublicKeys::new(Arc::new(
                    tls::sign::CertifiedKey::new(
                        vec![tls::pki_types::CertificateDer::from(
                            kt.get_client_spki().as_ref().to_vec(),
                        )],
                        signing,
                    ),
                )),
            ))
        } else {
            builder
                .with_client_auth_cert(kt.get_client_chain(), kt.get_client_key())
                .unwrap()
        };
        config.cert_compressors = vec![&CLIENT];
        if !offer {
            config.cert_decompressors.clear();
        }
        tls::ClientConnection::new(Arc::new(config), "localhost".try_into().unwrap()).unwrap()
    }
    pub(super) fn server(server_rpk: bool, client_rpk: bool, offer: bool) -> tls::ServerConnection {
        let kt = KeyType::Ed25519;
        let provider = Arc::new(tls::crypto::aws_lc_rs::default_provider());
        let verifier: Arc<dyn tls::server::danger::ClientCertVerifier> = if client_rpk {
            Arc::new(Pin {
                key: kt.get_client_spki().as_ref().to_vec(),
                algorithms: provider.signature_verification_algorithms,
            })
        } else {
            tls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots()),
                provider.clone(),
            )
            .build()
            .unwrap()
        };
        let builder = tls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&tls::version::TLS13])
            .unwrap()
            .with_client_cert_verifier(verifier);
        let mut config = if server_rpk {
            let signing = provider
                .key_provider
                .load_private_key(kt.get_key())
                .unwrap();
            builder.with_cert_resolver(Arc::new(
                tls::server::AlwaysResolvesServerRawPublicKeys::new(Arc::new(
                    tls::sign::CertifiedKey::new(
                        vec![tls::pki_types::CertificateDer::from(
                            kt.get_spki().as_ref().to_vec(),
                        )],
                        signing,
                    ),
                )),
            ))
        } else {
            builder
                .with_single_cert(kt.get_chain(), kt.get_key())
                .unwrap()
        };
        config.cert_compressors = vec![&SERVER];
        if !offer {
            config.cert_decompressors.clear();
        }
        tls::ServerConnection::new(Arc::new(config)).unwrap()
    }
}

enum Peer {
    NewClient(rustls_test::TestClient),
    NewServer(rustls_test::TestServer),
    OldClient(upstream_rustls::ClientConnection),
    OldServer(upstream_rustls::ServerConnection),
}
#[derive(Debug)]
enum TlsError {
    New(hyper_tls::Error),
    Old(upstream_rustls::Error),
    Io(std::io::Error),
    NoProgress,
    ChangedPayload,
}
impl std::fmt::Display for TlsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::New(error) => write!(formatter, "candidate: {error}"),
            Self::Old(error) => write!(formatter, "oracle: {error}"),
            Self::Io(error) => write!(formatter, "fixture IO: {error}"),
            Self::NoProgress => formatter.write_str("TLS stopped making progress"),
            Self::ChangedPayload => formatter.write_str("application payload changed"),
        }
    }
}
impl Peer {
    fn handshaking(&self) -> bool {
        match self {
            Self::NewClient(p) => p.is_handshaking(),
            Self::NewServer(p) => p.is_handshaking(),
            Self::OldClient(p) => p.is_handshaking(),
            Self::OldServer(p) => p.is_handshaking(),
        }
    }
    fn pending(&self) -> bool {
        match self {
            Self::NewClient(p) => p.wants_write(),
            Self::NewServer(p) => p.wants_write(),
            Self::OldClient(p) => p.wants_write(),
            Self::OldServer(p) => p.wants_write(),
        }
    }
    fn write(&mut self, out: &mut Vec<u8>) -> usize {
        match self {
            Self::NewClient(p) => p.write_tls(out).unwrap(),
            Self::NewServer(p) => p.write_tls(out).unwrap(),
            Self::OldClient(p) => p.write_tls(out).unwrap(),
            Self::OldServer(p) => p.write_tls(out).unwrap(),
        }
    }
    fn receive(&mut self, mut input: &[u8]) -> Result<(), TlsError> {
        while !input.is_empty() {
            let n = match self {
                Self::NewClient(p) => p.read_tls(&mut input),
                Self::NewServer(p) => p.read_tls(&mut input),
                Self::OldClient(p) => p.read_tls(&mut input),
                Self::OldServer(p) => p.read_tls(&mut input),
            }
            .map_err(TlsError::Io)?;
            if n == 0 {
                return Err(TlsError::NoProgress);
            }
        }
        match self {
            Self::NewClient(p) => p.process_new_packets().map(|_| ()).map_err(TlsError::New),
            Self::NewServer(p) => p.process_new_packets().map(|_| ()).map_err(TlsError::New),
            Self::OldClient(p) => p.process_new_packets().map(|_| ()).map_err(TlsError::Old),
            Self::OldServer(p) => p.process_new_packets().map(|_| ()).map_err(TlsError::Old),
        }
    }
    fn send_application(&mut self, bytes: &[u8]) {
        match self {
            Self::NewClient(p) => p.writer().write_all(bytes).unwrap(),
            Self::NewServer(p) => p.writer().write_all(bytes).unwrap(),
            Self::OldClient(p) => p.writer().write_all(bytes).unwrap(),
            Self::OldServer(p) => p.writer().write_all(bytes).unwrap(),
        }
    }
    fn read_application(&mut self, bytes: &mut [u8]) {
        match self {
            Self::NewClient(p) => p.reader().read_exact(bytes).unwrap(),
            Self::NewServer(p) => p.reader().read_exact(bytes).unwrap(),
            Self::OldClient(p) => p.reader().read_exact(bytes).unwrap(),
            Self::OldServer(p) => p.reader().read_exact(bytes).unwrap(),
        }
    }
}
fn transfer(from: &mut Peer, to: &mut Peer) -> Result<usize, TlsError> {
    let mut bytes = Vec::new();
    while from.pending() {
        if from.write(&mut bytes) == 0 {
            return Err(TlsError::NoProgress);
        }
    }
    let n = bytes.len();
    if n > 0 {
        to.receive(&bytes)?;
    }
    Ok(n)
}
fn handshake(mut client: Peer, mut server: Peer) -> Result<Vec<(usize, usize)>, TlsError> {
    let mut flights = Vec::new();
    while client.handshaking() || server.handshaking() || client.pending() || server.pending() {
        let up = transfer(&mut client, &mut server)?;
        let down = transfer(&mut server, &mut client)?;
        if up + down == 0 {
            return Err(TlsError::NoProgress);
        }
        flights.push((up, down));
    }
    let payload = b"exact\0\xffpayload";
    client.send_application(payload);
    transfer(&mut client, &mut server)?;
    let mut got = vec![0; payload.len()];
    server.read_application(&mut got);
    if got != payload {
        return Err(TlsError::ChangedPayload);
    }
    Ok(flights)
}
fn run_case(
    new_client: bool,
    new_server: bool,
    server_rpk: bool,
    client_rpk: bool,
    offer: bool,
) -> (usize, usize) {
    SERVER_COMPRESSIONS.set(0);
    CLIENT_COMPRESSIONS.set(0);
    let (flights, measured) = cost::measure(|| {
        let client = if new_client {
            Peer::NewClient(new::client(server_rpk, client_rpk, offer))
        } else {
            Peer::OldClient(old::client(server_rpk, client_rpk, offer))
        };
        let server = if new_server {
            Peer::NewServer(new::server(server_rpk, client_rpk, offer))
        } else {
            Peer::OldServer(old::server(server_rpk, client_rpk, offer))
        };
        handshake(client, server).unwrap()
    });
    let counts = (CLIENT_COMPRESSIONS.get(), SERVER_COMPRESSIONS.get());
    println!("case new_client={new_client} new_server={new_server} server_rpk={server_rpk} client_rpk={client_rpk} offer={offer} compressions={counts:?} flights={flights:?} cost={measured:?}");
    counts
}

#[test]
fn rpk_senders_avoid_compression_with_older_offering_peers() {
    assert!(alloc::installed());
    assert_eq!(run_case(true, false, true, true, true), (0, 0));
    assert_eq!(run_case(false, true, true, true, true), (0, 0));
}

#[test]
fn certificate_type_policy_preserves_exact_pins_against_old_peers() {
    for offer in [false, true] {
        for server_rpk in [false, true] {
            let client = Peer::OldClient(old::client(server_rpk, true, offer));
            let server = Peer::NewServer(new::server_pinned(server_rpk, true, offer, true));
            assert!(matches!(
                handshake(client, server),
                Err(TlsError::New(hyper_tls::Error::InvalidCertificate(
                    hyper_tls::CertificateError::ApplicationVerificationFailure
                )))
            ));
        }
        for client_rpk in [false, true] {
            let client = Peer::NewClient(new::client_pinned(true, client_rpk, offer, true));
            let server = Peer::OldServer(old::server(true, client_rpk, offer));
            assert!(matches!(
                handshake(client, server),
                Err(TlsError::New(hyper_tls::Error::InvalidCertificate(
                    hyper_tls::CertificateError::ApplicationVerificationFailure
                )))
            ));
        }
    }
}

#[test]
#[ignore = "explicit admitted complete certificate-type/new-old matrix"]
fn certificate_type_and_old_peer_matrix() {
    assert!(alloc::installed());
    let policy = std::env::var("HYPER_MATRIX_EXPECT").unwrap_or_else(|_| "both".into());
    let sender = matches!(policy.as_str(), "sender" | "both");
    let receiver = matches!(policy.as_str(), "receiver" | "both");
    assert!(matches!(
        policy.as_str(),
        "baseline" | "sender" | "receiver" | "both"
    ));
    println!("expected_source_policy={policy}");
    for new_client in [false, true] {
        for new_server in [false, true] {
            for server_rpk in [false, true] {
                for client_rpk in [false, true] {
                    for offer in [false, true] {
                        let counts =
                            run_case(new_client, new_server, server_rpk, client_rpk, offer);
                        let expect_client = expected_compression(
                            offer,
                            client_rpk,
                            sender && new_client,
                            receiver && new_server,
                        );
                        let expect_server = expected_compression(
                            offer,
                            server_rpk,
                            sender && new_server,
                            receiver && new_client,
                        );
                        assert_eq!(counts, (expect_client, expect_server));
                    }
                }
            }
        }
    }
}

fn expected_compression(
    offer: bool,
    rpk: bool,
    sender_policy: bool,
    receiver_policy: bool,
) -> usize {
    usize::from(offer && !(rpk && (sender_policy || receiver_policy)))
}
