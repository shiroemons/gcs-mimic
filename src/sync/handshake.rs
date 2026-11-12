//! The `/_internal/sync` routes are the internal protocol between a server and `gcs-mimic sync`
//! of the same release, outside the public contract. Each side sends its version in
//! [`VERSION_HEADER`] and refuses the other's when it differs, so a mismatch fails naming both
//! versions instead of as an unexplained error about an option, a field or a route.

use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use std::fmt;

pub const VERSION_HEADER: HeaderName = HeaderName::from_static("x-gcs-mimic-version");

/// This release's version as the header value both sides send.
pub fn version() -> HeaderValue {
    HeaderValue::from_static(crate::VERSION)
}

/// Which side reported the version being checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// The server checks a request from `gcs-mimic sync`.
    Client,
    /// `gcs-mimic sync` checks the server's response.
    Server,
}

/// The peer is another release, or one from before this check, which reports no version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    peer: Peer,
    reported: Option<String>,
}

/// Accepts `headers` only when they report exactly this release's version.
pub fn check(headers: &HeaderMap, peer: Peer) -> Result<(), Mismatch> {
    match headers.get(&VERSION_HEADER) {
        Some(value) if value.as_bytes() == crate::VERSION.as_bytes() => Ok(()),
        reported => Err(Mismatch {
            peer,
            reported: reported.map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned()),
        }),
    }
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ours = crate::VERSION;
        match (self.peer, &self.reported) {
            (Peer::Client, Some(theirs)) => write!(
                f,
                "gcs-mimic sync {theirs} cannot control this gcs-mimic {ours} server; use gcs-mimic sync from release {ours}"
            ),
            (Peer::Client, None) => write!(
                f,
                "Synchronization controls require gcs-mimic sync from this server's release, {ours}; older commands do not report their version"
            ),
            (Peer::Server, Some(theirs)) => write!(
                f,
                "The server runs gcs-mimic {theirs}, but this command is {ours}; use gcs-mimic sync from release {theirs}"
            ),
            (Peer::Server, None) => write!(
                f,
                "The server reported no version, so it predates gcs-mimic {ours}; use gcs-mimic sync from the server's release. It may already have acted on this request, so inspect its status before retrying"
            ),
        }
    }
}

impl std::error::Error for Mismatch {}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(version: Option<&'static str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(version) = version {
            headers.insert(VERSION_HEADER, HeaderValue::from_static(version));
        }
        headers
    }

    #[test]
    fn only_the_same_release_passes_and_a_mismatch_names_both_versions() {
        for peer in [Peer::Client, Peer::Server] {
            assert_eq!(check(&headers(Some(crate::VERSION)), peer), Ok(()));
            let different = check(&headers(Some("0.0.1")), peer).unwrap_err();
            let text = different.to_string();
            assert!(
                text.contains("0.0.1") && text.contains(crate::VERSION),
                "{text}"
            );
            let missing = check(&headers(None), peer).unwrap_err();
            assert!(missing.to_string().contains(crate::VERSION), "{missing}");
        }
        // A candidate is a release of its own, never interchangeable with its final version.
        let candidate = format!("{}-rc.1", crate::VERSION);
        let mut map = HeaderMap::new();
        map.insert(VERSION_HEADER, HeaderValue::from_str(&candidate).unwrap());
        assert!(check(&map, Peer::Client).is_err());
    }
}
