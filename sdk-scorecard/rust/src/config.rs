use std::{env, error::Error, io, path::PathBuf};
use url::Url;

/// Environment variables that could route SDK traffic through a proxy, select ambient
/// credentials, reach the metadata server or redirect the SDK to another endpoint.
const UNSAFE_ENV: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "REQUEST_METHOD",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_QUOTA_PROJECT",
    "GOOGLE_CLOUD_UNIVERSE_DOMAIN",
    "CLOUDSDK_CONFIG",
    "GCE_METADATA_HOST",
    "GCE_METADATA_ROOT",
    "GCE_METADATA_IP",
    "STORAGE_EMULATOR_HOST",
    "SSL_CERT_DIR",
];

pub(crate) struct Config {
    /// Bare HTTPS origin without a trailing slash, as the SDK and signers expect it.
    pub(crate) origin: String,
    pub(crate) grpc: Option<String>,
    pub(crate) ca_pem: Option<PathBuf>,
    pub(crate) signing_key: PathBuf,
    pub(crate) project: String,
    pub(crate) filter: String,
}

pub(crate) type BoxError = Box<dyn Error + Send + Sync>;

impl Config {
    pub(crate) fn from_env() -> Result<Self, BoxError> {
        let origin = loopback_origin(&env::var("SCORECARD_ENDPOINT")?)?;
        let grpc = match env::var("SCORECARD_GRPC_ENDPOINT") {
            Ok(raw) => Some(loopback_grpc(&raw)?),
            Err(env::VarError::NotPresent) => None,
            Err(error) => return Err(error.into()),
        };
        let signing_key = env::var_os("SCORECARD_SIGNING_KEY").map_or_else(
            || {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/signing/test-only-private.pem")
            },
            PathBuf::from,
        );
        Ok(Self {
            origin,
            grpc,
            ca_pem: env::var_os("SCORECARD_CA_CERT").map(PathBuf::from),
            signing_key,
            project: env::var("SCORECARD_PROJECT").unwrap_or_else(|_| "local-project".into()),
            filter: env::var("SCORECARD_RUN").unwrap_or_default(),
        })
    }
}

/// Removes ambient routing and credential settings and makes the SDK trust only the
/// scorecard CA.
///
/// google-cloud-storage 1.21.0 builds its own reqwest client and accepts no custom
/// client or root store, so the CA reaches it only through `SSL_CERT_FILE`, which
/// rustls-native-certs reads on non-Apple Unix. Apple targets verify through the
/// Security framework and ignore this variable.
#[expect(
    unsafe_code,
    reason = "edition 2024 marks environment mutation unsafe; main calls this before any thread exists"
)]
pub(crate) fn sanitize_process_env() {
    let ca = env::var_os("SCORECARD_CA_CERT");
    // SAFETY: called first in `main`, before the Tokio runtime or any other thread starts,
    // so no concurrent environment reads can observe a partial update.
    unsafe {
        for name in UNSAFE_ENV {
            env::remove_var(name);
        }
        match ca {
            Some(path) => env::set_var("SSL_CERT_FILE", path),
            None => env::remove_var("SSL_CERT_FILE"),
        }
    }
}

fn loopback_origin(raw: &str) -> Result<String, BoxError> {
    let url = Url::parse(raw)?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !matches!(url.scheme(), "http" | "https")
        || !loopback
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || raw.trim_end_matches('/').len() + 1 < raw.len()
    {
        return Err(io::Error::other(
            "SCORECARD_ENDPOINT must be a bare loopback origin with an explicit port",
        )
        .into());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn loopback_grpc(raw: &str) -> Result<String, BoxError> {
    let url = Url::parse(&format!("http://{raw}"))?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !loopback || url.port().is_none() || url.path() != "/" || raw.contains(['/', '@', '?', '#'])
    {
        return Err(io::Error::other(
            "SCORECARD_GRPC_ENDPOINT must be a loopback host:port without a scheme",
        )
        .into());
    }
    Ok(format!("http://{raw}"))
}

#[cfg(test)]
mod tests {
    use super::{loopback_grpc, loopback_origin};

    #[test]
    fn origin_guard_accepts_only_bare_loopback_origins() {
        assert_eq!(
            loopback_origin("https://127.0.0.1:4443").ok().as_deref(),
            Some("https://127.0.0.1:4443")
        );
        assert!(loopback_origin("https://127.0.0.1:4443/").is_ok());
        for input in [
            "https://storage.googleapis.com",
            "http://metadata.google.internal:80",
            "https://user@127.0.0.1:4443",
            "https://127.0.0.1:4443/path",
            "https://127.0.0.1:4443?x",
            "https://127.0.0.1",
            "ftp://127.0.0.1:21",
        ] {
            assert!(loopback_origin(input).is_err(), "accepted {input}");
        }
    }

    #[test]
    fn grpc_guard_accepts_only_loopback_host_port() {
        assert_eq!(
            loopback_grpc("127.0.0.1:9000").ok().as_deref(),
            Some("http://127.0.0.1:9000")
        );
        for input in [
            "http://127.0.0.1:9000",
            "example.com:9000",
            "127.0.0.1",
            "127.0.0.1:9/x",
        ] {
            assert!(loopback_grpc(input).is_err(), "accepted {input}");
        }
    }
}
