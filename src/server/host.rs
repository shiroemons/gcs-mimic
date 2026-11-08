//! The host name a request addressed, for the surfaces that admit only some names: management
//! and the trusted listener. A DNS rebinding page reaches them under its own name, so the name,
//! not the connection, tells such a page apart from a local client.

use hyper::{HeaderMap, Uri, http::uri::Authority};
use std::fmt;

/// A request that names no single valid authority: several Host fields, an unparsable one,
/// user information, an empty host or a port that is not a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InvalidHost;

impl fmt::Display for InvalidHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the request names no single valid Host authority")
    }
}

impl std::error::Error for InvalidHost {}

/// A host name as names compare: lowercase, without its port or a trailing dot.
pub(super) fn normalized(authority: &Authority) -> String {
    let host = authority.host().to_ascii_lowercase();
    host.strip_suffix('.').unwrap_or(&host).to_owned()
}

/// The normalized host the request addressed, or `None` when it names none. An authority in the
/// request target overrides Host, as RFC 9112 section 3.2.2 requires.
pub(super) fn of_request(uri: &Uri, headers: &HeaderMap) -> Result<Option<String>, InvalidHost> {
    let mut hosts = headers.get_all("host").iter();
    let host = hosts.next();
    if hosts.next().is_some() {
        return Err(InvalidHost);
    }
    let authority = match uri.authority() {
        Some(authority) => authority.clone(),
        None => match host {
            Some(host) => host
                .to_str()
                .ok()
                .and_then(|value| value.parse::<Authority>().ok())
                .ok_or(InvalidHost)?,
            None => return Ok(None),
        },
    };
    // `Authority::port` reads only digits, so a text after the colon, or none, is checked here.
    if authority.as_str().contains('@')
        || authority.host().is_empty()
        || (authority.as_str() != authority.host() && authority.port_u16().is_none())
    {
        return Err(InvalidHost);
    }
    Ok(Some(normalized(&authority)))
}

/// An IPv4 address, or an IPv6 address in brackets as an authority writes it.
pub(super) fn is_ip_literal(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok()
}

/// A name without a dot, such as a Compose service name.
pub(super) fn is_single_label(host: &str) -> bool {
    !host.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(hosts: &[&'static str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for host in hosts {
            headers.append("host", hyper::header::HeaderValue::from_static(host));
        }
        headers
    }

    #[test]
    fn the_request_target_wins_and_malformed_hosts_name_nothing() {
        let origin_form: Uri = "/o".parse().unwrap();
        let absolute: Uri = "http://gcs:4443/o".parse().unwrap();
        assert_eq!(
            of_request(&absolute, &headers(&["other.example.com"])),
            Ok(Some("gcs".into()))
        );
        assert_eq!(
            of_request(&origin_form, &headers(&["Storage.Example.:4444"])),
            Ok(Some("storage.example".into()))
        );
        assert_eq!(
            of_request(&origin_form, &headers(&["[::1]:4444"])),
            Ok(Some("[::1]".into()))
        );
        assert_eq!(of_request(&origin_form, &headers(&[])), Ok(None));
        for hosts in [
            &["gcs", "gcs"][..],
            &["user@gcs"],
            &["gcs:port"],
            &["gcs:"],
            &[":4444"],
            &["gcs/path"],
        ] {
            assert_eq!(
                of_request(&origin_form, &headers(hosts)),
                Err(InvalidHost),
                "{hosts:?}"
            );
        }
    }

    #[test]
    fn literals_and_single_labels_are_told_apart_from_dotted_names() {
        for host in ["127.0.0.1", "[::1]", "10.0.0.5"] {
            assert!(is_ip_literal(host), "{host}");
        }
        for host in ["gcs", "localhost", "gcs.example", "127.1.example"] {
            assert!(!is_ip_literal(host), "{host}");
        }
        for host in ["gcs", "gcs-mimic", "localhost"] {
            assert!(is_single_label(host), "{host}");
        }
        assert!(!is_single_label("gcs.example"));
    }
}
