use crate::config::{Config, Scheme};
use std::{io, path::Path, sync::Arc};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};

const PEM_LIMIT: u64 = 1024 * 1024;

// These public test credentials make local startup reproducible, not suitable for shared hosts.
pub const DEVELOPMENT_CA_PEM: &str = include_str!("tls/development-ca.pem");
const DEVELOPMENT_CERT_PEM: &[u8] = include_bytes!("tls/development-cert.pem");
const DEVELOPMENT_KEY_PEM: &[u8] = include_bytes!("tls/development-key.pem");

fn read_pem(path: &Path) -> io::Result<Vec<u8>> {
    crate::bounded_file::read(path, PEM_LIMIT).map_err(|error| match error {
        crate::bounded_file::ReadError::Io(error) => error,
        crate::bounded_file::ReadError::Limit => {
            io::Error::new(io::ErrorKind::InvalidData, "TLS PEM file exceeds 1 MiB")
        }
    })
}

fn certificates(bytes: &[u8]) -> io::Result<Vec<CertificateDer<'static>>> {
    let certificates = CertificateDer::pem_slice_iter(bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    if certificates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TLS PEM file contains no certificates",
        ));
    }
    Ok(certificates)
}

pub fn load_server(config: &Config) -> io::Result<Option<TlsAcceptor>> {
    if config.scheme == Scheme::Http {
        if config.tls_cert.is_some() || config.tls_key.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TLS files require scheme=https or both",
            ));
        }
        return Ok(None);
    }
    let (cert, key) = match (&config.tls_cert, &config.tls_key) {
        (None, None) => (DEVELOPMENT_CERT_PEM.to_vec(), DEVELOPMENT_KEY_PEM.to_vec()),
        (Some(cert), Some(key)) => (read_pem(cert)?, read_pem(key)?),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tls_cert and tls_key must be supplied together",
            ));
        }
    };
    let mut keys = PrivateKeyDer::pem_slice_iter(&key)
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    if keys.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TLS PEM file must contain exactly one private key",
        ));
    }
    let mut server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates(&cert)?, keys.remove(0))
        .map_err(io::Error::other)?;
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Some(TlsAcceptor::from(Arc::new(server))))
}

pub fn client_config(ca_cert: &Path) -> io::Result<Arc<rustls::ClientConfig>> {
    client_config_from_pem(&read_pem(ca_cert)?)
}

pub fn development_client_config() -> io::Result<Arc<rustls::ClientConfig>> {
    client_config_from_pem(DEVELOPMENT_CA_PEM.as_bytes())
}

fn client_config_from_pem(pem: &[u8]) -> io::Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in certificates(pem)? {
        roots.add(certificate).map_err(io::Error::other)?;
    }
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
