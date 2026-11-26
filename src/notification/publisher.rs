//! Explicit local Pub/Sub REST integration; never discover credentials or a production endpoint.

use super::Topic;
use crate::net::is_local_address;
use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper::{Request, Uri, client::conn::http1};
use hyper_util::rt::TokioIo;
use std::{
    fmt, io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::net::{TcpStream, lookup_host};

pub(crate) const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const INVALID_ENDPOINT: &str = "Pub/Sub requires an explicit local emulator host:port (HTTP, no credentials, path or query); public, metadata and Google endpoints are disabled";

/// The emulator endpoint is not an explicit local `host:port`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidEndpoint;

impl fmt::Display for InvalidEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(INVALID_ENDPOINT)
    }
}

impl std::error::Error for InvalidEndpoint {}

/// A connection attempt reports a rejected endpoint as invalid input, as parsing does.
impl From<InvalidEndpoint> for io::Error {
    fn from(error: InvalidEndpoint) -> Self {
        Self::new(io::ErrorKind::InvalidInput, error)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PublishError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error(transparent)]
    Request(#[from] hyper::http::Error),
    #[error(transparent)]
    Http(#[from] hyper::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("length limit exceeded")]
    TooLarge,
    #[error("Pub/Sub publish returned {0}")]
    Status(hyper::StatusCode),
    #[error("Pub/Sub publish did not acknowledge exactly one message")]
    Acknowledgement,
}

#[derive(Clone, Debug)]
pub(crate) struct Endpoint {
    authority: String,
    host: String,
    port: u16,
}

impl Endpoint {
    pub(crate) fn parse(value: &str) -> Result<Self, InvalidEndpoint> {
        let uri: Uri = if value.contains("://") {
            value.to_owned()
        } else {
            format!("http://{value}")
        }
        .parse()
        .map_err(|_| InvalidEndpoint)?;
        let authority = uri.authority().ok_or(InvalidEndpoint)?;
        let port = authority
            .port_u16()
            .filter(|port| *port != 0)
            .ok_or(InvalidEndpoint)?;
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        if uri.scheme_str() != Some("http")
            || uri
                .path_and_query()
                .is_some_and(|path| path.as_str() != "/")
            || authority.as_str().contains('@')
            || host.is_empty()
            || host.ends_with("googleapis.com")
            || host.ends_with("google.internal")
            || host.parse::<IpAddr>().is_ok_and(|ip| !is_local_address(ip))
        {
            return Err(InvalidEndpoint);
        }
        Ok(Self {
            authority: authority.to_string(),
            host: host.to_owned(),
            port,
        })
    }

    async fn address(&self) -> io::Result<SocketAddr> {
        let mut addresses = lookup_host((self.host.as_str(), self.port)).await?;
        let first = addresses.next().ok_or(InvalidEndpoint)?;
        if !is_local_address(first.ip()) || addresses.any(|address| !is_local_address(address.ip()))
        {
            return Err(InvalidEndpoint.into());
        }
        // Connect to the checked address itself, so a second DNS lookup cannot bypass the guard.
        Ok(first)
    }

    pub(crate) async fn publish(&self, topic: &Topic, message: String) -> Result<(), PublishError> {
        tokio::time::timeout(ATTEMPT_TIMEOUT, self.publish_inner(topic, message)).await?
    }

    /// Ask the emulator whether `topic` exists, bounded by the same attempt timeout as a publish.
    /// Only existence is checked: the emulator has no permission model to consult.
    pub(crate) async fn topic_exists(&self, topic: &Topic) -> Result<(), TopicCheckError> {
        self.topic_exists_within(topic, ATTEMPT_TIMEOUT).await
    }

    /// `topic_exists` with the attempt timeout injected, so tests need not wait five seconds.
    async fn topic_exists_within(
        &self,
        topic: &Topic,
        timeout: Duration,
    ) -> Result<(), TopicCheckError> {
        let status = tokio::time::timeout(timeout, self.topic_status(topic))
            .await
            .map_err(|elapsed| TopicCheckError::Unreachable(elapsed.into()))?
            .map_err(TopicCheckError::Unreachable)?;
        match status {
            hyper::StatusCode::OK => Ok(()),
            hyper::StatusCode::NOT_FOUND => Err(TopicCheckError::NotFound),
            status => Err(TopicCheckError::Unexpected(status)),
        }
    }

    async fn topic_status(&self, topic: &Topic) -> Result<hyper::StatusCode, PublishError> {
        let relative = Self::topic_path(topic);
        let request = Request::get(format!("/v1/{relative}"))
            .header("Host", &self.authority)
            .body(Full::new(Bytes::new()))?;
        Ok(self.roundtrip(request).await?.0)
    }

    /// The encoded `projects/P/topics/T` path of a topic.
    fn topic_path(topic: &Topic) -> String {
        crate::url::encode_object_name(topic.relative())
    }

    async fn publish_inner(&self, topic: &Topic, message: String) -> Result<(), PublishError> {
        let relative = Self::topic_path(topic);
        let request = Request::post(format!("/v1/{relative}:publish"))
            .header("Host", &self.authority)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(message)))?;
        let (status, bytes) = self.roundtrip(request).await?;
        if status != hyper::StatusCode::OK {
            return Err(PublishError::Status(status));
        }
        let response: serde_json::Value = serde_json::from_slice(&bytes)?;
        if response.get("messageIds")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|ids| !matches!(ids.as_slice(), [id] if id.as_str().is_some_and(|id| !id.is_empty())))
        {
            return Err(PublishError::Acknowledgement);
        }
        Ok(())
    }

    /// One request on a fresh connection to the checked address; the caller owns the timeout.
    /// The response body is bounded so a misbehaving emulator cannot grow memory.
    async fn roundtrip(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<(hyper::StatusCode, Bytes), PublishError> {
        let stream = TcpStream::connect(self.address().await?).await?;
        let (mut sender, connection) = http1::Builder::new()
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(stream))
            .await?;
        // Poll the driver in this future: cancellation/timeout drops the socket immediately.
        let exchange = async {
            let response = sender.send_request(request).await?;
            let status = response.status();
            let mut body = response.into_body();
            let mut bytes = BytesMut::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame?.into_data() {
                    if data.len() > 8192 - bytes.len() {
                        return Err(PublishError::TooLarge);
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            Ok((status, bytes.freeze()))
        };
        tokio::pin!(connection, exchange);
        tokio::select! {
            biased;
            result = &mut exchange => result,
            result = &mut connection => {
                result?;
                exchange.await
            },
        }
    }
}

/// Why the emulator could not confirm that a topic exists.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TopicCheckError {
    /// The emulator answered 404 for the topic.
    #[error("Pub/Sub emulator reports no such topic")]
    NotFound,
    /// The emulator answered with another status besides 200 or 404.
    #[error("Pub/Sub emulator topic lookup returned {0}")]
    Unexpected(hyper::StatusCode),
    /// No usable answer arrived because of a connection failure, timeout or malformed response.
    #[error("Pub/Sub emulator topic lookup failed")]
    Unreachable(#[source] PublishError),
}

#[cfg(test)]
mod tests;
