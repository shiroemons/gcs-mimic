use super::*;
use crate::object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget};
use hyper::{HeaderMap, Method, StatusCode, header::HeaderName};
use serde_json::json;
use std::time::{Duration, UNIX_EPOCH};

#[track_caller]
fn request(method: Method, path: &str) -> Result<ObjectRequest, JsonError> {
    route_object(&method, &path.parse().unwrap()).expect("object route")
}

#[track_caller]
fn header_map<'a>(entries: impl IntoIterator<Item = (&'static str, &'a str)>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in entries {
        headers.append(HeaderName::from_static(name), value.parse().unwrap());
    }
    headers
}

pub(super) fn record() -> ObjectRecord {
    ObjectRecord {
        bucket: "test-bucket".to_owned(),
        name: "dir/猫 %2F?#".to_owned(),
        generation: 9_007_199_254_740_993,
        metageneration: 2,
        owner: None,
        content_type: "text/plain; charset=utf-8".to_owned(),
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: Default::default(),
        size: 5,
        md5: [0; 16],
        md5_availability: Default::default(),
        crc32c: 0x01020304,
        component_count: None,
        storage_class: crate::bucket::StorageClass::Standard,
        time_created: UNIX_EPOCH + Duration::from_millis(1234),
        updated: UNIX_EPOCH + Duration::from_millis(2345),
        time_deleted: None,
        soft_delete_time: None,
        hard_delete_time: None,
        event_based_hold: crate::object::Hold::Unset,
        temporary_hold: crate::object::Hold::Unset,
        custom_time: None,
        retention_expiration_time: None,
        retention: None,
        customer_key_sha256: None,
    }
}

#[test]
fn absent_md5_has_crc32c_and_opaque_etag_without_composite_metadata() {
    let mut object = record();
    object.md5_availability = crate::object::Md5Availability::Unavailable;
    let resource = object_resource(&object, "http://localhost", Projection::NoAcl).unwrap();
    assert!(resource.get("md5Hash").is_none());
    assert!(resource.get("componentCount").is_none());
    assert!(resource.get("md5_availability").is_none());
    let headers = object_media_headers(&object).unwrap();
    assert_eq!(headers["x-goog-hash"], "crc32c=AQIDBA==");
    assert!(!object_etag(&object).contains(&base64(&object.md5)));
    let key = crate::csek::CustomerKey::from_sha256([3; 32]);
    object.customer_key_sha256 = Some(key.sha256());
    let keyed = super::csek::object_resource_keyed(
        &object,
        "http://localhost",
        Projection::NoAcl,
        Some(key),
    )
    .unwrap();
    assert!(keyed.get("md5Hash").is_none());
    assert_eq!(keyed["crc32c"], "AQIDBA==");
    let mut old = serde_json::to_value(record()).unwrap();
    old.as_object_mut().unwrap().remove("md5_availability");
    let old: ObjectRecord = serde_json::from_value(old).unwrap();
    assert!(old.has_md5());
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
        ObjectRequest::Insert(UploadRequest {
            bucket: "abc".to_owned(), name: "dir/猫 %2F".to_owned(),
            params: ObjectInsertParams { conditions, projection: Projection::Full, predefined_acl: None }, content_encoding: None,
        })
    );
    for upload_type in ["media&name=file", "multipart", "resumable&name=file"] {
        let parsed = request(
            Method::POST,
            &format!(
                "/upload/storage/v1/b/abc/o?uploadType={upload_type}&predefinedAcl=publicRead"
            ),
        )
        .unwrap();
        let (ObjectRequest::Insert(UploadRequest { params, .. })
        | ObjectRequest::Multipart(UploadRequest { params, .. })
        | ObjectRequest::Resumable { params, .. }) = parsed
        else {
            panic!("{upload_type} parsed as {parsed:?}");
        };
        assert_eq!(
            params.predefined_acl,
            Some(crate::acl::PredefinedObjectAcl::PublicRead)
        );
        for invalid in ["publicReadWrite", "", "bogus"] {
            let error = request(
                Method::POST,
                &format!(
                    "/upload/storage/v1/b/abc/o?uploadType={upload_type}&predefinedAcl={invalid}"
                ),
            )
            .unwrap_err();
            assert_eq!(error.reason, "invalidParameter", "{upload_type} {invalid}");
        }
    }
    for prefix in ["", "/download"] {
        assert_eq!(
            request(Method::GET, &format!("{prefix}/storage/v1/b/abc/o/dir%2F%E7%8C%AB+%252F?alt=media&generation=7&{query}")).unwrap(),
            ObjectRequest::Get {
                target: ObjectTarget::new("abc", "dir/猫+%2F", Some(7)), conditions,
                params: ObjectGetParams { projection: Projection::NoAcl, media: true, soft_deleted: false },
            }
        );
    }
    assert!(matches!(
        request(Method::GET, "/storage/v1/b/abc/o/file?alt=json").unwrap(),
        ObjectRequest::Get {
            target: ObjectTarget {
                generation: None,
                ..
            },
            params: ObjectGetParams { media: false, .. },
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
            target: ObjectTarget::new("abc", "file", Some(7)),
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
    let error = request(Method::POST, "/storage/v1/b/abc/o/file").unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    let response = error.into_response();
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=UTF-8"
    );
    for (path, method, allow) in [
        ("/upload/storage/v1/b/abc/o", Method::GET, "POST"),
        ("/storage/v1/b/abc/o", Method::DELETE, "GET"),
        (
            "/download/storage/v1/b/abc/o/file",
            Method::DELETE,
            "GET, HEAD",
        ),
        ("/storage/v1/b/abc/o", Method::HEAD, "GET"),
    ] {
        let error = request(method, path).unwrap_err();
        assert_eq!(error.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(error.into_response().headers()["allow"], allow);
    }
    assert!(matches!(
        request(Method::HEAD, "/storage/v1/b/abc/o/file").unwrap(),
        ObjectRequest::Get {
            params: ObjectGetParams { media: false, .. },
            ..
        }
    ));
    for path in [
        "/storage/v1/b/abc/o/file?alt=media",
        "/download/storage/v1/b/abc/o/file?alt=media",
    ] {
        assert!(matches!(
            request(Method::HEAD, path).unwrap(),
            ObjectRequest::Get {
                params: ObjectGetParams { media: true, .. },
                ..
            }
        ));
    }
    assert_eq!(
        request(Method::HEAD, "/download/storage/v1/b/abc/o/file")
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
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
            matches!(request(Method::GET, &format!("/storage/v1/b/abc/o/{raw}")).unwrap(), ObjectRequest::Get { target, .. } if target.name == name)
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
    // An upload that names no type is a media upload (recorded 200).
    assert!(matches!(
        request(Method::POST, "/upload/storage/v1/b/abc/o?name=file"),
        Ok(ObjectRequest::Insert(_))
    ));
    for (query, status, reason) in [
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
    // An empty `fields` selects nothing, as Cloud Storage did.
    assert!(request(Method::GET, "/storage/v1/b/abc/o/file?fields=").is_ok());
    for key in [
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
                StatusCode::BAD_REQUEST,
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
    request(Method::GET, "/storage/v1/b/abc/o/file?prettyPrint=true").unwrap();
    assert_eq!(
        request(Method::GET, "/storage/v1/b/abc/o/file?uploadType=media")
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
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
    let headers = header_map([("if-range", "opaque")]);
    // Cloud Storage ignored an If-Range validator and applied the Range anyway.
    assert!(validate_object_get_headers(&headers).is_ok());
    let headers = header_map([("x-goog-meta-example", "value")]);
    assert_eq!(
        parse_object_upload_headers(&headers, Default::default())
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn obj_headers_upload_tokens_require_one_nonempty_value_with_or_without_conditions() {
    let headers = header_map([("x-goog-gcs-idempotency-token", "opaque, token=1")]);
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
        assert!(parse_object_upload_headers(&headers, conditions).is_ok());
    }
    for values in [
        vec![""],
        vec![" \t"],
        vec!["same", "same"],
        vec!["first", "second"],
    ] {
        let headers = header_map(
            values
                .iter()
                .map(|&value| ("x-goog-gcs-idempotency-token", value)),
        );
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
                (StatusCode::BAD_REQUEST, "invalidParameter"),
                "{values:?}: generation {generation:?}"
            );
        }
        let error = validate_object_get_headers(&headers).unwrap_err();
        assert_eq!(
            (error.status, error.reason),
            (StatusCode::BAD_REQUEST, "invalidParameter"),
            "{values:?}"
        );
    }
}

#[test]
fn obj_headers_001_upload_defaults_hashes_and_multiple_headers() {
    assert_eq!(
        parse_object_upload_headers(&HeaderMap::new(), Default::default()).unwrap(),
        // No Content-Type stores none.
        (String::new(), ExpectedHashes::default())
    );
    for combined in [true, false] {
        let mut entries = vec![("content-type", "text/plain; charset=utf-8")];
        if combined {
            entries.push((
                "x-goog-hash",
                "crc32c=AQIDBA==, md5=AAAAAAAAAAAAAAAAAAAAAA==",
            ));
        } else {
            entries.extend([
                ("x-goog-hash", "crc32c=AQIDBA=="),
                ("x-goog-hash", "md5=AAAAAAAAAAAAAAAAAAAAAA=="),
            ]);
        }
        let headers = header_map(entries);
        // Media uploads do not examine the header; resumable initiation still reads it.
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default()).unwrap(),
            (
                "text/plain; charset=utf-8".to_owned(),
                ExpectedHashes::default()
            ),
            "combined: {combined}"
        );
        assert_eq!(
            declared_hashes(&headers).unwrap(),
            ExpectedHashes {
                md5: Some([0; 16]),
                crc32c: Some(0x01020304)
            },
            "combined: {combined}"
        );
    }
    let headers = header_map([("x-goog-hash", "md5=1B2M2Y8AsgTpgAmY7PhCfg==")]);
    assert_eq!(
        declared_hashes(&headers).unwrap().md5.unwrap(),
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
        let headers = header_map([("x-goog-hash", value)]);
        assert_eq!(
            declared_hashes(&headers).unwrap_err().status,
            StatusCode::BAD_REQUEST,
            "{value}"
        );
        // The media-upload parser never looks at the header.
        assert!(
            parse_object_upload_headers(&headers, Default::default()).is_ok(),
            "{value}"
        );
    }
    // Upload routes parse customer-supplied keys themselves; incomplete keys are rejected there.
    for (name, value, reason) in [
        (
            "x-goog-encryption-key",
            "value",
            "customerEncryptionAlgorithmIsInvalid",
        ),
        (
            "x-goog-encryption-algorithm",
            "AES256",
            "customerEncryptionKeyFormatIsInvalid",
        ),
        (
            "x-goog-encryption-algorithm",
            "AES128",
            "customerEncryptionAlgorithmIsInvalid",
        ),
    ] {
        let headers = header_map([(name, value)]);
        assert!(
            parse_object_upload_headers(&headers, Default::default()).is_ok(),
            "{name}"
        );
        let error = super::request_key(&headers).unwrap_err();
        assert_eq!(
            (error.status, error.reason),
            (StatusCode::BAD_REQUEST, reason),
            "{name}"
        );
    }
    // Source keys and customer-managed (KMS) key names are not upload key headers.
    for name in [
        "x-goog-copy-source-encryption-algorithm",
        "x-goog-encryption-kms-key-name",
    ] {
        let headers = header_map([(name, "AES256")]);
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default())
                .unwrap_err()
                .reason,
            "badRequest",
            "{name}"
        );
    }
    for (name, value) in [
        ("content-encoding", "gzip"),
        ("content-range", "bytes 0-1/2"),
        ("x-goog-user-project", "p"),
        ("x-http-method-override", "PATCH"),
    ] {
        let headers = header_map([(name, value)]);
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default())
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST,
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
        let headers = header_map([("content-type", value)]);
        assert!(
            parse_object_upload_headers(&headers, Default::default()).is_err(),
            "{value}"
        );
    }
    let headers = header_map([
        ("content-type", "text/plain"),
        ("content-type", "text/html"),
    ]);
    assert!(parse_object_upload_headers(&headers, Default::default()).is_err());
    let headers = header_map([
        ("x-goog-hash", "crc32c=AQIDBA=="),
        ("x-goog-hash", "crc32c=AQIDBA=="),
    ]);
    assert!(declared_hashes(&headers).is_err());
}

#[test]
fn obj_headers_003_range_is_accepted_and_unsupported_conditionals_are_rejected() {
    for value in ["bytes=0-1", "", "invalid"] {
        let headers = header_map([("range", value)]);
        assert!(validate_object_get_headers(&headers).is_ok(), "{value}");
    }
    for key in ["if-match", "if-none-match"] {
        let headers = header_map([(key, "\"value\"")]);
        assert!(validate_object_get_headers(&headers).is_ok(), "{key}");
        assert_eq!(
            parse_object_upload_headers(&headers, Default::default())
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST,
            "{key}"
        );
    }
    let headers = header_map([("x-goog-encryption-key-sha256", "value")]);
    assert_eq!(
        validate_object_get_headers(&headers).unwrap_err().status,
        StatusCode::BAD_REQUEST
    );
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
        "etag":"CIGAgICAgIAQEAI=",
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
    changed.time_created = UNIX_EPOCH - Duration::from_nanos(100);
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
        ("x-goog-generation", "9007199254740993"),
        ("x-goog-metageneration", "2"),
        (
            "x-goog-hash",
            "crc32c=AQIDBA==,md5=AAAAAAAAAAAAAAAAAAAAAA==",
        ),
        ("x-goog-stored-content-length", "5"),
        ("x-goog-stored-content-encoding", "identity"),
        ("etag", "CIGAgICAgIAQEAI="),
    ] {
        assert_eq!(headers[name], value);
    }
    // Cloud Storage sends no accept-ranges on a JSON media response (observed 2026-10-31).
    assert!(!headers.contains_key("accept-ranges"));
    assert_eq!(
        headers["cache-control"],
        "no-cache, no-store, max-age=0, must-revalidate"
    );
    assert_eq!(headers.len(), 9);
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
            "The specified bucket does not exist.",
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
            "At least one of the pre-conditions you specified did not hold.",
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
            ObjectError::BodyAborted("private transport error".into()),
            400,
            "badRequest",
            "The upload request body did not complete",
        ),
        (
            ObjectError::CustomerEncryption(crate::csek::CsekError::ResourceIsEncrypted),
            400,
            "resourceIsEncryptedWithCustomerEncryptionKey",
            "The resource is encrypted with a customer-supplied encryption key, but the request did not provide one.",
        ),
        (
            ObjectError::CustomerEncryption(crate::csek::CsekError::KeyIncorrect),
            400,
            "customerEncryptionKeyIsIncorrect",
            "The provided encryption key is incorrect.",
        ),
        (
            ObjectError::Unavailable("data/catalog.sqlite3".into()),
            503,
            "backendError",
            "Object storage is unavailable: internal storage failure (private backend details withheld)",
        ),
    ] {
        let error = JsonError::from(error);
        assert_eq!(error.status.as_u16(), status);
        // Cloud Storage names the `If-Match` header for every failed precondition.
        let mut item = json!({"message":message,"domain":"global","reason":reason});
        if status == 412 && reason == "conditionNotMet" {
            item["locationType"] = "header".into();
            item["location"] = "If-Match".into();
        }
        assert_eq!(
            error.value(),
            json!({"error":{"code":status,"message":message,"errors":[item]}})
        );
        let response = error.into_response();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(
            response.headers()["content-type"],
            "application/json; charset=UTF-8"
        );
    }
}

#[test]
fn obj_csek_001_encrypted_resource_hides_hashes_without_its_key() {
    let mut object = record();
    let sha256 = crate::csek::key_sha256(&[9; 32]);
    object.customer_key_sha256 = Some(sha256);
    let value = object_resource(&object, "http://localhost", Projection::NoAcl).unwrap();
    assert_eq!(
        value["customerEncryption"],
        json!({"encryptionAlgorithm": "AES256", "keySha256": base64(&sha256)})
    );
    assert!(value.get("md5Hash").is_none() && value.get("crc32c").is_none());
    // The validator must not expose the plaintext MD5 either.
    assert!(
        !value["etag"]
            .as_str()
            .unwrap()
            .contains(&base64(&object.md5))
    );
    let key = crate::csek::CustomerKey::from_sha256(sha256);
    let keyed =
        object_resource_keyed(&object, "http://localhost", Projection::NoAcl, Some(key)).unwrap();
    assert_eq!(keyed["md5Hash"], base64(&object.md5));
    assert_eq!(keyed["crc32c"], base64(&object.crc32c.to_be_bytes()));
    let other = crate::csek::CustomerKey::from_sha256([0; 32]);
    let wrong =
        object_resource_keyed(&object, "http://localhost", Projection::NoAcl, Some(other)).unwrap();
    assert!(wrong.get("md5Hash").is_none());
    let plain = object_resource(&record(), "http://localhost", Projection::NoAcl).unwrap();
    assert!(plain.get("customerEncryption").is_none() && plain.get("md5Hash").is_some());
}

#[test]
fn obj_csek_002_metadata_update_accepts_only_echoed_customer_encryption() {
    let mut object = record();
    object.customer_key_sha256 = Some(crate::csek::key_sha256(&[9; 32]));
    let echoed = object_resource(&object, "http://localhost", Projection::NoAcl).unwrap();
    let body = json!({"customerEncryption": echoed["customerEncryption"]}).to_string();
    let request = parse_object_metadata(body.as_bytes(), WriteMode::Patch).unwrap();
    assert!(
        ObjectMetadataRequest::validate_unchanged(&request.unchanged, &object, "http://localhost")
            .is_ok()
    );
    let changed =
        json!({"customerEncryption": {"encryptionAlgorithm": "AES256", "keySha256": "AAAA"}});
    let request = parse_object_metadata(changed.to_string().as_bytes(), WriteMode::Patch).unwrap();
    assert!(
        ObjectMetadataRequest::validate_unchanged(&request.unchanged, &object, "http://localhost")
            .is_err()
    );
}

/// A Pub/Sub JSON payload drops the listener-dependent links without reordering the other
/// fields: `serde_json` removes by swapping the last field into the gap unless told to shift.
#[test]
fn notification_payload_keeps_the_resource_field_order_after_dropping_links() {
    let object = record();
    let message = crate::notification::Message {
        event: crate::notification::Event::Finalize,
        object: &object,
        time: UNIX_EPOCH,
        replacement: None,
    };
    let config = crate::notification::NotificationConfig {
        topic: "//pubsub.googleapis.com/projects/p/topics/t".into(),
        payload_format: "JSON_API_V1".into(),
        event_types: vec![],
        custom_attributes: Default::default(),
        object_name_prefix: String::new(),
    };
    let encoded: serde_json::Value =
        serde_json::from_str(&message.encode(&config, None).unwrap()).unwrap();
    let mut expected = serde_json::Map::new();
    for (key, value) in object_resource(&object, "", Projection::NoAcl)
        .unwrap()
        .as_object()
        .unwrap()
    {
        if key != "selfLink" && key != "mediaLink" {
            expected.insert(key.clone(), value.clone());
        }
    }
    assert_eq!(
        encoded["messages"][0]["data"],
        base64(serde_json::Value::Object(expected).to_string().as_bytes())
    );
}

/// With a bucket policy and an object retention both in force, the denial names the later
/// deadline whichever of the two it is (an inference; one source was observed at a time).
#[test]
fn retained_denial_names_the_later_of_policy_and_object_retention() {
    use crate::object::retention::{ObjectRetention, RetentionMode};
    let at = |seconds| UNIX_EPOCH + Duration::from_secs(seconds);
    let nanos = |seconds| crate::object::unix_nanos::UnixNanos::from(at(seconds));
    let until = |error| match error {
        ObjectError::ObjectRetentionNotMet(retained) => retained.until,
        other => panic!("unexpected {other:?}"),
    };
    for (policy, object) in [(2000, 3000), (3000, 2000)] {
        let mut record = record();
        record.retention_expiration_time = Some(at(policy));
        record.retention = Some(ObjectRetention {
            mode: RetentionMode::Unlocked,
            retain_until: nanos(object),
        });
        let error = record.check_retention(at(1000)).unwrap_err();
        assert_eq!(until(error), nanos(3000), "policy {policy} object {object}");
        let error = record.check_retention(at(2500)).unwrap_err();
        assert_eq!(until(error), nanos(3000), "policy {policy} object {object}");
        assert!(record.check_retention(at(3000)).is_ok());
    }
}
