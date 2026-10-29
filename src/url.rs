//! Pure construction of public object URLs. Building a URL never grants or checks access; whether
//! the URL is servable depends entirely on IAM/ACL state at request time.

use crate::{
    config::InvalidDnsName,
    json::{validate_bucket_name, validate_object_name},
};
use std::fmt;

/// Why a base URL, public origin or object URL was rejected. `Display` is the operator-facing
/// diagnostic that CLI output and configuration reports show verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlError {
    InvalidBaseUrl,
    UnsupportedBaseUrl,
    MissingAuthority,
    MissingScheme,
    InvalidBasePort,
    OriginHasPath,
    OriginPortZero,
    InvalidOriginIpv6,
    AmbiguousNumericHost,
    InvalidOriginAuthority,
    InvalidDnsName(InvalidDnsName),
    InvalidBucket(String),
    InvalidObject(String),
    ReservedNamespace,
    VirtualHostPathPrefix,
    VirtualHostBucketNotDns,
    InvalidVirtualHostPort,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseUrl => f.write_str("invalid base URL"),
            Self::UnsupportedBaseUrl => f.write_str(
                "base URL must be an absolute HTTP(S) URL without credentials, query or fragment",
            ),
            Self::MissingAuthority => f.write_str("missing base URL authority"),
            Self::MissingScheme => f.write_str("missing base URL scheme"),
            Self::InvalidBasePort => {
                f.write_str("invalid base URL port; use an integer in 0..=65535")
            }
            Self::OriginHasPath => {
                f.write_str("public_base_url must be an HTTP(S) origin with a root path only")
            }
            Self::OriginPortZero => f.write_str(
                "public_base_url port must be in 1..=65535; use a reachable port such as http://storage.invalid:4443",
            ),
            Self::InvalidOriginIpv6 => f.write_str("invalid public_base_url IPv6 address"),
            Self::AmbiguousNumericHost => f.write_str(
                "ambiguous numeric public hostname; use canonical IPv4 such as http://127.0.0.1 or a DNS name ending in a nonnumeric label",
            ),
            Self::InvalidOriginAuthority => f.write_str("invalid public origin authority"),
            Self::InvalidDnsName(error) => error.fmt(f),
            Self::InvalidBucket(message) => write!(f, "bucket: {message}"),
            Self::InvalidObject(message) => write!(f, "object: {message}"),
            Self::ReservedNamespace => {
                f.write_str("object URL collides with a reserved API namespace")
            }
            Self::VirtualHostPathPrefix => {
                f.write_str("virtual-host base URL must not have a path prefix")
            }
            Self::VirtualHostBucketNotDns => {
                f.write_str("bucket name must be a DNS label sequence for virtual-host URLs")
            }
            Self::InvalidVirtualHostPort => f.write_str("invalid virtual-host base URL port"),
        }
    }
}

// `InvalidDnsName` is displayed transparently, so it is not repeated as a `source`.
impl std::error::Error for UrlError {}

impl From<InvalidDnsName> for UrlError {
    fn from(error: InvalidDnsName) -> Self {
        Self::InvalidDnsName(error)
    }
}

#[derive(Clone, Debug)]
enum OriginHost {
    Dns(String),
    Ipv4(std::net::Ipv4Addr),
    Ipv6(std::net::Ipv6Addr),
}

/// A validated public origin; private fields keep routing and displayed URLs consistent.
#[derive(Clone, Debug)]
pub struct PublicOrigin {
    base: String,
    authority: hyper::http::uri::Authority,
    host: OriginHost,
}

impl PublicOrigin {
    pub fn parse(value: &str) -> Result<Self, UrlError> {
        let base = normalize_base_url(value)?;
        let uri: hyper::Uri = base.parse().map_err(|_| UrlError::InvalidBaseUrl)?;
        if !uri.path().trim_matches('/').is_empty() {
            return Err(UrlError::OriginHasPath);
        }
        let authority = uri.authority().ok_or(UrlError::MissingAuthority)?;
        if authority.port_u16() == Some(0) {
            return Err(UrlError::OriginPortZero);
        }
        let host = authority.host();
        let host = if host.starts_with('[') {
            OriginHost::Ipv6(
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse()
                    .map_err(|_| UrlError::InvalidOriginIpv6)?,
            )
        } else if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
            OriginHost::Ipv4(ip)
        } else {
            let last = host.rsplit('.').next().unwrap_or_default();
            if !last.is_empty()
                && (last.bytes().all(|byte| byte.is_ascii_digit())
                    || last
                        .strip_prefix("0x")
                        .or_else(|| last.strip_prefix("0X"))
                        .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit())))
            {
                return Err(UrlError::AmbiguousNumericHost);
            }
            crate::config::validate_dns_name(host)?;
            OriginHost::Dns(host.to_ascii_lowercase())
        };
        let host_text = match &host {
            OriginHost::Dns(name) => name.clone(),
            OriginHost::Ipv4(ip) => ip.to_string(),
            OriginHost::Ipv6(ip) => format!("[{ip}]"),
        };
        let scheme = uri.scheme_str().ok_or(UrlError::MissingScheme)?;
        let port = authority
            .port_u16()
            .filter(|port| !matches!((scheme, port), ("http", 80) | ("https", 443)));
        let authority_text = format!(
            "{host_text}{}",
            port.map(|p| format!(":{p}")).unwrap_or_default()
        );
        Ok(Self {
            base: format!("{scheme}://{authority_text}"),
            authority: authority_text
                .parse()
                .map_err(|_| UrlError::InvalidOriginAuthority)?,
            host,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.base
    }

    pub fn dns_name(&self) -> Option<&str> {
        match &self.host {
            OriginHost::Dns(name) => Some(name),
            _ => None,
        }
    }

    pub(crate) fn authority(&self) -> &hyper::http::uri::Authority {
        &self.authority
    }
}

/// Validate an advertised base URL with the same rules as `api_base_url` and trim trailing
/// slashes so callers can append `/<path>` without producing `//`.
pub fn normalize_base_url(value: &str) -> Result<String, UrlError> {
    let uri: hyper::Uri = value.parse().map_err(|_| UrlError::InvalidBaseUrl)?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.query().is_some()
        || value.contains('#')
    {
        return Err(UrlError::UnsupportedBaseUrl);
    }
    let authority = uri.authority().ok_or(UrlError::MissingAuthority)?;
    if authority.as_str() != authority.host() && authority.port_u16().is_none() {
        return Err(UrlError::InvalidBasePort);
    }
    Ok(value.trim_end_matches('/').to_owned())
}

/// Normalize a public origin before deriving routing domains or signing helper URLs.
pub fn normalize_public_base_url(value: &str) -> Result<String, UrlError> {
    PublicOrigin::parse(value).map(|origin| origin.base)
}

/// Build `<base>/<bucket>/<encoded object>` for the XML-style path endpoint.
pub fn public_object_url(base_url: &str, bucket: &str, object: &str) -> Result<String, UrlError> {
    let base = normalize_base_url(base_url)?;
    validate_bucket_name(bucket).map_err(|e| UrlError::InvalidBucket(e.message))?;
    validate_object_name(object).map_err(|e| UrlError::InvalidObject(e.message))?;
    let path = format!("/{bucket}/{}", encode_object_name(object));
    if crate::xml::reserved_path(&path) {
        return Err(UrlError::ReservedNamespace);
    }
    Ok(format!("{base}{path}"))
}

/// Build the final bucket host before signing; virtual-host bases must have no path prefix.
pub fn virtual_host_object_url(
    base_url: &str,
    bucket: &str,
    object: &str,
) -> Result<String, UrlError> {
    let base = normalize_base_url(base_url)?;
    validate_bucket_name(bucket).map_err(|e| UrlError::InvalidBucket(e.message))?;
    validate_object_name(object).map_err(|e| UrlError::InvalidObject(e.message))?;
    let uri: hyper::Uri = base.parse().map_err(|_| UrlError::InvalidBaseUrl)?;
    if uri.path() != "/" && !uri.path().is_empty() {
        return Err(UrlError::VirtualHostPathPrefix);
    }
    let authority = uri.authority().ok_or(UrlError::MissingAuthority)?;
    crate::config::validate_dns_name(authority.host())?;
    crate::config::validate_dns_name(bucket).map_err(|_| UrlError::VirtualHostBucketNotDns)?;
    let host = format!("{bucket}.{}", authority.host().to_ascii_lowercase());
    crate::config::validate_dns_name(&host)?;
    if authority.as_str().contains(':') && authority.port_u16().is_none() {
        return Err(UrlError::InvalidVirtualHostPort);
    }
    Ok(format!(
        "{}://{host}{}/{}",
        uri.scheme_str().ok_or(UrlError::MissingScheme)?,
        authority
            .port_u16()
            .map(|port| format!(":{port}"))
            .unwrap_or_default(),
        encode_object_name(object)
    ))
}

/// Percent-encode every UTF-8 byte except RFC 3986 unreserved characters and `/`. Clients and
/// proxies normalize `.` and `..` path segments away, so when any segment is a dot segment every
/// `/` is encoded as `%2F` and the whole name stays a single opaque path segment.
pub fn encode_object_name(object: &str) -> String {
    let keep_slash = !object
        .split('/')
        .any(|segment| matches!(segment, "." | ".."));
    let mut encoded = String::with_capacity(object.len());
    for byte in object.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (byte == b'/' && keep_slash)
        {
            encoded.push(byte as char);
        } else {
            push_percent_encoded(&mut encoded, byte);
        }
    }
    encoded
}

/// Append `%XX` with uppercase hex digits, the RFC 3986 form shared by object paths and
/// signed-URL canonicalization.
pub(crate) fn push_percent_encoded(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(char::from(HEX[usize::from(byte >> 4)]));
    out.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn public_origins_normalize_dns_ips_ports_and_root_paths() {
        for (input, expected) in [
            (
                "https://Storage.Example.Invalid:443///",
                "https://storage.example.invalid",
            ),
            ("http://storage.invalid:80/", "http://storage.invalid"),
            (
                "http://storage.invalid:8080/",
                "http://storage.invalid:8080",
            ),
            ("http://127.0.0.1:80/", "http://127.0.0.1"),
            ("https://[0:0:0:0:0:0:0:1]:443/", "https://[::1]"),
            ("http://[::1]:8080/", "http://[::1]:8080"),
        ] {
            assert_eq!(normalize_public_base_url(input).unwrap(), expected);
        }
        for input in [
            "http://storage.invalid/prefix",
            "http://storage.invalid/%2F",
            "http://storage.invalid:invalid",
            "http://storage.invalid:65536",
            "http://storage.invalid:",
            "http://storage.invalid:0",
            "https://127.0.0.1:0",
            "http://[::1]:0",
            "http://bad_name.invalid",
            "http://storage.invalid.",
            "http://[bad]",
            "http://user@storage.invalid",
            "http://storage.invalid?",
            "http://storage.invalid/#fragment",
            "ftp://storage.invalid",
            "http://127.1",
            "http://0x7f.0.0.1",
            "http://2130706433",
            "http://0177.0.0.1",
            "http://storage.123",
            "http://storage.0xFF",
            "http://storage.0x",
            "http://999.999.999.999",
            "",
        ] {
            assert!(normalize_public_base_url(input).is_err(), "{input}");
        }
    }

    fn public_origin_inputs() -> impl Strategy<Value = String> {
        let hosts = prop_oneof![
            proptest::collection::vec(
                prop_oneof![
                    "[a-zA-Z][a-zA-Z0-9-]{0,61}[a-zA-Z0-9]".boxed(),
                    "[0-9]{1,63}".boxed(),
                    Just("x".repeat(63)).boxed(),
                ],
                2..4
            )
            .prop_map(|labels| format!("Mixed.{}.Invalid", labels.join("."))),
            any::<[u8; 4]>().prop_map(|ip| std::net::Ipv4Addr::from(ip).to_string()),
            any::<[u16; 8]>().prop_map(|ip| format!(
                "[{:04X}:{:04X}:{:04X}:{:04X}:{:04X}:{:04X}:{:04X}:{:04X}]",
                ip[0], ip[1], ip[2], ip[3], ip[4], ip[5], ip[6], ip[7]
            )),
        ];
        (
            any::<bool>(),
            hosts,
            prop_oneof![
                Just(None),
                Just(Some(80)),
                Just(Some(443)),
                (1..=u16::MAX).prop_map(Some)
            ],
            0..8usize,
        )
            .prop_map(|(https, host, port, slashes)| {
                format!(
                    "{}://{host}{}{}",
                    if https { "https" } else { "http" },
                    port.map(|port| format!(":{port}")).unwrap_or_default(),
                    "/".repeat(slashes)
                )
            })
    }

    proptest! {
        #[test]
        fn normalized_public_origins_are_idempotent(input in public_origin_inputs()) {
            let origin = PublicOrigin::parse(&input).unwrap();
            let normalized = origin.as_str();
            prop_assert_eq!(normalize_public_base_url(normalized).unwrap(), normalized);
            let url = public_object_url(normalized, "my-bucket", "a b").unwrap();
            prop_assert_eq!(&url, &format!("{normalized}/my-bucket/a%20b"));
            let signed = crate::auth::signature::sign_url(&url, &hyper::Method::GET,
                "admin@example.invalid", include_bytes!("../fixtures/signing/test-only-private.pem"),
                60, std::time::SystemTime::now(), hyper::HeaderMap::new()).unwrap();
            let prefix = format!("{url}?");
            prop_assert!(signed.starts_with(&prefix));
            let mut headers = hyper::HeaderMap::new();
            headers.insert("host", origin.authority().as_str().parse().unwrap());
            let config = crate::config::Config {
                compat_profile: crate::config::CompatProfile::LegacyEmulator,
                xml_virtual_host_domains: origin.dns_name().map(|name| vec![name.to_owned()]).unwrap_or_default(),
                public_base_url: Some(origin), ..Default::default()
            };
            let selected = crate::xml::select_object(&url.parse().unwrap(), &headers, &config).unwrap().unwrap();
            prop_assert_eq!(selected.bucket, "my-bucket");
        }

        #[test]
        fn prefixed_public_origins_normalize_idempotently(input in public_origin_inputs(), prefix in "[a-zA-Z0-9/]{0,32}") {
            let normalized = normalize_base_url(&format!("{}/{prefix}/", normalize_public_base_url(&input).unwrap())).unwrap();
            prop_assert_eq!(normalize_base_url(&normalized).unwrap(), normalized);
        }

        #[test]
        fn arbitrary_base_normalization_is_idempotent(input in proptest::collection::vec(any::<char>(), 0..256).prop_map(|chars| chars.into_iter().collect::<String>())) {
            if let Ok(normalized) = normalize_base_url(&input) {
                prop_assert_eq!(normalize_base_url(&normalized).unwrap(), normalized);
            }
            if let Ok(origin) = PublicOrigin::parse(&input) {
                prop_assert_eq!(normalize_public_base_url(origin.as_str()).unwrap(), origin.as_str());
            }
        }

        #[test]
        fn public_origins_reject_port_zero(input in public_origin_inputs()) {
            let origin = PublicOrigin::parse(&input).unwrap();
            let uri: hyper::Uri = origin.as_str().parse().unwrap();
            let input = format!("{}://{}:0", uri.scheme_str().unwrap(), uri.host().unwrap());
            prop_assert!(PublicOrigin::parse(&input).unwrap_err().to_string().contains(":4443"));
        }
    }

    #[test]
    fn virtual_host_urls_preserve_final_authority_and_raw_object_path() {
        assert_eq!(
            virtual_host_object_url(
                "https://storage.invalid:4443/",
                "dotted.bucket",
                "é//a b/%2F"
            )
            .unwrap(),
            "https://dotted.bucket.storage.invalid:4443/%C3%A9//a%20b/%252F"
        );
        assert_eq!(
            virtual_host_object_url("http://storage.invalid", "bucket", "storage/v1/b").unwrap(),
            "http://bucket.storage.invalid/storage/v1/b"
        );
        assert_eq!(
            virtual_host_object_url("https://Storage.Invalid:4443", "bucket", "o").unwrap(),
            "https://bucket.storage.invalid:4443/o"
        );
        assert!(
            virtual_host_object_url("https://storage.invalid", "bucket_name", "o")
                .unwrap_err()
                .to_string()
                .contains("bucket name must be a DNS label sequence")
        );
        for base in [
            "http://127.0.0.1",
            "http://[::1]",
            "http://storage.invalid/prefix",
            "http://storage.invalid:invalid",
        ] {
            assert!(
                virtual_host_object_url(base, "bucket", "object").is_err(),
                "{base}"
            );
        }
    }

    fn url(object: &str) -> String {
        public_object_url("http://127.0.0.1:4443", "bucket", object).unwrap()
    }

    #[test]
    fn encodes_reserved_and_non_ascii_bytes() {
        assert_eq!(url("a b"), "http://127.0.0.1:4443/bucket/a%20b");
        assert_eq!(url("a#b?c"), "http://127.0.0.1:4443/bucket/a%23b%3Fc");
        assert_eq!(url("100%+1"), "http://127.0.0.1:4443/bucket/100%25%2B1");
        assert_eq!(url("é日"), "http://127.0.0.1:4443/bucket/%C3%A9%E6%97%A5");
        assert_eq!(url("A-z_0.9~"), "http://127.0.0.1:4443/bucket/A-z_0.9~");
        assert_eq!(
            url(":@!$&'()*,;="),
            "http://127.0.0.1:4443/bucket/%3A%40%21%24%26%27%28%29%2A%2C%3B%3D"
        );
    }

    #[test]
    fn keeps_slashes_unless_a_segment_is_a_dot_segment() {
        assert_eq!(url("a/b/c.txt"), "http://127.0.0.1:4443/bucket/a/b/c.txt");
        assert_eq!(url("a//b/"), "http://127.0.0.1:4443/bucket/a//b/");
        assert_eq!(url("/a"), "http://127.0.0.1:4443/bucket//a");
        assert_eq!(url("a/../b"), "http://127.0.0.1:4443/bucket/a%2F..%2Fb");
        assert_eq!(url("./x"), "http://127.0.0.1:4443/bucket/.%2Fx");
        assert_eq!(url("x/."), "http://127.0.0.1:4443/bucket/x%2F.");
        assert_eq!(url("a/.../b"), "http://127.0.0.1:4443/bucket/a/.../b");
        assert_eq!(url(".hidden/x"), "http://127.0.0.1:4443/bucket/.hidden/x");
    }

    #[test]
    fn normalizes_base_urls() {
        for (base, expected) in [
            ("http://localhost:4443/", "http://localhost:4443/bucket/o"),
            ("http://localhost:4443///", "http://localhost:4443/bucket/o"),
            (
                "https://storage.example.invalid",
                "https://storage.example.invalid/bucket/o",
            ),
            (
                "http://proxy.invalid:8080/prefix/gcs/",
                "http://proxy.invalid:8080/prefix/gcs/bucket/o",
            ),
        ] {
            assert_eq!(public_object_url(base, "bucket", "o").unwrap(), expected);
        }
    }

    #[test]
    fn rejects_invalid_base_urls() {
        for base in [
            "",
            "relative",
            "/path",
            "ftp://example.invalid",
            "http://user@example.invalid",
            "http://user:secret@example.invalid",
            "http://example.invalid?q=x",
            "http://example.invalid/#fragment",
            "http://",
            "http://example.invalid\r\n",
        ] {
            assert!(public_object_url(base, "bucket", "o").is_err(), "{base:?}");
        }
    }

    #[test]
    fn rejects_invalid_bucket_names() {
        for bucket in [
            "", "ab", "UPPER", "-bucket", "bucket-", "goog-x", "a/b", "1.2.3.4",
        ] {
            let error = public_object_url("http://localhost", bucket, "o")
                .unwrap_err()
                .to_string();
            assert!(error.starts_with("bucket:"), "{bucket:?}: {error}");
        }
    }

    #[test]
    fn rejects_invalid_object_names() {
        let long = "a".repeat(1025);
        for object in [
            "",
            ".",
            "..",
            "a\rb",
            "a\nb",
            ".well-known/acme-challenge/x",
            long.as_str(),
        ] {
            let error = public_object_url("http://localhost", "bucket", object)
                .unwrap_err()
                .to_string();
            assert!(error.starts_with("object:"), "{object:?}: {error}");
        }
        assert!(public_object_url("http://localhost", "bucket", &"a".repeat(1024)).is_ok());
    }
}
