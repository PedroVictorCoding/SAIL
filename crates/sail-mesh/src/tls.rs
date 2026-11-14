//! TLS for Mesh: the worker serves its self-signed certificate; the host accepts exactly
//! that certificate (by SHA-256 pin) and nothing else, not even publicly trusted ones.
use crate::{MeshError, Result};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, WebPkiSupportedAlgorithms},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use std::{sync::Arc, time::Duration};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts the one certificate whose SHA-256 matches the pin, and checks that the server
/// really holds its private key (the handshake signature).
#[derive(Debug)]
struct PinnedCert {
    fingerprint: String,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if crate::identity::fingerprint(end_entity.as_ref()) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "SAIL Mesh worker certificate does not match the paired fingerprint".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// An HTTPS client that talks only to the worker with this certificate and sends the
/// paired secret on every request. Proxies are never used: the worker is on the owner's
/// network and its traffic must not be diverted.
pub fn pinned_client(
    fingerprint: &str,
    secret: &str,
    connect_timeout: Duration,
) -> Result<reqwest::Client> {
    let provider = provider();
    let verifier = PinnedCert {
        fingerprint: fingerprint.to_ascii_lowercase(),
        algorithms: provider.signature_verification_algorithms,
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| MeshError::new(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let mut headers = reqwest::header::HeaderMap::new();
    let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {secret}"))
        .map_err(|_| MeshError::new("Malformed worker secret"))?;
    auth.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, auth);
    reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .default_headers(headers)
        .build()
        .map_err(|e| MeshError::new(e.to_string()))
}

/// TLS server configuration for a worker.
pub fn server_config(cert_der: &[u8], key_der: &[u8]) -> Result<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| MeshError::new(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der.to_vec())),
        )
        .map_err(|e| MeshError::new(format!("Worker certificate is unusable: {e}")))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}
