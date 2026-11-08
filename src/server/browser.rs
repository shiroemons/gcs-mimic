//! Requests a web page initiated, which the surfaces for local tools and for the application's
//! own servers refuse.

use crate::{auth::ListenerRole, json::JsonError};
use hyper::{HeaderMap, StatusCode};

/// Whether a browser sent the request on a page's behalf. Browsers add `Origin` to every request
/// but GET and HEAD and to cross-origin script requests, and send `Sec-Fetch-Site` to loopback
/// and HTTPS origins, where only a navigation the user started (a typed URL or a bookmark) says
/// `none`. Server-side clients and command-line tools send neither header.
pub(super) fn initiated(headers: &HeaderMap) -> bool {
    headers.contains_key("origin")
        || headers
            .get_all("sec-fetch-site")
            .iter()
            .any(|value| value != "none")
}

/// Refuses a browser-initiated request on the trusted listener before routing. A request there
/// without credentials acts as `trusted_principal`, so a page the developer opens must not reach
/// it; the browser-facing listener keeps every strict check.
pub(super) fn refuse_on_trusted_listener(
    listener: ListenerRole,
    headers: &HeaderMap,
) -> Result<(), JsonError> {
    match listener {
        ListenerRole::Trusted if initiated(headers) => Err(JsonError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "The trusted listener serves the application's own servers and refuses browser-initiated requests; send browser requests to the main listener, and send no Origin or Sec-Fetch-Site header from a server",
        )),
        ListenerRole::Trusted | ListenerRole::Public => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(entries: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in entries {
            headers.append(*name, hyper::header::HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn only_the_trusted_listener_refuses_browser_initiated_requests() {
        for entries in [
            &[("origin", "null")][..],
            &[("origin", "http://localhost:3000")],
            &[("sec-fetch-site", "cross-site")],
            &[("sec-fetch-site", "same-origin")],
            &[("sec-fetch-site", "same-site")],
            &[("sec-fetch-site", "none"), ("sec-fetch-site", "cross-site")],
        ] {
            let headers = headers(entries);
            assert_eq!(
                refuse_on_trusted_listener(ListenerRole::Trusted, &headers)
                    .unwrap_err()
                    .status,
                StatusCode::FORBIDDEN,
                "{entries:?}"
            );
            assert!(
                refuse_on_trusted_listener(ListenerRole::Public, &headers).is_ok(),
                "{entries:?}"
            );
        }
        for entries in [&[][..], &[("sec-fetch-site", "none")]] {
            assert!(
                refuse_on_trusted_listener(ListenerRole::Trusted, &headers(entries)).is_ok(),
                "{entries:?}"
            );
        }
    }
}
