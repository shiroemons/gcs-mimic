use crate::config::{Config, GrpcScheme, Scheme, Tls, TlsIdentity};
use crate::diagnostics::{Diagnostic, DiagnosticSource};
use std::{io, path::Path, sync::Arc};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self,
        crypto::CryptoProvider,
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
    /// rustls refused the pair, such as a key that does not match the certificate.
    Rejected(rustls::Error),
}

impl ServerIdentityError {
    pub fn setting(&self) -> &'static str {
        match self {
            Self::Certificate(_) | Self::Identity(_) | Self::Rejected(_) => "tls_cert",
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
                "provide readable matching PEM tls_cert/tls_key files, each at most 1 MiB, with a certificate chain and exactly one private key; use scheme=auto, https or both, or grpc_listen with grpc_scheme=auto or tls"
            }
            Self::Rejected(_) => {
                "provide a tls_key that matches the first certificate in tls_cert, as an RSA, ECDSA P-256 or P-384, or Ed25519 key"
            }
        }
    }

    fn cause(&self) -> &(dyn std::error::Error + 'static) {
        match self {
            Self::Certificate(error) | Self::PrivateKey(error) | Self::Identity(error) => error,
            Self::Rejected(error) => error,
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
        std::fmt::Display::fmt(self.cause(), f)
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

/// The ring provider, named explicitly: `ServerConfig::builder()` and `ClientConfig::builder()`
/// panic at runtime once the dependency graph enables a second rustls provider feature, because
/// neither can then pick a process default.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The application protocol a TLS listener negotiates through ALPN (RFC 7301). Each listener
/// offers exactly one. rustls completes a handshake without any protocol for a client that sends
/// no ALPN extension, so the bytes that follow decide, and refuses one that offers ALPN without
/// this protocol with a `no_application_protocol` alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Alpn {
    /// `http/1.1`, for the HTTP listeners, which speak HTTP/1 only: advertising `h2` there would
    /// select a protocol they cannot serve.
    Http1,
    /// `h2`, for the gRPC listener: gRPC clients refuse a TLS server that does not select it.
    H2,
}

impl Alpn {
    fn protocol(self) -> &'static [u8] {
        match self {
            Self::Http1 => b"http/1.1",
            Self::H2 => b"h2",
        }
    }
}

/// The certificate chain and private key the TLS listeners present, parsed once and shared by
/// every listener that serves TLS.
pub(crate) struct ServerIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl ServerIdentity {
    /// The configured identity: `tls_cert` and `tls_key`, or the public development identity
    /// when neither is set.
    pub(crate) fn load(identity: &TlsIdentity) -> Result<Self, ServerIdentityError> {
        match identity {
            TlsIdentity::Development => Self::from_pem(DEVELOPMENT_CERT_PEM, DEVELOPMENT_KEY_PEM),
            TlsIdentity::Provided { certificate, key } => Self::from_pem(
                &read_pem(certificate).map_err(ServerIdentityError::Certificate)?,
                &read_pem(key).map_err(ServerIdentityError::PrivateKey)?,
            ),
        }
    }

    fn from_pem(certificate: &[u8], key: &[u8]) -> Result<Self, ServerIdentityError> {
        let keys = PrivateKeyDer::pem_slice_iter(key)
            .collect::<Result<Vec<_>, _>>()
            .map_err(io::Error::other)
            .map_err(ServerIdentityError::PrivateKey)?;
        let Ok([key]) = <[PrivateKeyDer<'static>; 1]>::try_from(keys) else {
            return Err(ServerIdentityError::PrivateKey(io::Error::new(
                io::ErrorKind::InvalidData,
                "TLS PEM file must contain exactly one private key",
            )));
        };
        let identity = Self {
            chain: certificates(certificate).map_err(ServerIdentityError::Certificate)?,
            key,
        };
        // Building one configuration checks that the key matches the certificate, so a bad pair
        // fails here whichever listeners use it.
        identity.acceptor(Alpn::Http1)?;
        Ok(identity)
    }

    /// A TLS 1.2 and 1.3 acceptor that presents this identity and negotiates `alpn`.
    pub(crate) fn acceptor(&self, alpn: Alpn) -> Result<TlsAcceptor, ServerIdentityError> {
        let mut server = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(ServerIdentityError::Rejected)?
            .with_no_client_auth()
            .with_single_cert(self.chain.clone(), self.key.clone_key())
            .map_err(ServerIdentityError::Rejected)?;
        server.alpn_protocols = vec![alpn.protocol().to_vec()];
        Ok(TlsAcceptor::from(Arc::new(server)))
    }
}

/// How a TCP listener secures the connections it accepts. `T` is what TLS needs: the ALPN
/// protocol while the configuration plans the listener, then the acceptor once the identity is
/// loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListenerSecurity<T = TlsAcceptor> {
    /// Plaintext only: plain HTTP on an HTTP listener, h2c on the gRPC listener.
    Plain,
    /// TLS only.
    Tls(T),
    /// TLS or plaintext, as each connection's first byte selects.
    Detect(T),
}

impl ListenerSecurity<Alpn> {
    /// The primary listener's plan. A `scheme=both` HTTP listener is always plain.
    fn primary(scheme: Scheme, unix_socket: Option<&Path>) -> Self {
        match scheme {
            Scheme::Http => Self::Plain,
            // Validation refuses TLS files with a Unix socket, which is never inspected.
            Scheme::Auto if unix_socket.is_some() => Self::Plain,
            Scheme::Auto => Self::Detect(Alpn::Http1),
            Scheme::Https | Scheme::Both => Self::Tls(Alpn::Http1),
        }
    }

    /// The gRPC listener's plan; plain while there is no gRPC listener.
    fn grpc(scheme: Option<GrpcScheme>, unix_socket: Option<&Path>) -> Self {
        // Validation refuses grpc_listen beside a Unix socket, which excludes every TCP listener,
        // so the setting never gives the TLS settings a use there.
        if unix_socket.is_some() {
            return Self::Plain;
        }
        match scheme {
            Some(GrpcScheme::Auto) => Self::Detect(Alpn::H2),
            None | Some(GrpcScheme::H2c) => Self::Plain,
            Some(GrpcScheme::Tls) => Self::Tls(Alpn::H2),
        }
    }

    fn uses_tls(&self) -> bool {
        match self {
            Self::Plain => false,
            Self::Tls(_) | Self::Detect(_) => true,
        }
    }

    fn secure(self, identity: &ServerIdentity) -> Result<ListenerSecurity, ServerIdentityError> {
        Ok(match self {
            Self::Plain => ListenerSecurity::Plain,
            Self::Tls(alpn) => ListenerSecurity::Tls(identity.acceptor(alpn)?),
            Self::Detect(alpn) => ListenerSecurity::Detect(identity.acceptor(alpn)?),
        })
    }
}

/// Whether some listener serves TLS, so that the TLS settings take effect.
pub(crate) fn serves_tls(
    scheme: Scheme,
    unix_socket: Option<&Path>,
    grpc: Option<GrpcScheme>,
) -> bool {
    ListenerSecurity::primary(scheme, unix_socket).uses_tls()
        || ListenerSecurity::grpc(grpc, unix_socket).uses_tls()
}

/// How the TCP listeners secure their connections, all with one identity.
pub struct ServerSecurity {
    /// The primary listener, `listen`.
    pub primary: ListenerSecurity,
    /// The gRPC listener, `grpc_listen`; plain while there is none.
    pub grpc: ListenerSecurity,
}

impl ServerSecurity {
    /// Plaintext on every listener, for a configuration that loads no identity.
    pub fn plain() -> Self {
        Self {
            primary: ListenerSecurity::Plain,
            grpc: ListenerSecurity::Plain,
        }
    }

    /// Loads the identity once, and only when some listener serves TLS.
    pub fn load(config: &Config) -> Result<Self, ServerIdentityError> {
        Self::load_settings(
            config.scheme(),
            config.unix_socket(),
            config.grpc_security_scheme(),
            config.tls(),
        )
    }

    pub(crate) fn load_settings(
        scheme: Scheme,
        unix_socket: Option<&Path>,
        grpc: Option<GrpcScheme>,
        tls: Option<&Tls>,
    ) -> Result<Self, ServerIdentityError> {
        let Some(tls) = tls else {
            return Ok(Self::plain());
        };
        let identity = ServerIdentity::load(tls.identity())?;
        Ok(Self {
            primary: ListenerSecurity::primary(scheme, unix_socket).secure(&identity)?,
            grpc: ListenerSecurity::grpc(grpc, unix_socket).secure(&identity)?,
        })
    }
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
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![Alpn::Http1.protocol().to_vec()];
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests;
