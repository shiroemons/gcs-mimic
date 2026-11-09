//! `x-amz-*` extension headers. Cloud Storage processes the ones with an `x-goog-*` equivalent
//! when the request carries an AWS signature. This runs after verification, which has already
//! consumed the original header names.

use super::{Error, canned_acl};
use crate::auth::signature::ExtensionDialect;
use hyper::{HeaderMap, Method, Request, header::HeaderName};

/// The XML surface a request reached. Without an AWS signature only object writes have a
/// recorded answer for these headers, so every other request keeps rejecting them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Object,
    Bucket,
}

impl Route {
    fn writes_object(self, method: &Method) -> bool {
        match self {
            Self::Object => matches!(*method, Method::PUT | Method::POST),
            Self::Bucket => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Names {
    Exact,
    Prefix,
}

/// What an object write without an AWS signature does with the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WithoutAwsSignature {
    /// Accepted without effect.
    Ignored,
    /// Refused as Cloud Storage refused it.
    MissingCopySource,
    /// Left for the adapter's existing rejection, or for authentication that already used it.
    Unchanged,
}

#[derive(Clone, Copy, Debug)]
struct Equivalent {
    amz: &'static str,
    goog: &'static str,
    names: Names,
    without_aws_signature: WithoutAwsSignature,
}

impl Equivalent {
    const fn exact(
        amz: &'static str,
        goog: &'static str,
        without_aws_signature: WithoutAwsSignature,
    ) -> Self {
        Self {
            amz,
            goog,
            names: Names::Exact,
            without_aws_signature,
        }
    }

    const fn prefix(
        amz: &'static str,
        goog: &'static str,
        without_aws_signature: WithoutAwsSignature,
    ) -> Self {
        Self {
            amz,
            goog,
            names: Names::Prefix,
            without_aws_signature,
        }
    }
}

/// The only list of `x-amz-*` headers this adapter understands: the migration guide's headers
/// whose `x-goog-*` equivalent differs only in its prefix, plus `x-amz-project-id`, which it
/// accepts for simple migrations. Other `x-amz-*` headers have no equivalent here; see
/// [`ignored_with_aws_signature`].
///
/// The `Ignored` and `MissingCopySource` entries are what Cloud Storage did on an OAuth PUT
/// (recorded 2026-11-09), except `x-amz-metadata-directive`, which was not recorded: it has
/// nothing to act on without a copy source, so it is accepted like the others.
const EQUIVALENTS: &[Equivalent] = &{
    use WithoutAwsSignature::{Ignored, MissingCopySource, Unchanged};
    [
        Equivalent::prefix("x-amz-meta-", "x-goog-meta-", Ignored),
        Equivalent::exact("x-amz-acl", canned_acl::HEADER, Ignored),
        Equivalent::exact("x-amz-storage-class", "x-goog-storage-class", Ignored),
        Equivalent::exact(
            "x-amz-metadata-directive",
            "x-goog-metadata-directive",
            Ignored,
        ),
        Equivalent::exact("x-amz-copy-source", "x-goog-copy-source", MissingCopySource),
        Equivalent::exact(
            "x-amz-copy-source-if-match",
            "x-goog-copy-source-if-match",
            Unchanged,
        ),
        Equivalent::exact(
            "x-amz-copy-source-if-modified-since",
            "x-goog-copy-source-if-modified-since",
            Unchanged,
        ),
        Equivalent::exact(
            "x-amz-copy-source-if-none-match",
            "x-goog-copy-source-if-none-match",
            Unchanged,
        ),
        Equivalent::exact(
            "x-amz-copy-source-if-unmodified-since",
            "x-goog-copy-source-if-unmodified-since",
            Unchanged,
        ),
        Equivalent::exact("x-amz-date", "x-goog-date", Unchanged),
        Equivalent::exact("x-amz-project-id", "x-goog-project-id", Unchanged),
    ]
};

/// The table row covering a header name, and the part of the name after the row's `amz`.
fn equivalent(name: &str) -> Option<(&'static Equivalent, &str)> {
    EQUIVALENTS.iter().find_map(|row| {
        let suffix = name.strip_prefix(row.amz)?;
        match row.names {
            Names::Exact if !suffix.is_empty() => None,
            Names::Exact | Names::Prefix => Some((row, suffix)),
        }
    })
}

pub(crate) fn normalize<B>(request: &mut Request<B>, route: Route) -> Result<(), Error> {
    let dialect = ExtensionDialect::of(request.uri().query(), request.headers());
    let writes_object = route.writes_object(request.method());
    let headers = request.headers_mut();
    match dialect {
        ExtensionDialect::Aws => map_to_google(headers),
        ExtensionDialect::Google if writes_object => apply_without_aws_signature(headers),
        ExtensionDialect::Google => Ok(()),
    }
}

/// Cloud Storage's `Details` for both spellings of one header (recorded 2026-11-09).
pub(super) const BOTH_SPELLINGS: &str = "Multiple HTTP header values where one was expected. Requests cannot specify both x-amz and x-goog headers.";

/// Whether an AWS-signed request may carry this `x-amz-*` header with no effect. Cloud Storage
/// answered 200 to an unknown header, `x-amz-server-side-encryption: AES256` and
/// `x-amz-grant-read` (recorded 2026-11-09). It never accepted the customer-key headers as sent
/// (400 when unsigned, 403 when signed), so the adapters keep refusing them.
fn ignored_with_aws_signature(name: &str) -> bool {
    name.starts_with("x-amz-")
        && !name.starts_with("x-amz-server-side-encryption-customer-")
        && !crate::auth::signature::authentication_header(name)
        && name != "x-amz-decoded-content-length"
}

fn map_to_google(headers: &mut HeaderMap) -> Result<(), Error> {
    let mut replacements = Vec::new();
    let mut ignored = Vec::new();
    let copies = headers.contains_key("x-amz-copy-source");
    for name in headers.keys() {
        let Some((row, suffix)) = equivalent(name.as_str()) else {
            if ignored_with_aws_signature(name.as_str()) {
                ignored.push(name.clone());
            }
            continue;
        };
        // A directive has nothing to act on without a copy source; Cloud Storage answered 200
        // (recorded 2026-11-09).
        if row.amz == "x-amz-metadata-directive" && !copies {
            ignored.push(name.clone());
            continue;
        }
        let target = HeaderName::from_bytes(format!("{}{suffix}", row.goog).as_bytes())
            .map_err(|_| Error::unsupported(name.as_str()))?;
        if headers.contains_key(&target) {
            return Err(Error::excess_header_values(BOTH_SPELLINGS));
        }
        replacements.push((name.clone(), target));
    }
    for name in ignored {
        headers.remove(name);
    }
    for (raw, target) in replacements {
        let values: Vec<_> = headers.get_all(&raw).iter().cloned().collect();
        headers.remove(&raw);
        for value in values {
            headers.append(target.clone(), value);
        }
    }
    Ok(())
}

/// Spells custom metadata in an object read as the caller's dialect does: Cloud Storage answered
/// an AWS-signed read with `x-amz-meta-*` (recorded 2026-11-09).
pub(crate) fn spell_read_metadata(
    headers: &mut HeaderMap,
    dialect: ExtensionDialect,
) -> Result<(), Error> {
    match dialect {
        ExtensionDialect::Google => Ok(()),
        ExtensionDialect::Aws => {
            let renamed: Vec<_> = headers
                .keys()
                .filter_map(|name| {
                    let suffix = name.as_str().strip_prefix("x-goog-meta-")?;
                    Some((name.clone(), format!("x-amz-meta-{suffix}")))
                })
                .collect();
            for (name, spelled) in renamed {
                let target = HeaderName::from_bytes(spelled.as_bytes())
                    .map_err(|_| Error::unsupported(&spelled))?;
                if let Some(value) = headers.remove(&name) {
                    headers.insert(target, value);
                }
            }
            Ok(())
        }
    }
}

fn apply_without_aws_signature(headers: &mut HeaderMap) -> Result<(), Error> {
    let mut ignored = Vec::new();
    for name in headers.keys() {
        let Some((row, _)) = equivalent(name.as_str()) else {
            continue;
        };
        match row.without_aws_signature {
            WithoutAwsSignature::Ignored => ignored.push(name.clone()),
            WithoutAwsSignature::MissingCopySource => return Err(Error::missing_copy_source()),
            WithoutAwsSignature::Unchanged => {}
        }
    }
    for name in ignored {
        headers.remove(name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AWS4: &str = "AWS4-HMAC-SHA256 Credential=key/20261109/auto/s3/aws4_request";

    fn request(method: &str, uri: &str, headers: &[(&str, &str)]) -> Request<()> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn every_aws_signature_form_maps_the_same_headers() {
        for (uri, authorization) in [
            ("/b/o", AWS4),
            ("/b/o", "AWS key:signature"),
            ("/b/o?X-Amz-Algorithm=AWS4-HMAC-SHA256", ""),
            ("/b/o?x-amz-signature=abc", ""),
        ] {
            let mut headers = vec![
                ("x-amz-meta-origin", "s3"),
                ("x-amz-acl", "private"),
                ("x-amz-copy-source", "/b/source"),
                ("x-amz-copy-source-if-match", "etag"),
                ("x-amz-metadata-directive", "REPLACE"),
                ("x-amz-storage-class", "NEARLINE"),
                ("x-amz-project-id", "project"),
                ("x-amz-date", "20261109T000000Z"),
                ("x-amz-content-sha256", "UNSIGNED-PAYLOAD"),
                ("x-amz-decoded-content-length", "5"),
            ];
            if !authorization.is_empty() {
                headers.push(("authorization", authorization));
            }
            let mut mapped = request("PUT", uri, &headers);
            normalize(&mut mapped, Route::Object).unwrap();
            let headers = mapped.headers();
            for (name, value) in [
                ("x-goog-meta-origin", "s3"),
                ("x-goog-acl", "private"),
                ("x-goog-copy-source", "/b/source"),
                ("x-goog-copy-source-if-match", "etag"),
                ("x-goog-metadata-directive", "REPLACE"),
                ("x-goog-storage-class", "NEARLINE"),
                ("x-goog-project-id", "project"),
                ("x-goog-date", "20261109T000000Z"),
            ] {
                assert_eq!(headers[name], value, "{uri} {authorization} {name}");
            }
            // Signing inputs keep their names, and no mapped raw name survives.
            assert_eq!(headers["x-amz-content-sha256"], "UNSIGNED-PAYLOAD");
            assert_eq!(headers["x-amz-decoded-content-length"], "5");
            assert!(!headers.contains_key("x-amz-meta-origin"));
            assert!(!headers.contains_key("x-amz-acl"));
        }
    }

    #[test]
    fn repeated_values_keep_their_order_when_mapped() {
        let mut mapped = request(
            "PUT",
            "/b/o",
            &[
                ("authorization", AWS4),
                ("x-amz-meta-k", "first"),
                ("x-amz-meta-k", "second"),
            ],
        );
        normalize(&mut mapped, Route::Object).unwrap();
        assert_eq!(
            mapped
                .headers()
                .get_all("x-goog-meta-k")
                .iter()
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn an_aws_signature_refuses_both_spellings_of_one_header() {
        for (amz, goog) in [
            ("x-amz-meta-k", "x-goog-meta-k"),
            ("x-amz-acl", "x-goog-acl"),
            ("x-amz-storage-class", "x-goog-storage-class"),
            ("x-amz-copy-source", "x-goog-copy-source"),
            ("x-amz-project-id", "x-goog-project-id"),
        ] {
            let mut both = request(
                "PUT",
                "/b/o",
                &[("authorization", AWS4), (amz, "a"), (goog, "b")],
            );
            let error = normalize(&mut both, Route::Object).unwrap_err();
            assert_eq!(error.status(), hyper::StatusCode::BAD_REQUEST, "{amz}");
            assert_eq!(error.details.as_deref(), Some(BOTH_SPELLINGS), "{amz}");
        }
    }

    #[test]
    fn an_aws_signature_ignores_headers_without_an_equivalent() {
        for name in [
            "x-amz-server-side-encryption",
            "x-amz-tagging",
            "x-amz-grant-read",
            "x-amz-version-id",
            "x-amz-acl-extra",
            "x-amz-copy-source-range",
            "x-amz-no-such-header",
        ] {
            let mut unmapped = request("PUT", "/b/o", &[("authorization", AWS4), (name, "v")]);
            normalize(&mut unmapped, Route::Object).unwrap();
            assert!(!unmapped.headers().contains_key(name), "{name}");
        }
    }

    #[test]
    fn an_aws_signature_leaves_customer_keys_for_the_adapters_to_refuse() {
        for name in [
            "x-amz-server-side-encryption-customer-key",
            "x-amz-decoded-content-length",
            "x-amz-content-sha256",
        ] {
            let mut kept = request("PUT", "/b/o", &[("authorization", AWS4), (name, "v")]);
            normalize(&mut kept, Route::Object).unwrap();
            assert!(kept.headers().contains_key(name), "{name}");
        }
    }

    #[test]
    fn read_metadata_is_spelled_for_the_callers_dialect() {
        let stored = || {
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-goog-meta-origin",
                hyper::header::HeaderValue::from_static("a"),
            );
            headers.insert(
                "x-goog-generation",
                hyper::header::HeaderValue::from_static("1"),
            );
            headers
        };
        let mut aws = stored();
        spell_read_metadata(&mut aws, ExtensionDialect::Aws).unwrap();
        assert_eq!(aws["x-amz-meta-origin"], "a");
        assert!(!aws.contains_key("x-goog-meta-origin"));
        assert_eq!(aws["x-goog-generation"], "1");

        let mut google = stored();
        spell_read_metadata(&mut google, ExtensionDialect::Google).unwrap();
        assert_eq!(google["x-goog-meta-origin"], "a");
        assert!(!google.contains_key("x-amz-meta-origin"));
    }

    #[test]
    fn writes_without_an_aws_signature_ignore_the_recorded_headers() {
        for (uri, authorization) in [
            ("/b/o", "Bearer token"),
            ("/b/o", "GOOG4-HMAC-SHA256 Credential=key"),
            ("/b/o", "GOOG1 key:signature"),
            ("/b/o?X-Goog-Algorithm=GOOG4-HMAC-SHA256", ""),
            ("/b/o", ""),
        ] {
            for method in ["PUT", "POST"] {
                let mut headers = vec![
                    ("x-amz-meta-origin", "s3"),
                    ("x-goog-meta-origin", "goog"),
                    ("x-amz-acl", "public-read"),
                    ("x-amz-storage-class", "NEARLINE"),
                    ("x-amz-metadata-directive", "REPLACE"),
                    ("x-amz-server-side-encryption", "AES256"),
                ];
                if !authorization.is_empty() {
                    headers.push(("authorization", authorization));
                }
                let mut write = request(method, uri, &headers);
                normalize(&mut write, Route::Object).unwrap();
                let headers = write.headers();
                assert_eq!(headers["x-goog-meta-origin"], "goog");
                for ignored in [
                    "x-amz-meta-origin",
                    "x-amz-acl",
                    "x-amz-storage-class",
                    "x-amz-metadata-directive",
                    "x-goog-acl",
                    "x-goog-storage-class",
                ] {
                    assert!(!headers.contains_key(ignored), "{authorization} {ignored}");
                }
                assert!(headers.contains_key("x-amz-server-side-encryption"));
            }
        }
    }

    #[test]
    fn a_write_without_an_aws_signature_refuses_a_copy_source() {
        let mut write = request(
            "PUT",
            "/b/o",
            &[
                ("authorization", "Bearer token"),
                ("x-amz-copy-source", "/b/source"),
                ("x-amz-metadata-directive", "REPLACE"),
            ],
        );
        let error = normalize(&mut write, Route::Object).unwrap_err();
        assert_eq!(error.status(), hyper::StatusCode::BAD_REQUEST);
        assert_eq!(error.message, "Invalid argument.");
        assert_eq!(error.details.as_deref(), Some("Missing copy source"));
    }

    #[test]
    fn other_requests_without_an_aws_signature_keep_their_headers_for_the_existing_rejection() {
        for (method, route) in [
            ("GET", Route::Object),
            ("HEAD", Route::Object),
            ("DELETE", Route::Object),
            ("PUT", Route::Bucket),
        ] {
            let mut other = request(
                method,
                "/b/o",
                &[
                    ("authorization", "Bearer token"),
                    ("x-amz-acl", "private"),
                    ("x-amz-copy-source", "/b/source"),
                ],
            );
            normalize(&mut other, route).unwrap();
            assert!(other.headers().contains_key("x-amz-acl"), "{method}");
            assert!(
                other.headers().contains_key("x-amz-copy-source"),
                "{method}"
            );
        }
    }
}
