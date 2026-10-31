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

    async fn publish_inner(&self, topic: &str, message: String) -> io::Result<()> {
        if super::normalize_topic(topic).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid queued Pub/Sub topic",
            ));
        }
        let relative = topic
            .strip_prefix("//pubsub.googleapis.com/")
            .ok_or_else(invalid_endpoint)?;
        let relative = crate::url::encode_object_name(relative);
        let stream = TcpStream::connect(self.address().await?).await?;
        let (mut sender, connection) = http1::Builder::new()
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(io::Error::other)?;
        let request = Request::post(format!("/v1/{relative}:publish"))
            .header("Host", &self.authority)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(message)))
            .map_err(io::Error::other)?;
        // Poll the driver in this future: cancellation/timeout drops the socket immediately.
        let exchange = async {
            let response = sender
                .send_request(request)
                .await
                .map_err(io::Error::other)?;
            if response.status() != hyper::StatusCode::OK {
                return Err(io::Error::other(format!(
                    "Pub/Sub publish returned {}",
                    response.status()
                )));
            }
            let bytes = Limited::new(response.into_body(), 8192)
                .collect()
                .await
                .map_err(io::Error::other)?
                .to_bytes();
            let response: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if response["messageIds"]
                .as_array()
                .is_none_or(|ids| ids.len() != 1 || ids[0].as_str().is_none_or(str::is_empty))
            {
                return Err(io::Error::other(
                    "Pub/Sub publish did not acknowledge exactly one message",
                ));
            }
            Ok(())
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
