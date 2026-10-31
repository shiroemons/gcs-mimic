use crate::config::{BoxError, Config};
use google_cloud_auth::credentials::anonymous;
use google_cloud_gax::retry_policy::NeverRetry;
use google_cloud_storage::client::{Storage, StorageControl};
use reqwest::{Certificate, Client, Method, Request, Response, redirect::Policy};
use std::{error::Error, io, time::Duration};
use url::Url;

/// HTTP client for signed URLs and POST policies. The SDK only produces those
/// requests, so they are sent with a separate client restricted to the emulator origin.
pub(crate) struct SignedHttp {
    client: Client,
    origin: Url,
}

impl SignedHttp {
    pub(crate) fn new(config: &Config) -> Result<Self, BoxError> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(20));
        if let Some(path) = &config.ca_pem {
            // Trust only the scorecard CA, matching the SDK transport's trust anchor.
            let ca = Certificate::from_pem(&std::fs::read(path)?)?;
            builder = builder.tls_certs_only([ca]);
        }
        Ok(Self {
            client: builder.build()?,
            origin: Url::parse(&config.origin)?,
        })
    }

    pub(crate) fn request(
        &self,
        method: Method,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, BoxError> {
        let parsed = Url::parse(url)?;
        if parsed.origin() != self.origin.origin() {
            return Err(io::Error::other("request escaped emulator").into());
        }
        Ok(self.client.request(method, parsed))
    }

    pub(crate) async fn send(&self, request: Request) -> Result<Response, BoxError> {
        if request.url().origin() != self.origin.origin() {
            return Err(io::Error::other("request escaped emulator").into());
        }
        Ok(self.client.execute(request).await?)
    }
}

pub(crate) async fn storage(config: &Config) -> Result<Storage, BoxError> {
    Ok(Storage::builder()
        .with_endpoint(config.origin.as_str())
        .with_credentials(anonymous::Builder::new().build())
        .with_retry_policy(NeverRetry)
        .build()
        .await?)
}

pub(crate) async fn control(endpoint: &str) -> Result<StorageControl, BoxError> {
    Ok(StorageControl::builder()
        .with_endpoint(endpoint)
        .with_credentials(anonymous::Builder::new().build())
        .with_retry_policy(NeverRetry)
        .build()
        .await?)
}

/// Renders an error with its whole `source()` chain; TLS failures are only visible in
/// the innermost causes.
pub(crate) fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut current = error.source();
    while let Some(cause) = current {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        current = cause.source();
    }
    text
}

/// Verifies, without an emulator, that the SDK transport and the signed-request client
/// both reach the configured origin with the configured CA and certificate hostname.
pub(crate) async fn check(config: &Config) -> Result<(), BoxError> {
    let http = SignedHttp::new(config)?;
    match http.request(Method::GET, "https://storage.googleapis.com/") {
        Err(error) if error.to_string() == "request escaped emulator" => {}
        _ => return Err(io::Error::other("external origin accepted").into()),
    }
    let request = http
        .request(Method::GET, &format!("{}/", config.origin))?
        .build()?;
    let response = http.send(request).await.map_err(|error| {
        io::Error::other(format!("signed-request client: {}", chain(error.as_ref())))
    })?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(io::Error::other(format!("GET / returned {}", response.status())).into());
    }
    // `openssl s_server -www` serves one connection at a time; finish this response and
    // drop the pooled connection before the SDK connects.
    response.bytes().await?;
    drop(http);
    sdk_reaches_origin(config).await
}

/// Issues an ordinary SDK read. Any HTTP response, even an error or an unparsable one,
/// proves that the TLS handshake succeeded; only a failure before a response arrives is
/// a transport failure.
async fn sdk_reaches_origin(config: &Config) -> Result<(), BoxError> {
    let storage = storage(config).await?;
    let result = storage
        .read_object("projects/_/buckets/transport-check", "probe")
        .send()
        .await;
    let Err(error) = result else { return Ok(()) };
    if error.http_status_code().is_some() || error.status().is_some() || error.is_deserialization()
    {
        return Ok(());
    }
    let mut detail = format!("SDK transport: {}", chain(&error));
    if cfg!(target_vendor = "apple") {
        detail.push_str(
            " (google-cloud-storage 1.21.0 verifies TLS with rustls-platform-verifier, which uses the macOS trust store and ignores SSL_CERT_FILE)",
        );
    }
    Err(io::Error::other(detail).into())
}
