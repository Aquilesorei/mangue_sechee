//! TLS utilities for Manguesechee.
//!
//! Provides automatic self-signed certificate generation/loading,
//! and server/client TLS configuration using rustls and ring.

use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Ensure default crypto provider is installed for rustls.
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Compute SHA-256 fingerprint of a DER certificate.
pub fn cert_fingerprint(cert_der: &[u8]) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA256, cert_der);
    hash.as_ref()
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(":")
}

/// Paths to TLS certificate and key in config directory.
pub fn tls_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("manguesechee")
        .join("tls")
}

pub fn cert_path() -> PathBuf {
    tls_dir().join("cert.der")
}

pub fn key_path() -> PathBuf {
    tls_dir().join("key.der")
}

/// Load existing TLS certificate and private key, or generate a fresh self-signed pair.
pub fn load_or_generate_identity(local_name: &str) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    init_crypto();
    let c_path = cert_path();
    let k_path = key_path();

    if c_path.exists() && k_path.exists() {
        let cert_bytes = std::fs::read(&c_path).with_context(|| format!("read {}", c_path.display()))?;
        let key_bytes = std::fs::read(&k_path).with_context(|| format!("read {}", k_path.display()))?;
        let cert_der = CertificateDer::from(cert_bytes);
        let key_der = PrivateKeyDer::try_from(key_bytes).map_err(|e| anyhow::anyhow!("invalid private key: {e:?}"))?;
        return Ok((vec![cert_der], key_der));
    }

    info!("generating self-signed TLS certificate for '{local_name}'");
    let sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "manguesechee.local".to_string(),
        local_name.to_string(),
    ];
    let cert = rcgen::generate_simple_self_signed(sans)
        .context("generate self-signed cert")?;

    let cert_der_vec = cert.cert.der().to_vec();
    let key_der_vec = cert.key_pair.serialize_der();

    if let Some(parent) = c_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&c_path, &cert_der_vec).with_context(|| format!("write {}", c_path.display()))?;
    std::fs::write(&k_path, &key_der_vec).with_context(|| format!("write {}", k_path.display()))?;

    let fingerprint = cert_fingerprint(&cert_der_vec);
    info!("new TLS certificate created: fingerprint = {fingerprint}");

    let cert_der = CertificateDer::from(cert_der_vec);
    let key_der = PrivateKeyDer::try_from(key_der_vec).map_err(|e| anyhow::anyhow!("invalid private key: {e:?}"))?;
    Ok((vec![cert_der], key_der))
}

/// Build rustls server configuration for incoming connections.
pub fn create_server_config(cert_chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> anyhow::Result<Arc<rustls::ServerConfig>> {
    init_crypto();
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .context("configure TLS server")?;
    Ok(Arc::new(server_config))
}

/// Custom certificate verifier for local LAN connections (Trust-On-First-Use / Self-Signed).
#[derive(Debug)]
pub struct LanCertVerifier;

impl ServerCertVerifier for LanCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = cert_fingerprint(end_entity.as_ref());
        info!("TLS peer server certificate fingerprint: {fp}");
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build rustls client configuration for outbound connections.
pub fn create_client_config() -> anyhow::Result<Arc<rustls::ClientConfig>> {
    init_crypto();
    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(LanCertVerifier))
        .with_no_client_auth();
    Ok(Arc::new(client_config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{TcpTransport, Transport};
    use manguesechee_core::protocol::Message;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn test_tls_handshake_and_exchange() -> anyhow::Result<()> {
        let (certs, key) = load_or_generate_identity("test-node")?;
        let server_config = create_server_config(certs, key)?;
        let client_config = create_client_config()?;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut transport = TcpTransport::new(tcp);
            transport.upgrade_to_tls_server(server_config).await.unwrap();
            let msg = transport.receive().await.unwrap();
            assert!(matches!(msg, Message::Ping));
            transport.send(&Message::Pong).await.unwrap();
        });

        let client_tcp = TcpStream::connect(addr).await?;
        let mut client_transport = TcpTransport::new(client_tcp);
        client_transport.upgrade_to_tls_client(client_config, "localhost").await?;
        assert!(client_transport.is_tls());

        client_transport.send(&Message::Ping).await?;
        let resp = client_transport.receive().await?;
        assert!(matches!(resp, Message::Pong));

        server_task.await.unwrap();
        Ok(())
    }
}
