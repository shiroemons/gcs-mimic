use crate::config::{Config, Scheme};
use crate::diagnostics::{Diagnostic, DiagnosticSource};
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

#[derive(Debug)]
pub enum ServerIdentityError {
    Certificate(io::Error),
    PrivateKey(io::Error),
    Identity(io::Error),
}

impl ServerIdentityError {
    pub fn setting(&self) -> &'static str {
        match self {
            Self::Certificate(_) | Self::Identity(_) => "tls_cert",
            Self::PrivateKey(_) => "tls_key",
        }
    }

    pub fn repair(&self) -> &'static str {
        match self {
            Self::Certificate(_) => {
                "provide a readable PEM certificate chain in tls_cert, at most 1 MiB, matching tls_key"
            }
            Self::PrivateKey(_) => {
                "provide exactly one readable PEM private key in tls_key, at most 1 MiB, matching tls_cert"
            }
            Self::Identity(_) => {
                "provide readable matching PEM tls_cert/tls_key files, each at most 1 MiB, with a certificate chain and exactly one private key; use scheme=auto, https or both"
            }
        }
    }

    fn cause(&self) -> &io::Error {
        match self {
            Self::Certificate(error) | Self::PrivateKey(error) | Self::Identity(error) => error,
        }
    }

    /// The startup diagnostic for this failure: the setting to fix, the cause and the repair.
    pub fn diagnostic(&self) -> Diagnostic {
        Diagnostic::new(
            DiagnosticSource::Tls,
            self.setting(),
            format!("Cannot load TLS identity: {self}; {}", self.repair()),
        )
    }
}

impl std::fmt::Display for ServerIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause().fmt(f)
    }
}

impl std::error::Error for ServerIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause())
    }
}

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

/// How a TCP listener secures the connections it accepts.
#[derive(Clone)]
pub enum ListenerSecurity {
    /// Plain HTTP only.
    Plain,
    /// TLS only.
    Tls(TlsAcceptor),
    /// TLS or plain HTTP, as each connection's first byte selects.
    Detect(TlsAcceptor),
}

/// Loads the primary listener's security. A `scheme=both` HTTP listener is always
/// [`ListenerSecurity::Plain`].
pub fn load_server(config: &Config) -> Result<ListenerSecurity, ServerIdentityError> {
    let secure: fn(TlsAcceptor) -> ListenerSecurity = match config.scheme {
        Scheme::Http => {
            if config.tls_cert.is_some() || config.tls_key.is_some() {
                return Err(ServerIdentityError::Identity(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "TLS files require scheme=auto, https or both",
                )));
            }
            return Ok(ListenerSecurity::Plain);
        }
        // Validation refuses TLS files with a Unix socket, which is never inspected.
        Scheme::Auto if config.unix_socket.is_some() => return Ok(ListenerSecurity::Plain),
        Scheme::Auto => ListenerSecurity::Detect,
        Scheme::Https | Scheme::Both => ListenerSecurity::Tls,
    };
    let (cert, key) = match (&config.tls_cert, &config.tls_key) {
        (None, None) => (DEVELOPMENT_CERT_PEM.to_vec(), DEVELOPMENT_KEY_PEM.to_vec()),
        (Some(cert), Some(key)) => (
            read_pem(cert).map_err(ServerIdentityError::Certificate)?,
            read_pem(key).map_err(ServerIdentityError::PrivateKey)?,
        ),
        _ => {
            return Err(ServerIdentityError::Identity(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tls_cert and tls_key must be supplied together",
            )));
        }
    };
    let mut keys = PrivateKeyDer::pem_slice_iter(&key)
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)
        .map_err(ServerIdentityError::PrivateKey)?;
    if keys.len() != 1 {
        return Err(ServerIdentityError::PrivateKey(io::Error::new(
            io::ErrorKind::InvalidData,
            "TLS PEM file must contain exactly one private key",
        )));
    }
    let mut server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            certificates(&cert).map_err(ServerIdentityError::Certificate)?,
            keys.remove(0),
        )
        .map_err(io::Error::other)
        .map_err(ServerIdentityError::Identity)?;
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(secure(TlsAcceptor::from(Arc::new(server))))
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

#[cfg(test)]
mod tests;
