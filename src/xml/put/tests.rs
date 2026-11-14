use super::*;

/// Which headers each XML write and read admits, by name. Signing headers pass only on a signed
/// request, and each operation has its own list of other `x-goog-*` names. Each row reads: PUT
/// unsigned/signed, resumable initiation unsigned/signed, then reads and strict reads.
#[test]
fn header_acceptance_matches_each_operation() {
    let empty_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    for (name, value, expected) in [
        (
            "x-goog-meta-a",
            "v",
            "put accept/accept resumable accept/accept read accept strict reject",
        ),
        (
            "x-goog-meta-",
            "v",
            "put accept/accept resumable accept/accept read accept strict reject",
        ),
        (
            "x-goog-date",
            "20261023T120000Z",
            "put reject/accept resumable reject/accept read accept strict accept",
        ),
        (
            "x-amz-date",
            "20261023T120000Z",
            "put reject/accept resumable reject/accept read accept strict accept",
        ),
        (
            "x-goog-content-sha256",
            empty_sha256,
            "put reject/accept resumable reject/accept read accept strict accept",
        ),
        (
            "x-amz-content-sha256",
            empty_sha256,
            "put reject/accept resumable reject/accept read accept strict accept",
        ),
        (
            "x-amz-decoded-content-length",
            "0",
            "put reject/accept resumable reject/reject read reject strict reject",
        ),
        (
            "x-goog-encryption-key",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "put accept/accept resumable accept/accept read accept strict accept",
        ),
        (
            "x-goog-user-project",
            "project",
            "put accept/accept resumable accept/accept read accept strict accept",
        ),
        (
            "x-goog-hash",
            "crc32c=AAAAAA==",
            "put accept/accept resumable reject/reject read accept strict reject",
        ),
        (
            "x-goog-storage-class",
            "STANDARD",
            "put accept/accept resumable accept/accept read accept strict reject",
        ),
        (
            "x-goog-resumable",
            "start",
            "put reject/reject resumable accept/accept read accept strict reject",
        ),
        (
            "x-goog-encryption-kms-key-name",
            "projects/p/locations/l/keyRings/r/cryptoKeys/k",
            "put reject/reject resumable reject/reject read reject strict reject",
        ),
        (
            "x-amz-acl",
            "private",
            "put reject/reject resumable reject/reject read reject strict reject",
        ),
        (
            "if-match",
            "\"etag\"",
            "put reject/reject resumable reject/reject read accept strict reject",
        ),
        (
            "if-range",
            "\"etag\"",
            "put reject/reject resumable reject/reject read reject strict reject",
        ),
        (
            "range",
            "bytes=0-1",
            "put reject/reject resumable reject/reject read accept strict accept",
        ),
        (
            "content-md5",
            "1B2M2Y8AsgTpgAmY7PhCfg==",
            "put accept/accept resumable reject/reject read accept strict accept",
        ),
        (
            "trailer",
            "x-goog-hash",
            "put reject/reject resumable reject/reject read accept strict accept",
        ),
        (
            "x-other",
            "1",
            "put accept/accept resumable accept/accept read accept strict accept",
        ),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::HeaderName::from_static(name),
            value.parse().unwrap(),
        );
        let verdict = |accepted: bool| if accepted { "accept" } else { "reject" };
        let resumable = |signing| {
            verdict(super::super::resumable::reject_unsupported_headers(&headers, signing).is_ok())
        };
        let read = |policy| verdict(super::super::headers(&headers, policy).is_ok());
        let table = format!(
            "put {}/{} resumable {}/{} read {} strict {}",
            verdict(reject_unsupported_headers(&headers, Signing::Unsigned).is_ok()),
            verdict(reject_unsupported_headers(&headers, Signing::Signed).is_ok()),
            resumable(Signing::Unsigned),
            resumable(Signing::Signed),
            read(super::super::HeaderPolicy::Read),
            read(super::super::HeaderPolicy::Strict),
        );
        assert_eq!(table, expected, "{name}");
    }
}

#[test]
fn storage_class_header_keeps_error_contracts() {
    for class in ["REGIONAL", "standard", "UNKNOWN"] {
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-storage-class", class.parse().unwrap());
        let error = metadata(&headers).unwrap_err();
        assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{class}");
        assert_eq!(error.code, XmlCode::InvalidArgument, "{class}");
        assert_eq!(
            error.message,
            if class == "REGIONAL" {
                "gcs-mimic does not support XML API storageClass=REGIONAL: storageClass requires a region or zone; omit this option or use a supported operation (for example, GET /BUCKET/OBJECT without extra query parameters)."
            } else {
                "Invalid x-goog-storage-class; use STANDARD, NEARLINE, COLDLINE or ARCHIVE."
            },
            "{class}"
        );
    }
}

#[test]
fn empty_request_framing_keeps_header_validation() {
    for (max, framing) in [
        (0, Framing::OptionalEmpty),
        (0, Framing::Required),
        (1, Framing::Required),
    ] {
        for (values, code) in [
            (
                vec![],
                (framing == Framing::Required).then_some(XmlCode::MissingContentLength),
            ),
            (vec![("content-length", "0")], None),
            (vec![("transfer-encoding", "chunked")], None),
            (
                vec![("content-length", "invalid")],
                Some(XmlCode::InvalidArgument),
            ),
            (
                vec![("transfer-encoding", "gzip")],
                Some(XmlCode::InvalidArgument),
            ),
            (
                vec![("content-length", "0"), ("transfer-encoding", "chunked")],
                Some(XmlCode::InvalidArgument),
            ),
            (vec![("content-length", "2")], Some(XmlCode::EntityTooLarge)),
            (
                vec![("content-length", "0"), ("content-length", "0")],
                Some(XmlCode::ExcessHeaderValues),
            ),
            (
                vec![
                    ("content-length", "0"),
                    ("x-amz-decoded-content-length", "2"),
                ],
                Some(XmlCode::EntityTooLarge),
            ),
        ] {
            let mut headers = HeaderMap::new();
            for (name, value) in values {
                headers.append(name, value.parse().unwrap());
            }
            assert_eq!(
                validate_framing(&headers, max, framing)
                    .err()
                    .map(|error| error.code),
                code,
                "{headers:?}, max={max}, framing={framing:?}"
            );
        }
    }
}
