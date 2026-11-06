//! Management is for local tools; page-initiated requests must never reach mutations.

use crate::{
    config::{Config, EndpointSnapshot},
    json::JsonError,
    server::{RequestBody, RequestCtx},
    url::PublicOrigin,
};
use hyper::{Request, StatusCode, http::uri::Authority};

pub(super) fn browser_or_misdirected(
    request: &Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<(), JsonError> {
    if request.headers().contains_key("origin")
        || request
            .headers()
            .get_all("sec-fetch-site")
            .iter()
            .any(|v| v != "none")
    {
        return Err(JsonError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Management routes refuse browser-initiated requests; use a local tool without Origin or Sec-Fetch-Site",
        ));
    }
    if let Some(host) = request_host(request)?
        && !host_allowed(&host, ctx.config, ctx.endpoints.snapshot())
    {
        return Err(JsonError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "misdirected",
            "Management host is not local or configured; add its DNS name to admin_allowed_hosts",
        ));
    }
    Ok(())
}

fn normalized_host(authority: &Authority) -> String {
    let host = authority.host().to_ascii_lowercase();
    host.strip_suffix('.').unwrap_or(&host).to_owned()
}

fn request_host(request: &Request<RequestBody>) -> Result<Option<String>, JsonError> {
    let invalid =
        || JsonError::invalid_argument("Require at most one valid management Host authority");
    let mut hosts = request.headers().get_all("host").iter();
    let host = hosts.next();
    if hosts.next().is_some() {
        return Err(invalid());
    }
    let authority = match request.uri().authority() {
        Some(authority) => authority.clone(),
        None => match host {
            Some(host) => host
                .to_str()
                .ok()
                .and_then(|v| v.parse::<Authority>().ok())
                .ok_or_else(invalid)?,
            None => return Ok(None),
        },
    };
    if authority.as_str().contains('@')
        || authority.host().is_empty()
        || (authority.port().is_some() && authority.port_u16().is_none())
    {
        return Err(invalid());
    }
    Ok(Some(normalized_host(&authority)))
}

fn local_host(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok()
        || !host.contains('.')
        || [
            "localhost",
            "internal",
            "local",
            "home.arpa",
            "test",
            "invalid",
            "example",
        ]
        .iter()
        .any(|suffix| {
            host == *suffix || host.strip_suffix(suffix).is_some_and(|v| v.ends_with('.'))
        })
}

/// Takes the snapshot rather than the request's endpoints: a base derived from this very Host
/// would match it and admit every DNS rebinding name.
fn host_allowed(host: &str, config: &Config, endpoints: &EndpointSnapshot) -> bool {
    local_host(host)
        || config.admin_allowed_hosts.iter().any(|name| name == host)
        || endpoints
            .xml_virtual_host_domains
            .iter()
            .any(|name| name == host)
        || std::iter::once(endpoints.fixed_api_authority())
            .chain(
                endpoints
                    .public_base_url
                    .as_ref()
                    .map(PublicOrigin::authority),
            )
            .any(|authority| normalized_host(authority) == host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_host_classification_does_not_accept_public_lookalikes() {
        for host in [
            "localhost",
            "127.0.0.1",
            "[::1]",
            "gcs",
            "host.docker.internal",
            "a.local",
            "a.home.arpa",
            "a.test",
            "a.invalid",
            "a.example",
        ] {
            assert!(local_host(host), "{host}");
        }
        for host in [
            "localhost.attacker.com",
            "gcs.example.com",
            "notlocalhost.com",
        ] {
            assert!(!local_host(host), "{host}");
        }
        assert_eq!(
            normalized_host(&"LOCALHOST.:4443".parse().unwrap()),
            "localhost"
        );
    }

    #[test]
    fn management_authority_rejects_duplicates_and_prefers_uri_authority() {
        let mut request = Request::builder()
            .uri("http://gcs:4443/_internal/reset")
            .header("host", "other.example.com")
            .body(RequestBody::Buffered(http_body_util::Full::new(
                bytes::Bytes::new(),
            )))
            .unwrap();
        assert_eq!(request_host(&request).unwrap().as_deref(), Some("gcs"));
        request.headers_mut().append("host", "gcs".parse().unwrap());
        assert_eq!(
            request_host(&request).unwrap_err().status,
            StatusCode::BAD_REQUEST
        );
    }

    /// A base derived from Host would match that Host, so a rebinding page's own name would pass.
    #[tokio::test]
    async fn management_host_is_not_admitted_by_a_base_derived_from_the_same_host() {
        use crate::{
            auth::Auth,
            config::ConnectionScheme,
            server::{ObjectActivity, State},
            storage::Catalog,
        };
        let dir = crate::test_support::TestDir::new();
        let state = State::for_test(Catalog::open(&dir.0).await.unwrap(), Auth::permissive());
        let config = Config::default();
        let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
        for (host, expected) in [
            ("evil.example.com", Some(StatusCode::MISDIRECTED_REQUEST)),
            ("api.example.com", None),
        ] {
            let request = Request::builder()
                .uri("/_internal/config")
                .header("host", host)
                .body(RequestBody::Buffered(http_body_util::Full::new(
                    bytes::Bytes::new(),
                )))
                .unwrap();
            let endpoints = config
                .runtime_endpoints
                .snapshot(&config, "http://api.example.com")
                .unwrap()
                .for_request(ConnectionScheme::Http, request.uri(), request.headers());
            assert_eq!(endpoints.api_base_url(), format!("http://{host}"));
            let ctx = RequestCtx {
                state: &state,
                config: &config,
                endpoints: &endpoints,
                activity: &activity,
            };
            assert_eq!(
                browser_or_misdirected(&request, ctx)
                    .err()
                    .map(|error| error.status),
                expected,
                "{host}"
            );
        }
        state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn management_guards_run_before_every_route_specific_gate() {
        use crate::{
            auth::Auth,
            config::RequestEndpoints,
            server::{ObjectActivity, State},
            storage::Catalog,
        };
        let dir = crate::test_support::TestDir::new();
        let state = State::for_test(Catalog::open(&dir.0).await.unwrap(), Auth::permissive());
        let config = Config {
            admin_allowed_hosts: vec!["dev.example.com".into()],
            ..Config::default()
        };
        let endpoints = RequestEndpoints::explicit("http://api.example.com");
        let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
        };
        for path in [
            "/_internal/config",
            "/_internal/objects",
            "/_internal/reset",
            "/_internal/delete_all",
            "/_internal/reseed",
            "/_internal/sync/approve",
        ] {
            for (name, value) in [
                ("origin", "null"),
                ("origin", "https://evil.example"),
                ("sec-fetch-site", "cross-site"),
                ("sec-fetch-site", "same-origin"),
                ("sec-fetch-site", "same-site"),
            ] {
                let request = Request::builder()
                    .uri(path)
                    .header(name, value)
                    .body(RequestBody::Buffered(http_body_util::Full::new(
                        bytes::Bytes::new(),
                    )))
                    .unwrap();
                let route = super::super::AdminRoute::parse(path).unwrap();
                assert_eq!(
                    super::super::respond(route, request, ctx).await.status(),
                    StatusCode::FORBIDDEN
                );
            }
        }
        for (host, expected) in [
            ("dev.example.com", true),
            ("api.example.com", true),
            ("evil.example.com", false),
        ] {
            assert_eq!(
                host_allowed(host, &config, endpoints.snapshot()),
                expected,
                "{host}"
            );
        }
        state.catalog.close().await.unwrap();
    }
}
