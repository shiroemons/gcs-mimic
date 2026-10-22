use super::*;
use crate::object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord};
use hyper::{HeaderMap, Method, StatusCode, header::HeaderValue};
use serde_json::json;
use std::time::{Duration, UNIX_EPOCH};

fn request(method: Method, path: &str) -> Result<ObjectRequest, JsonError> {
    route_object(&method, &path.parse().unwrap()).expect("object route")
}

fn record() -> ObjectRecord {
    ObjectRecord {
        bucket: "test-bucket".to_owned(),
        name: "dir/猫 %2F?#".to_owned(),
        generation: 9_007_199_254_740_993,
        metageneration: 2,
        content_type: "text/plain; charset=utf-8".to_owned(),
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: Default::default(),
        size: 5,
        md5: [0; 16],
        crc32c: 0x01020304,
        storage_class: "STANDARD".to_owned(),
        time_created: UNIX_EPOCH + Duration::from_millis(1234),
        updated: UNIX_EPOCH + Duration::from_millis(2345),
    }
}

#[test]
fn obj_route_001_media_upload_metadata_download_delete() {
    let conditions = ObjectConditions {
        if_generation_match: Some(0),
        if_generation_not_match: Some(5),
        if_metageneration_match: Some(2),
        if_metageneration_not_match: Some(3),
    };
    let query = "ifGenerationMatch=0&ifGenerationNotMatch=5&ifMetagenerationMatch=2&ifMetagenerationNotMatch=3";
    assert_eq!(
        request(Method::POST, &format!("/upload/storage/v1/b/abc/o?uploadType=media&name=dir%2F%E7%8C%AB+%252F&projection=full&{query}")).unwrap(),
        ObjectRequest::Insert {
            bucket: "abc".to_owned(), name: "dir/猫 %2F".to_owned(),
            params: ObjectInsertParams { conditions, projection: Projection::Full },
        }
    );
    for prefix in ["", "/download"] {
        assert_eq!(
            request(Method::GET, &format!("{prefix}/storage/v1/b/abc/o/dir%2F%E7%8C%AB+%252F?alt=media&generation=7&{query}")).unwrap(),
            ObjectRequest::Get {
                bucket: "abc".to_owned(), name: "dir/猫+%2F".to_owned(),
                params: ObjectGetParams { generation: Some(7), conditions, projection: Projection::NoAcl, media: true },
            }
        );
    }
    assert!(matches!(
        request(Method::GET, "/storage/v1/b/abc/o/file?alt=json").unwrap(),
        ObjectRequest::Get {
            params: ObjectGetParams {
                media: false,
                generation: None,
                ..
            },
            ..
        }
    ));
    assert_eq!(
        request(
            Method::DELETE,
            &format!("/storage/v1/b/abc/o/file?generation=7&{query}")
        )
        .unwrap(),
        ObjectRequest::Delete {
            bucket: "abc".to_owned(),
            name: "file".to_owned(),
            generation: Some(7),
            conditions
        }
    );
}

#[test]
fn obj_route_002_namespace_and_method_boundaries() {
    for path in [
        "/",
        "/storage/v1/b",
        "/storage/v1/b/abc",
        "/storage/v1/b/abc/o/name/acl",
        "/upload/storage/v1/b/abc/o/name",
        "/storage/v1/b/abc/o/name/",
        "/download/storage/v1/b/abc",
    ] {
        assert!(
            route_object(&Method::GET, &path.parse().unwrap()).is_none(),
            "{path}"
        );
    }
    for (path, method, allow) in [
        ("/upload/storage/v1/b/abc/o", Method::GET, "POST"),
        ("/storage/v1/b/abc/o", Method::DELETE, "GET"),
        (
            "/storage/v1/b/abc/o/file",
            Method::POST,
            "GET, DELETE, PATCH, PUT",
        ),
        ("/download/storage/v1/b/abc/o/file", Method::DELETE, "GET"),
        (
            "/storage/v1/b/abc/o/file",
            Method::HEAD,
            "GET, DELETE, PATCH, PUT",
        ),
    ] {
        let error = request(method, path).unwrap_err();
        assert_eq!(error.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(error.into_response().headers()["allow"], allow);
    }
    assert!(matches!(
        request(Method::GET, "/storage/v1/b/abc/o").unwrap(),
        ObjectRequest::List(_)
    ));
    assert!(matches!(
        super::route(&Method::GET, &"/storage/v1/b/abc".parse().unwrap())
            .unwrap()
            .unwrap(),
        BucketRequest::Get { .. }
    ));
}

#[test]
fn obj_name_001_exactly_once_decoding_and_utf8_boundaries() {
    for (raw, name) in [
        ("a%2Fb", "a/b"),
        ("a%252Fb", "a%2Fb"),
        ("%E7%8C%AB", "猫"),
        ("%00", "\0"),
        ("%23%5B%5D%2A%3F", "#[]*?"),
        ("a%2F..%2Fb", "a/../b"),
    ] {
        assert!(
            matches!(request(Method::GET, &format!("/storage/v1/b/abc/o/{raw}")).unwrap(), ObjectRequest::Get { name: decoded, .. } if decoded == name)
        );
    }
    for name in ["a".repeat(1024), "猫".repeat(341) + "a"] {
        assert!(validate_object_name(&name).is_ok());
    }
    for name in [
        "".to_owned(),
        ".".to_owned(),
        "..".to_owned(),
        ".well-known/acme-challenge/token".to_owned(),
        "a\rb".to_owned(),
        "a\nb".to_owned(),
        "a".repeat(1025),
        "猫".repeat(342),
    ] {
        assert_eq!(
            validate_object_name(&name).unwrap_err().status,
            StatusCode::BAD_REQUEST,
            "{name:?}"
        );
    }
    for raw in [
        "",
        "%",
        "%2",
        "%zz",
        "%FF",
        "%C0%AF",
        "%ED%A0%80",
        "%2E",
        "%2E%2E",
        "a%0Ab",
        "a%0Db",
        ".well-known%2Facme-challenge%2Fx",
    ] {
        assert_eq!(
            request(Method::GET, &format!("/storage/v1/b/abc/o/{raw}"))
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST,
            "{raw}"
        );
    }
    for raw in ["a".repeat(1024), "%61".repeat(1024)] {
        assert!(request(Method::GET, &format!("/storage/v1/b/abc/o/{raw}")).is_ok());
    }
    for raw in ["a".repeat(1025), "%61".repeat(1025)] {
        assert!(request(Method::GET, &format!("/storage/v1/b/abc/o/{raw}")).is_err());
    }
    for bucket in ["a", "ABC", "a%2Fb", "a%FF", "a%252Fb", "a%"] {
        assert!(request(Method::GET, &format!("/storage/v1/b/{bucket}/o/file")).is_err());
    }
}

#[test]
fn obj_query_001_required_upload_and_unsupported_types() {
    for (query, status, reason) in [
        ("name=file", StatusCode::BAD_REQUEST, "required"),
        ("uploadType=media", StatusCode::BAD_REQUEST, "required"),
        (
            "uploadType=media&name=",
            StatusCode::BAD_REQUEST,
            "invalidParameter",
        ),
        ("uploadType=", StatusCode::BAD_REQUEST, "invalidParameter"),
        (
            "uploadType=invalid",
            StatusCode::BAD_REQUEST,
            "invalidParameter",
        ),
        (
            "uploadType=resumable",
            StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
        ),
        (
            "uploadType=media&name=file&generation=1",
            StatusCode::BAD_REQUEST,
            "invalidParameter",
        ),
        (
            "uploadType=media&name=file&name=other",
            StatusCode::BAD_REQUEST,
            "invalidParameter",
        ),
    ] {
        let error =
            request(Method::POST, &format!("/upload/storage/v1/b/abc/o?{query}")).unwrap_err();
        assert_eq!((error.status, error.reason), (status, reason), "{query}");
    }
    for key in [
        "fields",
        "key",
        "oauth_token",
        "quotaUser",
        "userIp",
        "userProject",
        "callback",
        "contentEncoding",
        "kmsKeyName",
        "predefinedAcl",
        "restoreToken",
    ] {
        for value in ["", "false", "value"] {
            assert_eq!(
                request(
                    Method::GET,
                    &format!("/storage/v1/b/abc/o/file?{key}={value}")
                )
                .unwrap_err()
                .status,
                if ["contentEncoding", "kmsKeyName", "predefinedAcl"].contains(&key) {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::NOT_IMPLEMENTED
                },
                "{key}"
            );
        }
    }
}

#[test]
fn obj_query_002_duplicates_invalid_scalars_and_method_parameters() {
    for key in [
        "generation",
        "ifGenerationMatch",
        "ifGenerationNotMatch",
        "ifMetagenerationMatch",
        "ifMetagenerationNotMatch",
    ] {
        for value in [
            "",
            "-9223372036854775809",
            "%2B1",
            "1.0",
            "1e2",
            "9223372036854775808",
            "1%20",
            "abc",
        ] {
            assert_eq!(
                request(
                    Method::GET,
                    &format!("/storage/v1/b/abc/o/file?{key}={value}")
                )
                .unwrap_err()
                .status,
                StatusCode::BAD_REQUEST,
                "{key}={value}"
            );
        }
        assert!(
            request(
                Method::GET,
                &format!("/storage/v1/b/abc/o/file?{key}=9223372036854775807")
            )
            .is_ok()
        );
        assert!(
            request(
                Method::GET,
                &format!("/storage/v1/b/abc/o/file?{key}=1&{key}=2")
            )
            .is_err()
        );
    }
    for query in [
        "alt=json&%61lt=media",
        "alt=invalid",
        "projection=none",
        "projection=full&projection=noAcl",
        "softDeleted=yes",
        "prettyPrint=yes",
        "unknown=1",
        "name=file",
        "generation=%FF",
        "generation=%zz",
    ] {
        assert_eq!(
            request(Method::GET, &format!("/storage/v1/b/abc/o/file?{query}"))
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    for query in ["softDeleted=true", "prettyPrint=true", "uploadType=media"] {
        assert_eq!(
            request(Method::GET, &format!("/storage/v1/b/abc/o/file?{query}"))
                .unwrap_err()
                .status,
            if query == "uploadType=media" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::NOT_IMPLEMENTED
            }
        );
    }
    assert!(
        request(
            Method::GET,
            "/storage/v1/b/abc/o/file?softDeleted=false&prettyPrint=false"
        )
        .is_ok()
    );
    for query in ["projection=noAcl", "softDeleted=false", "name=file"] {
        assert!(request(Method::DELETE, &format!("/storage/v1/b/abc/o/file?{query}")).is_err());
    }
    assert!(
        request(
            Method::GET,
            &format!("/storage/v1/b/abc/o/file?{}", "a".repeat(16 * 1024 + 1))
        )
        .is_err()
    );
}

#[test]
fn obj_signed_int64_and_download_endpoint_regressions() {
    for key in [
        "generation",
        "ifGenerationMatch",
        "ifGenerationNotMatch",
        "ifMetagenerationMatch",
        "ifMetagenerationNotMatch",
    ] {
        for value in [i64::MIN, -1, 0, i64::MAX] {
            assert!(
                request(
                    Method::GET,
                    &format!("/storage/v1/b/abc/o/file?{key}={value}")
                )
                .is_ok()
            );
        }
    }
    for query in ["", "?alt=json"] {
        let error = request(
            Method::GET,
            &format!("/download/storage/v1/b/abc/o/file{query}"),
        )
        .unwrap_err();
        assert_eq!(
            (error.status, error.reason),
            (StatusCode::BAD_REQUEST, "notDownload")
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert("if-range", HeaderValue::from_static("opaque"));
    assert_eq!(
        validate_object_get_headers(&headers, true)
            .unwrap_err()
            .status,
        StatusCode::NOT_IMPLEMENTED
    );
    for header in ["x-goog-meta-example", "x-goog-gcs-idempotency-token"] {
        let mut headers = HeaderMap::new();
        headers.insert(header, HeaderValue::from_static("value"));
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default())
                .unwrap_err()
                .status,
            StatusCode::NOT_IMPLEMENTED
        );
    }
}

#[test]
fn obj_headers_upload_tokens_require_generation_match_and_one_nonempty_value() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-gcs-idempotency-token",
        HeaderValue::from_static("opaque, token=1"),
    );
    for generation in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert!(
            parse_object_upload_headers(
                &headers,
                ObjectConditions {
                    if_generation_match: Some(generation),
                    ..Default::default()
                }
            )
            .is_ok()
        );
    }
    for conditions in [
        ObjectConditions::default(),
        ObjectConditions {
            if_generation_not_match: Some(0),
            ..Default::default()
        },
        ObjectConditions {
            if_metageneration_match: Some(1),
            ..Default::default()
        },
        ObjectConditions {
            if_metageneration_not_match: Some(2),
            ..Default::default()
        },
    ] {
        assert_eq!(
            parse_object_upload_headers(&headers, conditions)
                .unwrap_err()
                .status,
            StatusCode::NOT_IMPLEMENTED
        );
    }
    for values in [
        vec![""],
        vec![" \t"],
        vec!["same", "same"],
        vec!["first", "second"],
    ] {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                "x-goog-gcs-idempotency-token",
                HeaderValue::from_str(value).unwrap(),
            );
        }
        for generation in [None, Some(0), Some(1)] {
            let error = parse_object_upload_headers(
                &headers,
                ObjectConditions {
                    if_generation_match: generation,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert_eq!(
                (error.status, error.reason),
                (StatusCode::BAD_REQUEST, "invalidParameter")
            );
        }
        for media in [false, true] {
            let error = validate_object_get_headers(&headers, media).unwrap_err();
            assert_eq!(
                (error.status, error.reason),
                (StatusCode::BAD_REQUEST, "invalidParameter")
            );
        }
    }
}

#[test]
fn obj_headers_001_upload_defaults_hashes_and_multiple_headers() {
    assert_eq!(
        parse_object_upload_headers(&HeaderMap::new(), Default::default()).unwrap(),
        (
            "application/octet-stream".to_owned(),
            ExpectedHashes::default()
        )
    );
    for combined in [true, false] {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        if combined {
            headers.insert(
                "x-goog-hash",
                HeaderValue::from_static("crc32c=AQIDBA==, md5=AAAAAAAAAAAAAAAAAAAAAA=="),
            );
        } else {
            headers.append("x-goog-hash", HeaderValue::from_static("crc32c=AQIDBA=="));
            headers.append(
                "x-goog-hash",
                HeaderValue::from_static("md5=AAAAAAAAAAAAAAAAAAAAAA=="),
            );
        }
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default()).unwrap(),
            (
                "text/plain; charset=utf-8".to_owned(),
                ExpectedHashes {
                    md5: Some([0; 16]),
                    crc32c: Some(0x01020304)
                }
            )
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-hash",
        HeaderValue::from_static("md5=1B2M2Y8AsgTpgAmY7PhCfg=="),
    );
    assert_eq!(
        parse_object_upload_headers(&headers, Default::default())
            .unwrap()
            .1
            .md5
            .unwrap(),
        [
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e
        ]
    );
}

#[test]
fn obj_headers_002_invalid_hashes_and_unsupported_semantics() {
    for value in [
        "",
        "crc32c",
        "crc32c=",
        "crc32c=AQIDBA",
        "crc32c=AQIDBA===",
        "crc32c=AQIDB!==",
        "crc32c=AQIDBB==",
        "crc32c=AA==AA==",
        "crc32c=AQIDBA==,",
        "crc32c=AQIDBA==,crc32c=AQIDBA==",
        "md5=AQIDBA==",
        "md5=AAAAAAAAAAAAAAAAAAAAAB==",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-hash", HeaderValue::from_str(value).unwrap());
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default())
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST,
            "{value}"
        );
    }
    for (name, value) in [
        ("x-goog-encryption-key", "value"),
        ("x-goog-encryption-algorithm", "AES256"),
        ("content-encoding", "gzip"),
        ("content-md5", "x"),
        ("content-range", "bytes 0-1/2"),
        ("x-goog-user-project", "p"),
        ("x-http-method-override", "PATCH"),
        ("x-goog-hash", "sha256=hash"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default())
                .unwrap_err()
                .status,
            StatusCode::NOT_IMPLEMENTED,
            "{name}"
        );
    }
    for value in [
        "",
        "text",
        "text/",
        "/plain",
        "text/a/b",
        "text/plain, text/html",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_str(value).unwrap());
        assert!(
            parse_object_upload_headers(&headers, Default::default()).is_err(),
            "{value}"
        );
    }
    let mut headers = HeaderMap::new();
    headers.append("content-type", HeaderValue::from_static("text/plain"));
    headers.append("content-type", HeaderValue::from_static("text/html"));
    assert!(parse_object_upload_headers(&headers, Default::default()).is_err());
    let mut headers = HeaderMap::new();
    headers.append("x-goog-hash", HeaderValue::from_static("crc32c=AQIDBA=="));
    headers.append("x-goog-hash", HeaderValue::from_static("crc32c=AQIDBA=="));
    assert!(parse_object_upload_headers(&headers, Default::default()).is_err());
}

#[test]
fn obj_headers_003_range_is_accepted_and_unsupported_conditionals_are_rejected() {
    for value in ["bytes=0-1", "", "invalid"] {
        let mut headers = HeaderMap::new();
        headers.insert("range", HeaderValue::from_str(value).unwrap());
        assert!(validate_object_get_headers(&headers, true).is_ok());
        assert!(validate_object_get_headers(&headers, false).is_ok());
    }
    for key in ["if-match", "if-none-match", "x-goog-encryption-key-sha256"] {
        let mut headers = HeaderMap::new();
        headers.insert(key, HeaderValue::from_static("value"));
        assert_eq!(
            validate_object_get_headers(&headers, false)
                .unwrap_err()
                .status,
            StatusCode::NOT_IMPLEMENTED
        );
    }
}

#[test]
fn obj_render_001_exact_resource_and_encoded_generation_links() {
    let object = record();
    let expected = json!({
        "kind":"storage#object", "id":"test-bucket/dir/猫 %2F?#/9007199254740993",
        "selfLink":"http://localhost:4443/base/storage/v1/b/test-bucket/o/dir%2F%E7%8C%AB%20%252F%3F%23",
        "mediaLink":"http://localhost:4443/base/download/storage/v1/b/test-bucket/o/dir%2F%E7%8C%AB%20%252F%3F%23?generation=9007199254740993&alt=media",
        "name":"dir/猫 %2F?#", "bucket":"test-bucket", "generation":"9007199254740993", "metageneration":"2",
        "contentType":"text/plain; charset=utf-8", "storageClass":"STANDARD", "size":"5",
        "md5Hash":"AAAAAAAAAAAAAAAAAAAAAA==", "crc32c":"AQIDBA==",
        "etag":"\"o-AAAAAAAAAAAAAAAAAAAAAA==-9007199254740993-2\"",
        "timeCreated":"1970-01-01T00:00:01.234Z", "updated":"1970-01-01T00:00:02.345Z", "timeStorageClassUpdated":"1970-01-01T00:00:01.234Z", "timeFinalized":"1970-01-01T00:00:01.234Z",
    });
    for projection in [Projection::NoAcl, Projection::Full] {
        assert_eq!(
            object_resource(&object, "http://localhost:4443/base/", projection).unwrap(),
            expected
        );
    }
    let mut changed = object.clone();
    changed.metageneration += 1;
    assert_ne!(
        object_resource(&object, "http://localhost", Projection::NoAcl).unwrap()["etag"],
        object_resource(&changed, "http://localhost", Projection::NoAcl).unwrap()["etag"]
    );
    changed.time_created = UNIX_EPOCH - Duration::from_nanos(1);
    assert_eq!(
        object_resource(&changed, "http://localhost", Projection::NoAcl).unwrap()["timeCreated"],
        "1969-12-31T23:59:59.999Z"
    );
}

#[test]
fn obj_render_002_media_headers_match_selected_generation_and_hashes() {
    let object = record();
    let headers = object_media_headers(&object).unwrap();
    for (name, value) in [
        ("content-type", "text/plain; charset=utf-8"),
        ("content-length", "5"),
        ("accept-ranges", "bytes"),
        ("x-goog-generation", "9007199254740993"),
        ("x-goog-metageneration", "2"),
        (
            "x-goog-hash",
            "crc32c=AQIDBA==,md5=AAAAAAAAAAAAAAAAAAAAAA==",
        ),
        ("x-goog-stored-content-length", "5"),
        ("x-goog-stored-content-encoding", "identity"),
        ("etag", "\"o-AAAAAAAAAAAAAAAAAAAAAA==-9007199254740993-2\""),
    ] {
        assert_eq!(headers[name], value);
    }
    assert_eq!(headers["cache-control"], "no-cache");
    assert_eq!(headers.len(), 10);
    let mut object = object;
    object.size = u64::MAX;
    assert_eq!(
        object_media_headers(&object).unwrap()["content-length"],
        u64::MAX.to_string()
    );
    object.content_type = "text/plain\r\nx-injected: yes".to_owned();
    assert_eq!(
        object_media_headers(&object).unwrap_err().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[test]
fn obj_error_001_status_reason_envelope_and_private_diagnostics() {
    for (error, status, reason, message) in [
        (
            ObjectError::BucketNotFound,
            404,
            "notFound",
            "Bucket not found",
        ),
        (
            ObjectError::ObjectNotFound,
            404,
            "notFound",
            "Object not found",
        ),
        (
            ObjectError::PreconditionFailed,
            412,
            "conditionNotMet",
            "Precondition failed",
        ),
        (
            ObjectError::ChecksumMismatch,
            400,
            "invalid",
            "The supplied checksum does not match the uploaded data",
        ),
        (
            ObjectError::TooLarge,
            413,
            "uploadTooLarge",
            "Object exceeds the upload size limit",
        ),
        (
            ObjectError::BodyAborted("private transport error".to_owned()),
            400,
            "badRequest",
            "The upload request body did not complete",
        ),
        (
            ObjectError::Unavailable("data/catalog.sqlite3".to_owned()),
            503,
            "backendError",
            "Object storage is unavailable",
        ),
    ] {
        let error = JsonError::from(error);
        assert_eq!(error.status.as_u16(), status);
        assert_eq!(
            error.value(),
            json!({"error":{"code":status,"message":message,"errors":[{"message":message,"domain":"global","reason":reason}]}})
        );
        let response = error.into_response();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(
            response.headers()["content-type"],
            "application/json; charset=UTF-8"
        );
    }
}
