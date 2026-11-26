//! Shared upload parser inputs whose refusals the adapters render.

use crate::{
    multipart::{Multipart, multipart_boundary},
    object::{
        media_headers::object_media_headers,
        upload_headers::{content_type, declared_hashes},
    },
    service::ServiceError,
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    HeaderMap,
    header::{CONTENT_TYPE, HeaderValue},
};

fn case(label: &'static str, error: impl Into<ServiceError>) -> (&'static str, ServiceError) {
    let error = error.into();
    crate::error::assert_distinct_causes(&error);
    (label, error)
}

pub(crate) const HASH_CASES: &[(&str, &str)] = &[
    ("malformed hash", "md5"),
    ("unsupported algorithm", "sha1=unused"),
    ("invalid digest", "md5=!"),
    ("duplicate algorithm", "crc32c=AAAAAA==,crc32c=!"),
];

pub(crate) fn hash_cases() -> Vec<(&'static str, ServiceError)> {
    let mut headers = HeaderMap::new();
    HASH_CASES
        .iter()
        .map(|&(label, value)| {
            headers.insert("x-goog-hash", HeaderValue::from_static(value));
            case(label, declared_hashes(&headers).unwrap_err())
        })
        .collect()
}

pub(crate) async fn cases() -> Vec<(&'static str, ServiceError)> {
    let mut cases = Vec::new();
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("bad"));
    cases.push(case(
        "invalid content type",
        content_type(&headers).unwrap_err(),
    ));
    headers.append(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    cases.push(case(
        "duplicate content type",
        content_type(&headers).unwrap_err(),
    ));
    for (label, value) in [
        ("invalid framing", "text/plain"),
        ("missing boundary", "multipart/related"),
        (
            "unsupported parameter",
            "multipart/related; boundary=b; start=part",
        ),
    ] {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(value));
        cases.push(case(label, multipart_boundary(&headers).unwrap_err()));
    }
    let mut parser = Multipart::related(
        Full::new(Bytes::from_static(b"--b\r\n\r\n")),
        &"b".parse().unwrap(),
    );
    parser.start().await.unwrap();
    parser.part_headers().await.unwrap();
    cases.push(case(
        "missing end boundary",
        parser.next_chunk().await.unwrap_err(),
    ));
    let mut object = crate::storage::test_fixtures::bare_object_record();
    object.content_type = "text/plain\nextra".into();
    cases.push(case(
        "stored header",
        object_media_headers(&object).unwrap_err(),
    ));
    cases
}
