use super::protocol::{ALPN, SERVER_NAME};
use anyhow::Result;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{sync::Arc, time::Duration};

pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Identity {
    /// # Errors
    /// Returns certificate or signing-key generation errors.
    pub fn generate() -> Result<Self> {
        let identity = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()])?;
        Ok(Self {
            cert: identity.cert.der().clone(),
            key: identity.signing_key.serialize_der().into(),
        })
    }

    pub(super) fn server(&self, client: CertificateDer<'static>) -> Result<quinn::ServerConfig> {
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(
                self.server_tls(Some(client), ALPN)?,
            )?,
        ));
        config.transport_config(Arc::new(transport()));
        Ok(config)
    }

    pub(super) fn server_tls(
        &self,
        client: Option<CertificateDer<'static>>,
        alpn: &[u8],
    ) -> Result<rustls::ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?;
        let builder = if let Some(client) = client {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(client)?;
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                provider,
            )
            .build()?;
            builder.with_client_cert_verifier(verifier)
        } else {
            // Local clients authenticate with the private descriptor token inside pinned TLS.
            builder.with_no_client_auth()
        };
        let mut tls =
            builder.with_single_cert(vec![self.cert.clone()], self.key.clone_key().into())?;
        tls.alpn_protocols = vec![alpn.to_vec()];
        Ok(tls)
    }

    pub(super) fn client(&self, server: CertificateDer<'static>) -> Result<quinn::ClientConfig> {
        let mut config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(self.client_tls(server, ALPN)?)?,
        ));
        config.transport_config(Arc::new(transport()));
        Ok(config)
    }

    pub(super) fn client_tls(
        &self,
        server: CertificateDer<'static>,
        alpn: &[u8],
    ) -> Result<rustls::ClientConfig> {
        let mut tls = pinned_client(server)?
            .with_client_auth_cert(vec![self.cert.clone()], self.key.clone_key().into())?;
        tls.alpn_protocols = vec![alpn.to_vec()];
        // Mutations are never sent as replayable early data.
        tls.enable_early_data = false;
        Ok(tls)
    }
}

pub(super) fn local_client(server: CertificateDer<'static>) -> Result<rustls::ClientConfig> {
    let mut config = pinned_client(server)?.with_no_client_auth();
    config.alpn_protocols = vec![super::protocol::LOCAL_ALPN.to_vec()];
    Ok(config)
}

fn pinned_client(
    server: CertificateDer<'static>,
) -> Result<rustls::ConfigBuilder<rustls::ClientConfig, rustls::client::WantsClientCert>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(server)?;
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_root_certificates(roots))
}

fn transport() -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config.max_concurrent_bidi_streams(64_u32.into());
    config.max_concurrent_uni_streams(0_u32.into());
    config.max_idle_timeout(Some(quinn::VarInt::from_u32(5_000).into()));
    config.keep_alive_interval(Some(Duration::from_secs(1)));
    config.stream_receive_window((2 * 1024 * 1024_u32).into());
    config.receive_window((16 * 1024 * 1024_u32).into());
    config
}
