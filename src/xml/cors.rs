use crate::{bucket::CorsRule, config::CorsFallback, json::cors::CorsRequest};
use hyper::{
    HeaderMap, Method,
    header::{HeaderName, HeaderValue},
};
use std::{collections::BTreeSet, sync::Mutex};

/// Methods the allow-all fallback grants; PATCH is omitted because no XML route accepts it.
const ALLOW_ALL_METHODS: &str = "HEAD, GET, POST, PUT, DELETE";
/// Bounds the remembered bucket names so the warning memory does not grow with the catalog.
const WARNED_BUCKET_LIMIT: usize = 64;

static WARNED_BUCKETS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Browsers only report a generic CORS failure, so the server log is the one place that can say
/// a migrated fake-gcs-server caller now needs the fallback or bucket rules.
pub(crate) fn warn_unconfigured_cors(bucket: &str, headers: &HeaderMap) {
    if crate::json::cors::origin(headers).is_none() {
        return;
    }
    let first = first_warning(
        &mut WARNED_BUCKETS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        bucket,
    );
    if first {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "cors_unconfigured",
            serde_json::json!({
                "bucket": bucket,
                "message": "XML/public browser request reached a bucket without CORS rules; set GCS_MIMIC_CORS_FALLBACK=allow-all or configure the bucket cors field",
            }),
        );
    }
}

fn first_warning(warned: &mut BTreeSet<String>, bucket: &str) -> bool {
    warned.len() < WARNED_BUCKET_LIMIT && warned.insert(bucket.to_owned())
}

/// `fallback` applies only when `rules` is empty, so stored bucket rules keep their exact XML
/// behavior, including denials.
pub(crate) fn cors(
    headers: &HeaderMap,
    method: &Method,
    rules: &[CorsRule],
    request: CorsRequest,
    fallback: CorsFallback,
) -> HeaderMap {
    let mut result = HeaderMap::new();
    let one = |name: &str| {
        let mut values = headers.get_all(name).iter();
        let value = values.next()?;
        values.next().is_none().then_some(value)?.to_str().ok()
    };
    let Some(origin_value) = crate::json::cors::origin(headers) else {
        return result;
    };
    let Ok(origin) = origin_value.to_str() else {
        return result;
    };
    let requested_method = match request {
        CorsRequest::Preflight => {
            let Some(method) = one("access-control-request-method")
                .filter(|value| Method::from_bytes(value.as_bytes()).is_ok())
            else {
                return result;
            };
            method
        }
        CorsRequest::Actual => method.as_str(),
    };
    if !matches!(requested_method, "DELETE" | "GET" | "HEAD" | "POST" | "PUT") {
        return result;
    }
    if rules.is_empty() && fallback == CorsFallback::AllowAll {
        return allow_all(headers, request, one("access-control-request-headers"));
    }
    // The first origin/method match determines the policy, even if its headers deny preflight.
    let Some(rule) = rules.iter().find(|rule| {
        rule.origin
            .iter()
            .any(|allowed| origin_matches(allowed, origin))
            && rule
                .method
                .iter()
                .any(|allowed| allowed == "*" || allowed == requested_method)
    }) else {
        return result;
    };
    match request {
        CorsRequest::Preflight => {
            let requested_headers = if headers.contains_key("access-control-request-headers") {
                let Some(value) = one("access-control-request-headers") else {
                    return result;
                };
                let names: Vec<_> = value.split(',').map(str::trim).collect();
                if names.iter().any(|name| {
                    HeaderName::from_bytes(name.as_bytes()).is_err()
                        || !rule
                            .response_header
                            .iter()
                            .any(|allowed| allowed == "*" || allowed.eq_ignore_ascii_case(name))
                }) {
                    return result;
                }
                names
            } else {
                Vec::new()
            };
            // `validate_cors` admits only method tokens, `*` and header names, and every stored rule
            // passes it on update and on catalog load, so these joined lists are valid header values.
            // Omitting a header is still safer than panicking if that invariant is ever broken.
            if let Ok(methods) = HeaderValue::from_str(&rule.method.join(", ")) {
                result.insert("access-control-allow-methods", methods);
            }
            // Each requested name already parsed as a `HeaderName`.
            if !requested_headers.is_empty()
                && let Ok(names) = HeaderValue::from_str(&requested_headers.join(", "))
            {
                result.insert("access-control-allow-headers", names);
            }
            result.insert(
                "access-control-max-age",
                HeaderValue::from(rule.max_age_seconds.unwrap_or(3600)),
            );
            result.insert(
                "vary",
                HeaderValue::from_static(
                    "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
                ),
            );
        }
        CorsRequest::Actual => {
            if !rule.response_header.is_empty()
                && let Ok(names) = HeaderValue::from_str(&rule.response_header.join(", "))
            {
                result.insert("access-control-expose-headers", names);
            }
            result.insert("vary", HeaderValue::from_static("Origin"));
        }
    }
    result.insert("access-control-allow-origin", origin_value.clone());
    result
}

/// Mirrors fake-gcs-server's wildcard middleware for valid origins without its credential grant:
/// browsers reject credentials with a wildcard origin, so omitting it changes no allowed request.
/// Every valid requested header is allowed, which also covers its `-cors-headers` additions.
fn allow_all(
    headers: &HeaderMap,
    request: CorsRequest,
    requested_headers: Option<&str>,
) -> HeaderMap {
    let mut result = HeaderMap::new();
    match request {
        CorsRequest::Preflight => {
            if headers.contains_key("access-control-request-headers") {
                let Some(value) = requested_headers
                    .filter(|value| {
                        value
                            .split(',')
                            .all(|name| HeaderName::from_bytes(name.trim().as_bytes()).is_ok())
                    })
                    .and_then(|value| HeaderValue::from_str(value).ok())
                else {
                    return result;
                };
                result.insert("access-control-allow-headers", value);
            }
            result.insert(
                "access-control-allow-methods",
                HeaderValue::from_static(ALLOW_ALL_METHODS),
            );
            result.insert(
                "vary",
                HeaderValue::from_static(
                    "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
                ),
            );
        }
        CorsRequest::Actual => {
            result.insert(
                "access-control-expose-headers",
                HeaderValue::from_static("Location"),
            );
            result.insert("vary", HeaderValue::from_static("Origin"));
        }
    }
    result.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    result
}

fn origin_matches(allowed: &str, origin: &str) -> bool {
    if allowed == "*" || allowed == origin {
        return true;
    }
    let (allowed_value, origin_value) = (allowed, origin);
    let (Ok(allowed), Ok(origin)) = (allowed.parse::<hyper::Uri>(), origin.parse::<hyper::Uri>())
    else {
        return false;
    };
    for (uri, value) in [(&allowed, allowed_value), (&origin, origin_value)] {
        let (Some(authority), Some(scheme)) = (uri.authority(), uri.scheme_str()) else {
            return false;
        };
        if value != format!("{scheme}://{authority}")
            || authority.as_str().contains('@')
            || authority
                .as_str()
                .rsplit_once(':')
                .is_some_and(|(_, port)| !port.contains(']') && port.parse::<u16>().is_err())
        {
            return false;
        }
    }
    let (Some(allowed_scheme), Some(origin_scheme), Some(allowed_host), Some(host)) = (
        allowed.scheme_str(),
        origin.scheme_str(),
        allowed.host(),
        origin.host(),
    ) else {
        return false;
    };
    let port = |uri: &hyper::Uri| {
        uri.port_u16().or_else(|| match uri.scheme_str()? {
            "http" | "ws" => Some(80),
            "https" | "wss" => Some(443),
            "ftp" => Some(21),
            _ => None,
        })
    };
    if !allowed_scheme.eq_ignore_ascii_case(origin_scheme)
        || port(&allowed) != port(&origin)
        || allowed.path() != "/"
        || allowed.query().is_some()
    {
        return false;
    }
    if allowed_host.eq_ignore_ascii_case(host) {
        return true;
    }
    let Some(suffix) = allowed_host
        .strip_prefix('*')
        .filter(|suffix| suffix.starts_with('.'))
    else {
        return false;
    };
    host.to_ascii_lowercase()
        .strip_suffix(&suffix.to_ascii_lowercase())
        .is_some_and(|prefix| !prefix.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(preflight: bool) -> CorsRequest {
        if preflight {
            CorsRequest::Preflight
        } else {
            CorsRequest::Actual
        }
    }

    fn rule() -> CorsRule {
        CorsRule {
            origin: vec!["https://app.example".into()],
            method: vec!["GET".into(), "HEAD".into()],
            response_header: vec!["X-Example".into(), "ETag".into()],
            max_age_seconds: None,
        }
    }

    /// Bucket-rule cases run without a fallback, as in the default configuration.
    fn cors(
        headers: &HeaderMap,
        method: &Method,
        rules: &[CorsRule],
        preflight: bool,
    ) -> HeaderMap {
        super::cors(headers, method, rules, kind(preflight), CorsFallback::None)
    }

    fn allow_all(
        headers: &HeaderMap,
        method: &Method,
        rules: &[CorsRule],
        preflight: bool,
    ) -> HeaderMap {
        super::cors(
            headers,
            method,
            rules,
            kind(preflight),
            CorsFallback::AllowAll,
        )
    }

    fn request(origin: &str, method: Option<&str>, names: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("origin", origin.parse().unwrap());
        if let Some(method) = method {
            headers.insert("access-control-request-method", method.parse().unwrap());
        }
        if let Some(names) = names {
            headers.insert("access-control-request-headers", names.parse().unwrap());
        }
        headers
    }

    #[test]
    fn xml_cors_matches_first_rule_and_never_grants_credentials() {
        let rule = rule();
        for method in [Method::GET, Method::HEAD] {
            let headers = cors(
                &request("https://app.example", None, None),
                &method,
                std::slice::from_ref(&rule),
                false,
            );
            assert_eq!(
                headers["access-control-allow-origin"],
                "https://app.example"
            );
            assert_eq!(headers["access-control-expose-headers"], "X-Example, ETag");
            assert!(!headers.contains_key("access-control-allow-credentials"));
        }
        let preflight = request("https://app.example", Some("GET"), Some("x-example, etag"));
        let headers = cors(
            &preflight,
            &Method::OPTIONS,
            std::slice::from_ref(&rule),
            true,
        );
        assert_eq!(headers["access-control-max-age"], "3600");
        assert_eq!(headers["access-control-allow-headers"], "x-example, etag");
        assert_eq!(headers["access-control-allow-methods"], "GET, HEAD");
        let mut second = rule.clone();
        second.response_header = vec!["*".into()];
        assert!(
            cors(
                &request("https://app.example", Some("GET"), Some("forbidden")),
                &Method::OPTIONS,
                &[rule, second],
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn xml_cors_denials_wildcards_and_malformed_headers() {
        for headers in [
            request("https://denied.example", Some("GET"), None),
            request("https://app.example", Some("PUT"), None),
            request("https://app.example", None, None),
            request("https://app.example", Some("GET"), Some("x-other")),
            request("https://app.example", Some("GET"), Some("x-example,")),
        ] {
            assert!(cors(&headers, &Method::OPTIONS, &[rule()], true).is_empty());
        }
        let wildcard = CorsRule {
            origin: vec!["*".into()],
            method: vec!["*".into()],
            response_header: vec!["*".into()],
            max_age_seconds: Some(0),
        };
        for origin in [
            "https://test,evil.example",
            "https://test?.example",
            "https://user@app.example",
            "https://app.example/path",
            "malformed",
        ] {
            assert!(
                cors(
                    &request(origin, Some("GET"), None),
                    &Method::OPTIONS,
                    std::slice::from_ref(&wildcard),
                    true
                )
                .is_empty()
            );
        }
        let mut headers = request("null", Some("HEAD"), Some("authorization, range"));
        let result = cors(
            &headers,
            &Method::OPTIONS,
            std::slice::from_ref(&wildcard),
            true,
        );
        assert_eq!(result["access-control-allow-origin"], "null");
        assert_eq!(result["access-control-max-age"], "0");
        assert!(
            cors(
                &request("null", Some("PATCH"), None),
                &Method::OPTIONS,
                std::slice::from_ref(&wildcard),
                true,
            )
            .is_empty()
        );
        headers.append("origin", "https://other.example".parse().unwrap());
        assert!(cors(&headers, &Method::OPTIONS, &[wildcard], true).is_empty());
    }

    #[test]
    fn xml_cors_subdomain_wildcard_preserves_scheme_port_and_domain_boundary() {
        for origin in [
            "https://test.example.com",
            "https://nested.test.example.com",
        ] {
            assert!(origin_matches("https://*.example.com", origin));
        }
        for origin in [
            "http://test.example.com",
            "https://example.com",
            "https://testexample.com",
            "https://test.example.com:4443",
            "https://test.example.com/",
            "https://user@example.com",
            "https://example.com.evil",
        ] {
            assert!(!origin_matches("https://*.example.com", origin), "{origin}");
        }
        assert!(origin_matches(
            "https://*.example.com:4443",
            "https://test.example.com:4443"
        ));
    }

    #[test]
    fn xml_cors_matches_default_ports_and_mobile_origins_without_http_allowlist() {
        for (allowed, request) in [
            ("http://app.example", "http://app.example:80"),
            ("http://app.example:80", "http://app.example"),
            ("https://app.example:443", "https://app.example"),
            ("https://*.example.com:443", "https://mobile.example.com"),
            ("capacitor://localhost", "capacitor://localhost"),
            ("ionic://localhost", "ionic://localhost"),
            ("app+local.v1://device:1234", "app+local.v1://device:1234"),
        ] {
            assert!(origin_matches(allowed, request), "{allowed} {request}");
        }
        for (allowed, request) in [
            ("http://app.example:80", "http://app.example:8080"),
            ("capacitor://localhost", "ionic://localhost"),
            ("capacitor://localhost", "capacitor://localhost:80"),
            (
                "https://*.example.com:443",
                "https://mobile.example.com:4443",
            ),
        ] {
            assert!(!origin_matches(allowed, request), "{allowed} {request}");
        }
    }

    #[test]
    fn allow_all_fallback_allows_any_valid_origin_and_header_only_without_bucket_rules() {
        let preflight = request(
            "https://other.example",
            Some("PUT"),
            Some("authorization, x-goog-meta-owner"),
        );
        let headers = allow_all(&preflight, &Method::OPTIONS, &[], true);
        assert_eq!(headers["access-control-allow-origin"], "*");
        assert_eq!(
            headers["access-control-allow-methods"],
            "HEAD, GET, POST, PUT, DELETE"
        );
        assert_eq!(
            headers["access-control-allow-headers"],
            "authorization, x-goog-meta-owner"
        );
        assert!(!headers.contains_key("access-control-allow-credentials"));
        assert!(!headers.contains_key("access-control-max-age"));
        let actual = allow_all(&request("null", None, None), &Method::GET, &[], false);
        assert_eq!(actual["access-control-allow-origin"], "*");
        assert_eq!(actual["access-control-expose-headers"], "Location");
        assert!(!actual.contains_key("access-control-allow-credentials"));
        // Stored rules stay authoritative, including their denials.
        let denied = request("https://other.example", Some("GET"), None);
        assert!(allow_all(&denied, &Method::OPTIONS, &[rule()], true).is_empty());
        let allowed = request("https://app.example", None, None);
        assert_eq!(
            allow_all(&allowed, &Method::GET, &[rule()], false)["access-control-allow-origin"],
            "https://app.example"
        );
        for headers in [
            request("https://app.example", Some("PATCH"), None),
            request("https://app.example", None, None),
            request("https://app.example", Some("GET"), Some("invalid header")),
            request("https://app.example", Some("GET"), Some("x-example,")),
            request("https://app.example/path", Some("GET"), None),
        ] {
            assert!(allow_all(&headers, &Method::OPTIONS, &[], true).is_empty());
        }
        assert!(cors(&preflight, &Method::OPTIONS, &[], true).is_empty());
    }

    #[test]
    fn unconfigured_warning_is_once_per_bucket_and_bounded() {
        let mut warned = BTreeSet::new();
        assert!(first_warning(&mut warned, "bucket-a"));
        assert!(!first_warning(&mut warned, "bucket-a"));
        for index in 1..WARNED_BUCKET_LIMIT {
            assert!(first_warning(&mut warned, &format!("bucket-{index}")));
        }
        assert!(!first_warning(&mut warned, "bucket-over-limit"));
        assert_eq!(warned.len(), WARNED_BUCKET_LIMIT);
    }
}
