//! Pure construction of public object URLs. Building a URL never grants or checks access; whether
//! the URL is servable depends entirely on IAM/ACL state at request time.

use crate::json::{validate_bucket_name, validate_object_name};
use std::fmt::Write;

/// Validate an advertised base URL with the same rules as `api_base_url` and trim trailing
/// slashes so callers can append `/<path>` without producing `//`.
pub fn normalize_base_url(value: &str) -> Result<String, String> {
    let uri: hyper::Uri = value.parse().map_err(|_| "invalid base URL")?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.query().is_some()
        || value.contains('#')
    {
        return Err(
            "base URL must be an absolute HTTP(S) URL without credentials, query or fragment"
                .into(),
        );
    }
    Ok(value.trim_end_matches('/').to_owned())
}

/// Build `<base>/<bucket>/<encoded object>` for the XML-style path endpoint.
pub fn public_object_url(base_url: &str, bucket: &str, object: &str) -> Result<String, String> {
    let base = normalize_base_url(base_url)?;
    validate_bucket_name(bucket).map_err(|e| format!("bucket: {}", e.message))?;
    validate_object_name(object).map_err(|e| format!("object: {}", e.message))?;
    let path = format!("/{bucket}/{}", encode_object_name(object));
    if crate::xml::reserved_path(&path) {
        return Err("object URL collides with a reserved API namespace".into());
    }
    Ok(format!("{base}{path}"))
}

/// Build the final bucket host before signing; virtual-host bases must have no path prefix.
pub fn virtual_host_object_url(
    base_url: &str,
    bucket: &str,
    object: &str,
) -> Result<String, String> {
    let base = normalize_base_url(base_url)?;
    validate_bucket_name(bucket).map_err(|e| format!("bucket: {}", e.message))?;
    validate_object_name(object).map_err(|e| format!("object: {}", e.message))?;
    let uri: hyper::Uri = base.parse().map_err(|_| "invalid base URL")?;
    if uri.path() != "/" && !uri.path().is_empty() {
        return Err("virtual-host base URL must not have a path prefix".into());
    }
    let authority = uri.authority().ok_or("missing base URL authority")?;
    crate::config::validate_dns_name(authority.host())?;
    crate::config::validate_dns_name(&format!("{bucket}.{}", authority.host()))?;
    if authority.as_str().contains(':') && authority.port_u16().is_none() {
        return Err("invalid virtual-host base URL port".into());
    }
    Ok(format!(
        "{}://{bucket}.{authority}/{}",
        uri.scheme_str().unwrap(),
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
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let error = public_object_url("http://localhost", bucket, "o").unwrap_err();
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
            let error = public_object_url("http://localhost", "bucket", object).unwrap_err();
            assert!(error.starts_with("object:"), "{object:?}: {error}");
        }
        assert!(public_object_url("http://localhost", "bucket", &"a".repeat(1024)).is_ok());
    }
}
