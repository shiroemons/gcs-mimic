//! Explicit local Pub/Sub REST integration; never discover credentials or a production endpoint.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, Uri, client::conn::http1};
use hyper_util::rt::TokioIo;
use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::net::{TcpStream, lookup_host};

pub(crate) const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const INVALID_ENDPOINT: &str = "Pub/Sub requires an explicit local emulator host:port (HTTP, no credentials, path or query); public, metadata and Google endpoints are disabled";

#[derive(Clone, Debug)]
pub(crate) struct Endpoint {
    authority: String,
    host: String,
    port: u16,
}

fn invalid_endpoint() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, INVALID_ENDPOINT)
}

impl Endpoint {
    pub(crate) fn parse(value: &str) -> io::Result<Self> {
        let uri: Uri = if value.contains("://") {
            value.to_owned()
        } else {
            format!("http://{value}")
        }
        .parse()
        .map_err(|_| invalid_endpoint())?;
        let authority = uri.authority().ok_or_else(invalid_endpoint)?;
        let port = authority
            .port_u16()
            .filter(|port| *port != 0)
            .ok_or_else(invalid_endpoint)?;
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
            || host.parse::<IpAddr>().is_ok_and(|ip| !local_address(ip))
        {
            return Err(invalid_endpoint());
        }
        Ok(Self {
            authority: authority.to_string(),
            host: host.to_owned(),
            port,
        })
    }

    async fn address(&self) -> io::Result<SocketAddr> {
        let mut addresses = lookup_host((self.host.as_str(), self.port)).await?;
        let first = addresses.next().ok_or_else(invalid_endpoint)?;
        if !local_address(first.ip()) || addresses.any(|address| !local_address(address.ip())) {
            return Err(invalid_endpoint());
        }
        // Connect to the checked address itself, so a second DNS lookup cannot bypass the guard.
        Ok(first)
    }

    pub(crate) async fn publish(&self, topic: &str, message: String) -> io::Result<()> {
        tokio::time::timeout(ATTEMPT_TIMEOUT, self.publish_inner(topic, message))
            .await
            .map_err(io::Error::other)?
    }

    /// Ask the emulator whether `topic` exists, bounded by the same attempt timeout as a publish.
    /// Only existence is checked: the emulator has no permission model to consult.
    pub(crate) async fn topic_exists(&self, topic: &str) -> Result<(), TopicCheckError> {
        self.topic_exists_within(topic, ATTEMPT_TIMEOUT).await
    }

    /// `topic_exists` with the attempt timeout injected, so tests need not wait five seconds.
    async fn topic_exists_within(
        &self,
        topic: &str,
        timeout: Duration,
    ) -> Result<(), TopicCheckError> {
        let status = tokio::time::timeout(timeout, self.topic_status(topic))
            .await
            .map_err(|elapsed| TopicCheckError::Unreachable(io::Error::other(elapsed)))?
            .map_err(TopicCheckError::Unreachable)?;
        match status {
            hyper::StatusCode::OK => Ok(()),
            hyper::StatusCode::NOT_FOUND => Err(TopicCheckError::NotFound),
            status => Err(TopicCheckError::Unexpected(status)),
        }
    }

    async fn topic_status(&self, topic: &str) -> io::Result<hyper::StatusCode> {
        let relative = self.topic_path(topic)?;
        let request = Request::get(format!("/v1/{relative}"))
            .header("Host", &self.authority)
            .body(Full::new(Bytes::new()))
            .map_err(io::Error::other)?;
        Ok(self.roundtrip(request).await?.0)
    }

    /// The encoded `projects/P/topics/T` path of a canonical topic name.
    fn topic_path(&self, topic: &str) -> io::Result<String> {
        if topic.parse::<super::Topic>().is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Pub/Sub topic",
            ));
        }
        let relative = topic
            .strip_prefix("//pubsub.googleapis.com/")
            .ok_or_else(invalid_endpoint)?;
        Ok(crate::url::encode_object_name(relative))
    }

    async fn publish_inner(&self, topic: &str, message: String) -> io::Result<()> {
        let relative = self.topic_path(topic)?;
        let request = Request::post(format!("/v1/{relative}:publish"))
            .header("Host", &self.authority)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(message)))
            .map_err(io::Error::other)?;
        let (status, bytes) = self.roundtrip(request).await?;
        if status != hyper::StatusCode::OK {
            return Err(io::Error::other(format!(
                "Pub/Sub publish returned {status}"
            )));
        }
        let response: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if response.get("messageIds")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|ids| !matches!(ids.as_slice(), [id] if id.as_str().is_some_and(|id| !id.is_empty())))
        {
            return Err(io::Error::other(
                "Pub/Sub publish did not acknowledge exactly one message",
            ));
        }
        Ok(())
    }

    /// One request on a fresh connection to the checked address; the caller owns the timeout.
    /// The response body is bounded so a misbehaving emulator cannot grow memory.
    async fn roundtrip(
        &self,
        request: Request<Full<Bytes>>,
    ) -> io::Result<(hyper::StatusCode, Bytes)> {
        let stream = TcpStream::connect(self.address().await?).await?;
        let (mut sender, connection) = http1::Builder::new()
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(io::Error::other)?;
        // Poll the driver in this future: cancellation/timeout drops the socket immediately.
        let exchange = async {
            let response = sender
                .send_request(request)
                .await
                .map_err(io::Error::other)?;
            let status = response.status();
            let bytes = Limited::new(response.into_body(), 8192)
                .collect()
                .await
                .map_err(io::Error::other)?
                .to_bytes();
            Ok((status, bytes))
        };
        tokio::pin!(connection, exchange);
        tokio::select! {
            biased;
            result = &mut exchange => result,
            result = &mut connection => {
                result.map_err(io::Error::other)?;
                exchange.await
            },
        }
    }
}

/// Why the emulator could not confirm that a topic exists.
#[derive(Debug)]
pub(crate) enum TopicCheckError {
    /// The emulator answered 404 for the topic.
    NotFound,
    /// The emulator answered something other than 200 or 404.
    Unexpected(hyper::StatusCode),
    /// No usable answer: connection failure, timeout or malformed response.
    Unreachable(io::Error),
}

impl std::fmt::Display for TopicCheckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("Pub/Sub emulator reports no such topic"),
            Self::Unexpected(status) => {
                write!(formatter, "Pub/Sub emulator topic lookup returned {status}")
            }
            Self::Unreachable(_) => formatter.write_str("Pub/Sub emulator topic lookup failed"),
        }
    }
}

impl std::error::Error for TopicCheckError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreachable(error) => Some(error),
            Self::NotFound | Self::Unexpected(_) => None,
        }
    }
}

fn local_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_loopback() || address.is_private(),
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unique_local()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|address| address.is_loopback() || address.is_private())
        }
    }
}

#[cfg(test)]
mod tests;
