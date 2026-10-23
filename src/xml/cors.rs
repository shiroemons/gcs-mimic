use crate::bucket::CorsRule;
use hyper::{
    HeaderMap, Method,
    header::{HeaderName, HeaderValue},
};

pub(crate) fn cors(
    headers: &HeaderMap,
    method: &Method,
    rules: &[CorsRule],
    preflight: bool,
) -> HeaderMap {
    let mut result = HeaderMap::new();
    let one = |name: &str| {
        let mut values = headers.get_all(name).iter();
        let value = values.next()?;
        values.next().is_none().then_some(value)?.to_str().ok()
    };
    let Some(origin) = crate::json::cors::origin(headers).and_then(|value| value.to_str().ok())
    else {
        return result;
    };
    let requested_method = if preflight {
        let Some(method) = one("access-control-request-method")
            .filter(|value| Method::from_bytes(value.as_bytes()).is_ok())
        else {
            return result;
        };
        method
    } else {
        method.as_str()
    };
    if !matches!(requested_method, "DELETE" | "GET" | "HEAD" | "POST" | "PUT") {
        return result;
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
    if preflight {
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
        result.insert(
            "access-control-allow-methods",
            HeaderValue::from_str(&rule.method.join(", ")).unwrap(),
        );
        if !requested_headers.is_empty() {
            result.insert(
                "access-control-allow-headers",
                HeaderValue::from_str(&requested_headers.join(", ")).unwrap(),
            );
        }
        result.insert(
            "access-control-max-age",
            HeaderValue::from_str(&rule.max_age_seconds.unwrap_or(3600).to_string()).unwrap(),
        );
        result.insert(
            "vary",
            HeaderValue::from_static(
                "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
            ),
        );
    } else {
        if !rule.response_header.is_empty() {
            result.insert(
                "access-control-expose-headers",
                HeaderValue::from_str(&rule.response_header.join(", ")).unwrap(),
            );
        }
        result.insert("vary", HeaderValue::from_static("Origin"));
    }
    result.insert(
        "access-control-allow-origin",
        HeaderValue::from_str(origin).unwrap(),
    );
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

    fn rule() -> CorsRule {
        CorsRule {
            origin: vec!["https://app.example".into()],
            method: vec!["GET".into(), "HEAD".into()],
            response_header: vec!["X-Example".into(), "ETag".into()],
            max_age_seconds: None,
        }
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
}
