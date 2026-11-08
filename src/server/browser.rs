//! Requests a web page could send, which the surfaces for local tools and for the application's
//! own servers refuse, and the headers that keep a page from using what the trusted listener
//! returns.

use super::host;
use crate::{auth::ListenerRole, json::JsonError};
use hyper::{HeaderMap, Request, StatusCode, header::HeaderValue};

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

/// Refuses, before routing, a request on the trusted listener that a page could have sent. A
/// request there without credentials acts as `trusted_principal`, so a page the developer opens
/// must not reach it; the browser-facing listener keeps every strict check. The Host rule applies
/// to requests with credentials too, so one rule decides who reaches the listener.
pub(super) fn refuse_on_trusted_listener<B>(
    listener: ListenerRole,
    request: &Request<B>,
    allowed_hosts: &[String],
) -> Result<(), JsonError> {
    match listener {
        ListenerRole::Public => Ok(()),
        ListenerRole::Trusted if initiated(request.headers()) => Err(JsonError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "The trusted listener serves the application's own servers and refuses browser-initiated requests; send browser requests to the main listener, and send no Origin or Sec-Fetch-Site header from a server",
        )),
        ListenerRole::Trusted => admit_trusted_host(request, allowed_hosts),
    }
}

/// A page that rebinds its own DNS name to this listener sends same-origin GETs without `Origin`,
/// and over plain HTTP to an address other than loopback without `Sec-Fetch-Site` either, so the
/// absence of those headers alone must not admit it. Its Host still names it.
fn admit_trusted_host<B>(request: &Request<B>, allowed_hosts: &[String]) -> Result<(), JsonError> {
    let host =
        host::of_request(request.uri(), request.headers()).map_err(|host::InvalidHost| {
            JsonError::invalid_argument(
                "Require at most one valid Host authority on the trusted listener",
            )
        })?;
    if host.is_some_and(|host| trusted_host(&host, allowed_hosts)) {
        return Ok(());
    }
    Err(JsonError::new(
        StatusCode::MISDIRECTED_REQUEST,
        "misdirected",
        "The trusted listener accepts only IP literals, localhost names and single-label hosts such as a Compose service name; add another DNS name to trusted_allowed_hosts",
    ))
}

/// Rebinding needs a name the attacker's DNS answers, which is a public name with a dot. A page
/// cannot make a browser send an IP literal (no DNS is involved), a `localhost` name (browsers
/// resolve those to loopback, and no public DNS delegates the reserved name) or a single-label
/// name for an origin its attacker controls. Other names are admitted only when configured.
fn trusted_host(host: &str, allowed_hosts: &[String]) -> bool {
    host::is_ip_literal(host)
        || host::is_single_label(host)
        || host.ends_with(".localhost")
        || allowed_hosts.iter().any(|name| name == host)
}

/// Keeps pages on other origins from using a trusted-listener response even as an opaque embed,
/// such as an image or a script, which needs no CORS. Clients that are not browsers ignore both
/// headers.
pub(super) fn protect_trusted_response(headers: &mut HeaderMap) {
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        host: Option<&'static str>,
        entries: &[(&'static str, &'static str)],
    ) -> Request<()> {
        let mut request = Request::builder().uri("/storage/v1/b/bucket/o/object");
        if let Some(host) = host {
            request = request.header("host", host);
        }
        for (name, value) in entries {
            request = request.header(*name, *value);
        }
        request.body(()).unwrap()
    }

    fn status(
        listener: ListenerRole,
        request: &Request<()>,
        allowed: &[String],
    ) -> Option<StatusCode> {
        refuse_on_trusted_listener(listener, request, allowed)
            .err()
            .map(|error| error.status)
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
            let request = request(Some("gcs:4444"), entries);
            assert_eq!(
                status(ListenerRole::Trusted, &request, &[]),
                Some(StatusCode::FORBIDDEN),
                "{entries:?}"
            );
            assert_eq!(
                status(ListenerRole::Public, &request, &[]),
                None,
                "{entries:?}"
            );
        }
        for entries in [&[][..], &[("sec-fetch-site", "none")]] {
            assert_eq!(
                status(
                    ListenerRole::Trusted,
                    &request(Some("gcs:4444"), entries),
                    &[]
                ),
                None,
                "{entries:?}"
            );
        }
    }

    #[test]
    fn the_trusted_listener_admits_only_hosts_a_rebinding_page_cannot_name() {
        let allowed = ["storage.compose.example".to_owned()];
        for host in [
            "127.0.0.1:4444",
            "[::1]:4444",
            "localhost",
            "localhost:4444",
            "app.localhost:4444",
            "gcs",
            "gcs-mimic:4444",
            "GCS.:4444",
            "storage.compose.example:4444",
        ] {
            assert_eq!(
                status(ListenerRole::Trusted, &request(Some(host), &[]), &allowed),
                None,
                "{host}"
            );
        }
        for host in [
            "evil.example:4444",
            "evil.example",
            "localhost.evil.example",
            "gcs.evil.example",
            "compose.example",
        ] {
            assert_eq!(
                status(ListenerRole::Trusted, &request(Some(host), &[]), &allowed),
                Some(StatusCode::MISDIRECTED_REQUEST),
                "{host}"
            );
            assert_eq!(
                status(ListenerRole::Public, &request(Some(host), &[]), &allowed),
                None
            );
        }
        // A configured name admits only itself.
        assert_eq!(
            status(
                ListenerRole::Trusted,
                &request(Some("storage.compose.example"), &[]),
                &[]
            ),
            Some(StatusCode::MISDIRECTED_REQUEST)
        );
        assert_eq!(
            status(ListenerRole::Trusted, &request(None, &[]), &allowed),
            Some(StatusCode::MISDIRECTED_REQUEST)
        );
        let mut duplicated = request(Some("gcs"), &[]);
        duplicated
            .headers_mut()
            .append("host", HeaderValue::from_static("gcs"));
        assert_eq!(
            status(ListenerRole::Trusted, &duplicated, &allowed),
            Some(StatusCode::BAD_REQUEST)
        );
        // Credentials do not bypass the rule, so no request reaches the listener under a foreign name.
        let presented = request(
            Some("evil.example:4444"),
            &[("authorization", "Bearer app-token")],
        );
        assert_eq!(
            status(ListenerRole::Trusted, &presented, &allowed),
            Some(StatusCode::MISDIRECTED_REQUEST)
        );
    }

    #[test]
    fn trusted_responses_refuse_cross_origin_embedding_and_sniffing() {
        let mut headers = HeaderMap::new();
        headers.insert("x-content-type-options", HeaderValue::from_static("other"));
        protect_trusted_response(&mut headers);
        assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers.get_all("x-content-type-options").iter().count(), 1);
    }
}
