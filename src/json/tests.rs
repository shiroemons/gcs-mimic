use super::*;
use crate::bucket::{BucketRecord, CatalogError};
use crate::config::CompatProfile;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{
    Method, StatusCode, Uri,
    body::{Body, Buf, Bytes, Frame},
    header,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn request(method: Method, path: &str) -> Result<BucketRequest, JsonError> {
    route(&method, &path.parse::<Uri>().unwrap()).expect("bucket route")
}

pub(super) fn record() -> BucketRecord {
    parse_insert(br#"{"name":"test-bucket"}"#, "test-project", UNIX_EPOCH).unwrap()
}

fn resource(bucket: &BucketRecord) -> Value {
    bucket_resource(bucket, "http://localhost:4443/", Projection::NoAcl).unwrap()
}

#[test]
fn lifecycle_insert_defaults_and_unknown_resource_fields() {
    for profile in [CompatProfile::Native, CompatProfile::LegacyEmulator] {
        for lifecycle in [
            json!([]),
            json!(false),
            json!({"rule":false}),
            json!({"rule":[{}]}),
            json!({"rule":[{"action":{"type":"Delete"},"condition":false}]}),
        ] {
            let bytes =
                serde_json::to_vec(&json!({"name":"lifecycle-invalid","lifecycle":lifecycle}))
                    .unwrap();
            assert!(
                buckets::parse_insert_with_profile(&bytes, "project", UNIX_EPOCH, profile, "US")
                    .is_err()
            );
            assert!(buckets::parse_bucket_patch(&bytes, profile).is_err());
            assert!(buckets::parse_bucket_replacement(&bytes, profile).is_err());
        }
        for lifecycle in [
            Value::Null,
            json!({}),
            json!({"rule":null}),
            json!({"rule":[]}),
            json!({"rule":[],"future":true}),
        ] {
            let bytes =
                serde_json::to_vec(&json!({"name":"lifecycle-defaults","lifecycle":lifecycle}))
                    .unwrap();
            let bucket =
                buckets::parse_insert_with_profile(&bytes, "project", UNIX_EPOCH, profile, "US")
                    .unwrap();
            assert!(bucket.lifecycle.is_empty());
            assert!(resource(&bucket).get("lifecycle").is_none());
        }
    }
    let bucket = parse_insert(&serde_json::to_vec(&json!({
        "name":"ruby-lifecycle", "lifecycle":{"rule":[{
            "action":{"type":"Delete","storageClass":null},
            "condition":{"age":30,"isLive":null,"createdBefore":null,"matchesPrefix":[],"matchesSuffix":[],"matchesStorageClass":[]}
        }]}
    })).unwrap(), "project", UNIX_EPOCH).unwrap();
    assert_eq!(
        resource(&bucket)["lifecycle"],
        json!({"rule":[{"action":{"type":"Delete"},"condition":{"age":30}}]})
    );
}

#[test]
fn bkt_route_001_exact_routes_and_method_variants() {
    assert!(matches!(
        request(Method::POST, "/storage/v1/b?project=p").unwrap(),
        BucketRequest::Insert(_)
    ));
    assert!(matches!(
        request(Method::GET, "/storage/v1/b?project=p").unwrap(),
        BucketRequest::List(_)
    ));
    assert!(
        matches!(request(Method::GET, "/storage/v1/b/abc").unwrap(), BucketRequest::Get { bucket, .. } if bucket == "abc")
    );
    assert!(
        matches!(request(Method::DELETE, "/storage/v1/b/abc").unwrap(), BucketRequest::Delete { bucket, .. } if bucket == "abc")
    );
    for path in [
        "/",
        "/storage/v1",
        "/storage/v1/b/",
        "/storage/v1/b/abc/",
        "/storage/v1/b/abc/o",
        "/storage/v1/bogus",
        "/storage/v1/%62",
        "/upload/storage/v1/b",
    ] {
        assert!(
            route(&Method::GET, &path.parse().unwrap()).is_none(),
            "{path}"
        );
    }
    assert!(request(Method::GET, &format!("/storage/v1/b/{}", "a".repeat(667))).is_err());
}

#[test]
fn bkt_route_002_path_decodes_only_after_splitting() {
    assert!(
        matches!(request(Method::GET, "/storage/v1/b/%61%62%63").unwrap(), BucketRequest::Get { bucket, .. } if bucket == "abc")
    );
    for path in [
        "abc%",
        "abc%2",
        "abc%zz",
        "abc%FF",
        "abc%2Fdef",
        "abc%252Fdef",
        "abc+def",
        "abc%00def",
        "%2e%2e%2fabc",
    ] {
        let error = request(Method::GET, &format!("/storage/v1/b/{path}")).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{path}");
    }
}

#[test]
fn bkt_route_003_wrong_method_has_allow_header() {
    for (path, allow) in [
        ("/storage/v1/b", "GET, POST"),
        ("/storage/v1/b/abc", "GET, DELETE, PATCH, PUT"),
    ] {
        for method in [Method::PUT, Method::PATCH, Method::HEAD, Method::OPTIONS] {
            if path.ends_with("/abc") && method == Method::PATCH {
                assert!(matches!(
                    request(method, path).unwrap(),
                    BucketRequest::Patch { .. }
                ));
                continue;
            }
            if path.ends_with("/abc") && method == Method::PUT {
                assert!(matches!(
                    request(method, path).unwrap(),
                    BucketRequest::Update {
                        params: GetParams {
                            projection: Projection::Full,
                            ..
                        },
                        ..
                    }
                ));
                continue;
            }
            let error = request(method, path).unwrap_err();
            assert_eq!(error.status, StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(error.reason, "methodNotAllowed");
            assert_eq!(error.into_response().headers()[header::ALLOW], allow);
        }
    }
}

#[test]
fn bkt_qry_001_duplicate_unknown_and_misplaced_parameters() {
    for query in [
        "project=p&project=p",
        "project=p&%70roject=p",
        "project=p&prefix=a&prefix=b",
        "project=p&wat=1",
        "project=p&ifMetagenerationMatch=1",
        "project=p&=x",
        "project=p&prefix=%ff",
        "project=p&prefix=%2",
    ] {
        assert_eq!(
            request(Method::GET, &format!("/storage/v1/b?{query}"))
                .unwrap_err()
                .reason,
            "invalidParameter",
            "{query}"
        );
    }
    assert!(request(Method::GET, "/storage/v1/b/abc?project=p").is_err());
    assert!(request(Method::DELETE, "/storage/v1/b/abc?projection=noAcl").is_err());
    assert!(request(Method::POST, "/storage/v1/b?project=p&prefix=a").is_err());
    assert!(
        request(Method::GET, "/storage/v1/b?project=p&&prefix=a+b&").is_ok_and(
            |request| matches!(request, BucketRequest::List(params) if params.prefix == "a b")
        )
    );
}

#[test]
fn bkt_qry_001_every_common_and_method_unsupported_parameter() {
    let common = [
        "fields",
        "key",
        "oauth_token",
        "quotaUser",
        "userIp",
        "uploadType",
        "userProject",
        "callback",
    ];
    for (method, path) in [
        (Method::GET, "/storage/v1/b?project=p"),
        (Method::POST, "/storage/v1/b?project=p"),
        (Method::GET, "/storage/v1/b/abc?alt=json"),
        (Method::DELETE, "/storage/v1/b/abc?alt=json"),
    ] {
        for parameter in common {
            if parameter == "fields" && method == Method::DELETE {
                assert!(request(method.clone(), &format!("{path}&fields=")).is_ok());
                continue;
            }
            let error = request(method.clone(), &format!("{path}&{parameter}=")).unwrap_err();
            assert_eq!(
                error.status,
                StatusCode::BAD_REQUEST,
                "{method} {parameter}"
            );
            assert!(error.message.contains(parameter));
        }
    }
    for parameter in ["predefinedAcl", "predefinedDefaultObjectAcl"] {
        assert!(
            request(
                Method::POST,
                &format!("/storage/v1/b?project=p&{parameter}=private")
            )
            .is_ok()
        );
        assert_eq!(
            reject_predefined_acl(Some(&format!("{parameter}=private")), false, false)
                .unwrap_err()
                .reason,
            JsonError::unsupported(parameter).reason
        );
    }
    assert_eq!(
        request(
            Method::POST,
            "/storage/v1/b?project=p&enableObjectRetention=yes"
        )
        .unwrap_err()
        .reason,
        "invalidParameter"
    );
    for path in [
        "/storage/v1/b/abc?generation=1",
        "/storage/v1/b/abc?softDeleted=true",
        "/storage/v1/b?project=p&softDeleted=true",
        "/storage/v1/b?project=p&returnPartialSuccess=true",
    ] {
        assert_eq!(
            request(Method::GET, path).unwrap_err().status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[test]
fn bkt_qry_002_required_project_and_limits() {
    for method in [Method::GET, Method::POST] {
        for query in [
            "",
            "?project=",
            "?project=%20",
            "?project=%00",
            "?project=%20p",
            "?project=p%20",
            "?project=a+b",
        ] {
            assert_eq!(
                request(method.clone(), &format!("/storage/v1/b{query}"))
                    .unwrap_err()
                    .status,
                StatusCode::BAD_REQUEST
            );
        }
        assert!(
            request(
                method.clone(),
                &format!("/storage/v1/b?project={}", "p".repeat(256))
            )
            .is_ok()
        );
        assert!(
            request(
                method,
                &format!("/storage/v1/b?project={}", "p".repeat(257))
            )
            .is_err()
        );
    }
    assert!(
        request(
            Method::GET,
            &format!("/storage/v1/b?project=p&prefix={}", "a".repeat(222))
        )
        .is_ok()
    );
    assert!(
        request(
            Method::GET,
            &format!("/storage/v1/b?project=p&prefix={}", "a".repeat(223))
        )
        .is_err()
    );
    assert!(
        request(
            Method::GET,
            &format!("/storage/v1/b?project=p&{}", "&".repeat(16384))
        )
        .is_err()
    );
}

#[test]
fn bkt_qry_002_projection_alt_and_boolean_validation() {
    for (method, path) in [
        (Method::GET, "/storage/v1/b?project=p"),
        (Method::POST, "/storage/v1/b?project=p"),
        (Method::GET, "/storage/v1/b/abc?alt=json"),
    ] {
        assert!(request(method.clone(), &format!("{path}&projection=noAcl")).is_ok());
        assert!(request(method.clone(), &format!("{path}&projection=full")).is_ok());
        for value in ["", "FULL", "other"] {
            assert_eq!(
                request(method.clone(), &format!("{path}&projection={value}"))
                    .unwrap_err()
                    .status,
                StatusCode::BAD_REQUEST
            );
        }
    }
    assert!(
        request(
            Method::GET,
            "/storage/v1/b?project=p&alt=json&softDeleted=false&returnPartialSuccess=false&prettyPrint=false"
        )
        .is_ok()
    );
    assert!(request(Method::GET, "/storage/v1/b/abc?softDeleted=false").is_ok());
    for (query, reason) in [
        ("alt=xml", "invalidAltValue"),
        ("alt=", "invalidAltValue"),
        ("softDeleted=1", "invalidParameter"),
        ("returnPartialSuccess=TRUE", "invalidParameter"),
        ("prettyPrint=", "invalidParameter"),
    ] {
        assert_eq!(
            request(Method::GET, &format!("/storage/v1/b?project=p&{query}"))
                .unwrap_err()
                .reason,
            reason
        );
    }
    assert_eq!(
        request(Method::GET, "/storage/v1/b?project=p&alt=media")
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(Method::GET, "/storage/v1/b?project=p&prettyPrint=true")
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn bkt_get_002_and_del_002_signed_conditions() {
    for method in [Method::GET, Method::DELETE] {
        let parsed = request(method.clone(), "/storage/v1/b/abc?ifMetagenerationMatch=-9223372036854775808&ifMetagenerationNotMatch=9223372036854775807").unwrap();
        let conditions = match parsed {
            BucketRequest::Get { params, .. } => params.conditions,
            BucketRequest::Delete { conditions, .. } => conditions,
            _ => unreachable!(),
        };
        assert_eq!(conditions.if_metageneration_match, Some(i64::MIN));
        assert_eq!(conditions.if_metageneration_not_match, Some(i64::MAX));
        for key in ["ifMetagenerationMatch", "ifMetagenerationNotMatch"] {
            for value in [
                "",
                "1.0",
                "one",
                "9223372036854775808",
                "-9223372036854775809",
                "1%20",
                "%2B1",
                "+1",
            ] {
                assert_eq!(
                    request(method.clone(), &format!("/storage/v1/b/abc?{key}={value}"))
                        .unwrap_err()
                        .status,
                    StatusCode::BAD_REQUEST
                );
            }
        }
    }
}

#[test]
fn bkt_lst_001_max_results_defaults_bounds_and_clamping() {
    for (query, expected) in [
        ("", 1000),
        ("&maxResults=0", 1000),
        ("&maxResults=1", 1),
        ("&maxResults=1000", 1000),
        ("&maxResults=1001", 1000),
        ("&maxResults=4294967295", 1000),
    ] {
        let BucketRequest::List(params) =
            request(Method::GET, &format!("/storage/v1/b?project=p{query}")).unwrap()
        else {
            panic!("list");
        };
        assert_eq!(params.max_results, expected);
        assert_eq!(params.projection, Projection::NoAcl);
        assert!(params.prefix.is_empty());
        assert_eq!(params.start_after, None);
    }
    for (query, expected) in [
        ("", 200),
        ("&maxResults=0", 200),
        ("&maxResults=1", 1),
        ("&maxResults=200", 200),
        ("&maxResults=1000", 200),
        ("&maxResults=4294967295", 200),
    ] {
        let BucketRequest::List(params) = request(
            Method::GET,
            &format!("/storage/v1/b?project=p&projection=full{query}"),
        )
        .unwrap() else {
            panic!("list");
        };
        assert_eq!(params.max_results, expected);
        assert_eq!(params.projection, Projection::Full);
    }
    for value in [
        "",
        "-1",
        "%2B1",
        "1.0",
        "1e3",
        "4294967296",
        "18446744073709551616",
        "a",
    ] {
        assert_eq!(
            request(
                Method::GET,
                &format!("/storage/v1/b?project=p&maxResults={value}")
            )
            .unwrap_err()
            .status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[test]
fn bkt_ins_001_defaults_and_server_owned_values() {
    let now = UNIX_EPOCH + Duration::new(123, 456_789_123);
    let bucket = parse_insert(br#"{"name":"test-bucket"}"#, "p", now).unwrap();
    assert_eq!(bucket.project, "p");
    assert_eq!(bucket.location, "US");
    assert_eq!(bucket.location_type, "multi-region");
    assert_eq!(bucket.storage_class, "STANDARD");
    assert!(bucket.labels.is_empty());
    assert_eq!(bucket.metageneration, 1);
    assert_eq!(bucket.time_created, now);
    assert_eq!(bucket.time_created, bucket.updated);
    let bucket = parse_insert(
        br#"{"name":"abc","futureField":{"enabled":true}}"#,
        "p",
        now,
    )
    .unwrap();
    assert_eq!(bucket.name, "abc");
}

#[test]
fn bkt_ins_002_invalid_json_shape_scalar_types_and_required_fields() {
    for body in ["", "{", "{\"name\":\"abc\",}"] {
        assert_eq!(
            parse_insert(body.as_bytes(), "p", UNIX_EPOCH)
                .unwrap_err()
                .reason,
            "parseError"
        );
    }
    assert_eq!(
        parse_insert(b"{}", "p", UNIX_EPOCH).unwrap_err().reason,
        "required"
    );
    assert_eq!(
        request(Method::GET, "/storage/v1/b").unwrap_err().reason,
        "required"
    );
    for body in [
        "null",
        "[]",
        "true",
        r#"{"name":null}"#,
        r#"{"name":1}"#,
        r#"{"name":"Bad"}"#,
        r#"{"name":"abc","location":1}"#,
        r#"{"name":"abc","storageClass":2}"#,
        r#"{"name":"abc","labels":null}"#,
        r#"{"name":"abc","labels":[]}"#,
        r#"{"name":"abc","labels":{"key":false}}"#,
    ] {
        assert_eq!(
            parse_insert(body.as_bytes(), "p", UNIX_EPOCH)
                .unwrap_err()
                .reason,
            "invalidArgument",
            "{body}"
        );
    }
    assert_eq!(
        parse_insert(br#"{"name":"abc"}"#, "", UNIX_EPOCH)
            .unwrap_err()
            .reason,
        "invalidParameter"
    );
}

#[test]
fn bkt_ins_002_recognized_unsupported_fields_never_succeed() {
    for field in super::buckets::UNSUPPORTED_FIELDS {
        for value in [Value::Null, json!({}), json!([]), json!(false)] {
            let mut body = json!({"name":"abc"});
            body[field] = if *field == "autoclass" && (value.is_null() || value == json!({})) {
                json!({"enabled": true})
            } else if *field == "customPlacementConfig" && (value.is_null() || value == json!({})) {
                json!({"dataLocations":["US-EAST1", "US-WEST1"]})
            } else {
                value
            };
            let error = parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{field}");
            assert!(error.message.contains(field));
        }
    }
}

#[test]
fn bkt_ins_002_output_only_fields_do_not_override_server_state() {
    let expected = parse_insert(br#"{"name":"abc"}"#, "p", UNIX_EPOCH).unwrap();
    for field in [
        "kind",
        "id",
        "selfLink",
        "projectNumber",
        "metageneration",
        "timeCreated",
        "updated",
        "etag",
        "locationType",
        "owner",
        "generation",
        "softDeleteTime",
        "hardDeleteTime",
    ] {
        for value in [
            Value::Null,
            json!({}),
            json!([]),
            json!(false),
            json!("spoofed"),
        ] {
            let mut body = json!({"name":"abc"});
            body[field] = value;
            let actual = parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).unwrap();
            assert_eq!(actual, expected, "{field}");
        }
    }
    let mut body = resource(&expected);
    body["name"] = json!("new-bucket");
    let actual = parse_insert(body.to_string().as_bytes(), "q", UNIX_EPOCH).unwrap();
    assert_eq!(actual.name, "new-bucket");
    assert_eq!(actual.project, "q");
}

#[test]
fn bkt_ins_002_labels_unicode_boundaries_and_count() {
    let body = json!({"name":"abc", "labels":{"env":"", "日本語":"開発", "a_1-b":"c_2-d", "équipe":"été", "ключ":"значение"}});
    let bucket = parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).unwrap();
    assert_eq!(bucket.labels["env"], "");
    assert_eq!(resource(&bucket)["labels"], body["labels"]);
    for (key, value, valid) in [
        ("a".repeat(63), "日".repeat(63), true),
        ("a".repeat(64), "".to_owned(), false),
        ("a".to_owned(), "日".repeat(64), false),
        ("".to_owned(), "x".to_owned(), false),
        ("0key".to_owned(), "x".to_owned(), false),
        ("Key".to_owned(), "x".to_owned(), false),
        ("key".to_owned(), "Upper".to_owned(), false),
        ("key".to_owned(), "a b".to_owned(), false),
        ("_key".to_owned(), "x".to_owned(), false),
        ("key".to_owned(), "a\u{301}".to_owned(), false),
        ("😀".to_owned(), "x".to_owned(), false),
    ] {
        let body = json!({"name":"abc", "labels":{key:value}});
        assert_eq!(
            parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).is_ok(),
            valid,
            "{body}"
        );
    }
    for (count, valid) in [(64, true), (65, false)] {
        let labels: serde_json::Map<String, Value> = (0..count)
            .map(|index| (format!("key{index}"), json!("value")))
            .collect();
        let body = json!({"name":"abc", "labels":labels});
        assert_eq!(
            parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).is_ok(),
            valid
        );
    }
}

#[test]
fn bkt_ins_003_location_and_storage_class_subset() {
    for location in ["US", "EU", "ASIA"] {
        for class in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            let body = json!({"name":"abc", "location":location, "storageClass":class});
            let bucket = parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).unwrap();
            assert_eq!(bucket.location, location);
            assert_eq!(bucket.storage_class, class);
            assert_eq!(bucket.location_type, "multi-region");
        }
    }
    for (location, kind) in [
        ("us", "multi-region"),
        ("ASIA-NORTHEAST1", "region"),
        ("US-CENTRAL1", "region"),
        ("EUROPE-WEST1", "region"),
        ("NAM4", "dual-region"),
        ("ASIA1", "dual-region"),
        ("EUR4", "dual-region"),
        ("EUR5", "dual-region"),
        ("EUR7", "dual-region"),
        ("EUR8", "dual-region"),
    ] {
        let body = json!({"name":"abc", "location":location});
        let bucket = parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).unwrap();
        assert_eq!(bucket.location, location.to_ascii_uppercase());
        assert_eq!(bucket.location_type, kind);
    }
    for location in ["MARS-CENTRAL1", "unknown", " US", "US ", "US-CENTRAL1-A"] {
        let body = json!({"name":"abc", "location":location});
        let error = parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.reason, "invalidArgument");
        assert!(error.message.contains("US or ASIA-NORTHEAST1"));
    }
    for class in ["", "standard", "made-up"] {
        let body = json!({"name":"abc", "storageClass":class});
        assert_eq!(
            parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH)
                .unwrap_err()
                .reason,
            "invalidArgument"
        );
    }
    for class in ["REGIONAL", "RAPID"] {
        let body = json!({"name":"abc", "storageClass":class});
        assert_eq!(
            parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH)
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[test]
fn bucket_location_defaults_and_unsupported_placement() {
    for location in [Value::Null, json!("")] {
        let body = json!({"name":"abc", "location":location});
        let bucket = super::buckets::parse_insert_with_profile(
            body.to_string().as_bytes(),
            "p",
            UNIX_EPOCH,
            crate::config::CompatProfile::Native,
            "asia-northeast1",
        )
        .unwrap();
        assert_eq!(bucket.location, "ASIA-NORTHEAST1");
        assert_eq!(bucket.location_type, "region");
    }
    for placement in [Value::Null, json!({}), json!({"dataLocations": []})] {
        let body = json!({"name":"abc", "customPlacementConfig":placement});
        assert!(parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).is_ok());
    }
    for placement in [
        json!({"dataLocations": ["US-EAST1", "US-WEST1"]}),
        json!({"unknown":true}),
    ] {
        let body = json!({"name":"abc", "customPlacementConfig":placement});
        let error = parse_insert(body.to_string().as_bytes(), "p", UNIX_EPOCH).unwrap_err();
        assert_eq!(
            error.reason,
            JsonError::unsupported("customPlacementConfig").reason
        );
    }
}

#[test]
fn bkt_name_001_length_charset_and_dot_boundaries() {
    for name in [
        "abc",
        "0ab",
        "ab0",
        "a_b",
        "a-b",
        "test.example.com",
        "a.b",
        "a.1",
        "1.2.3.256",
    ] {
        assert!(validate_bucket_name(name).is_ok(), "{name}");
    }
    for name in [
        "",
        "a",
        "ab",
        "Abc",
        "abC",
        "a b",
        "-abc",
        "abc-",
        "_abc",
        "abc_",
        ".abc",
        "abc.",
        "a..b",
        "a/b",
        "日本語",
        "ab\0c",
    ] {
        assert!(validate_bucket_name(name).is_err(), "{name}");
    }
    assert!(validate_bucket_name(&"a".repeat(63)).is_ok());
    assert!(validate_bucket_name(&"a".repeat(64)).is_err());
    let longest = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(30)
    );
    assert_eq!(longest.len(), 222);
    assert!(validate_bucket_name(&longest).is_ok());
    assert!(validate_bucket_name(&format!("{longest}d")).is_err());
    assert!(validate_bucket_name(&format!("{}.a", "a".repeat(64))).is_err());
}

#[test]
fn bkt_name_002_ipv4_and_reserved_names() {
    for name in [
        "192.168.5.4",
        "0.0.0.0",
        "255.255.255.255",
        "001.002.003.004",
        "goog",
        "goog-bucket",
        "my-google-bucket",
        "my-g00gle-bucket",
        "a-go0gle-b",
        "a-g0ogle-b",
    ] {
        assert!(validate_bucket_name(name).is_err(), "{name}");
    }
    assert!(validate_bucket_name("g00g").is_ok());
}

#[test]
fn bkt_get_001_and_id_001_bucket_wire_fields() {
    let mut bucket = record();
    bucket.project = "000123".to_owned();
    bucket.metageneration = i64::MAX;
    let value = resource(&bucket);
    assert_eq!(value["kind"], "storage#bucket");
    assert_eq!(value["id"], value["name"]);
    assert_eq!(value["name"], "test-bucket");
    assert_eq!(
        value["selfLink"],
        "http://localhost:4443/storage/v1/b/test-bucket"
    );
    assert_eq!(value["projectNumber"], "123");
    assert_eq!(value["metageneration"], i64::MAX.to_string());
    assert_eq!(value["location"], "US");
    assert_eq!(value["locationType"], "multi-region");
    assert_eq!(value["storageClass"], "STANDARD");
    assert_eq!(value["timeCreated"], "1970-01-01T00:00:00.000Z");
    assert!(value["etag"].is_string());
    for field in [
        "acl",
        "owner",
        "defaultObjectAcl",
        "labels",
        "softDeletePolicy",
    ] {
        assert!(value.get(field).is_none(), "{field}");
    }
    bucket.project = "test-project".to_owned();
    assert_eq!(resource(&bucket)["projectNumber"], "0");
    bucket.project = u64::MAX.to_string();
    assert_eq!(resource(&bucket)["projectNumber"], u64::MAX.to_string());
    bucket.project = "18446744073709551616".to_owned();
    assert_eq!(resource(&bucket)["projectNumber"], "0");
    bucket.name = "a/b ?#é".to_owned();
    assert_eq!(
        resource(&bucket)["selfLink"],
        "http://localhost:4443/storage/v1/b/a%2Fb%20%3F%23%C3%A9"
    );
}

#[test]
fn bkt_id_001_utc_milliseconds_leap_century_and_range() {
    for (millis, expected) in [
        (-62_167_219_200_000i64, "0000-01-01T00:00:00.000Z"),
        (-2_208_988_800_000, "1900-01-01T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (0, "1970-01-01T00:00:00.000Z"),
        (951_782_400_123, "2000-02-29T00:00:00.123Z"),
        (4_107_542_400_000, "2100-03-01T00:00:00.000Z"),
        (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
    ] {
        let Some(time) = system_time_or_unrepresentable(millis) else {
            continue;
        };
        let bucket = parse_insert(br#"{"name":"abc"}"#, "p", time).unwrap();
        assert_eq!(resource(&bucket)["timeCreated"], expected, "{millis}");
        assert_eq!(resource(&bucket)["updated"], expected);
    }
    let bucket = parse_insert(
        br#"{"name":"abc"}"#,
        "p",
        // 100 ns is the finest pre-epoch step every platform's SystemTime keeps.
        UNIX_EPOCH - Duration::from_nanos(100),
    )
    .unwrap();
    assert_eq!(resource(&bucket)["timeCreated"], "1969-12-31T23:59:59.999Z");
    for millis in [-62_167_219_200_001i64, 253_402_300_800_000] {
        let Some(time) = system_time_or_unrepresentable(millis) else {
            continue;
        };
        assert_eq!(
            parse_insert(br#"{"name":"abc"}"#, "p", time)
                .unwrap_err()
                .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let mut bucket = record();
        bucket.updated = time;
        assert!(bucket_resource(&bucket, "http://localhost", Projection::NoAcl).is_err());
    }
}

/// Windows SystemTime starts in 1601, so earlier instants cannot even be
/// constructed there; every other platform must represent the whole range.
fn system_time_or_unrepresentable(millis: i64) -> Option<SystemTime> {
    let duration = Duration::from_millis(millis.unsigned_abs());
    let time = if millis < 0 {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    };
    assert!(time.is_some() || cfg!(windows), "{millis}");
    time
}

#[test]
fn bkt_id_002_etag_changes_with_revision_and_incarnation() {
    let original = record();
    assert_eq!(resource(&original)["etag"], resource(&original)["etag"]);
    let mut changed = original.clone();
    changed.metageneration += 1;
    assert_ne!(resource(&changed)["etag"], resource(&original)["etag"]);
    changed = original.clone();
    changed.time_created += Duration::from_millis(1);
    assert_ne!(resource(&changed)["etag"], resource(&original)["etag"]);
    changed = original.clone();
    changed.updated += Duration::from_millis(1);
    assert_ne!(resource(&changed)["etag"], resource(&original)["etag"]);
}

#[test]
fn bkt_lst_001_response_omits_empty_items_and_absent_token() {
    assert_eq!(
        bucket_list(&[], None, "http://localhost", Projection::NoAcl).unwrap(),
        json!({"kind":"storage#buckets"})
    );
    let value = bucket_list(
        &[record()],
        Some("opaque"),
        "http://localhost:4443",
        Projection::NoAcl,
    )
    .unwrap();
    assert_eq!(value["nextPageToken"], "opaque");
    assert_eq!(value["items"], json!([resource(&record())]));
    assert!(value.get("unreachable").is_none());
}

#[test]
fn bkt_lst_002_tokens_round_trip_scope_and_query_cursor() {
    let token = encode_page_token("project-1", "test-", "test-bucket").unwrap();
    assert_eq!(
        decode_page_token(&token, "project-1", "test-").unwrap(),
        "test-bucket"
    );
    assert!(decode_page_token(&token, "project-2", "test-").is_err());
    assert!(decode_page_token(&token, "project-1", "").is_err());
    let BucketRequest::List(params) = request(
        Method::GET,
        &format!("/storage/v1/b?project=project-1&prefix=test-&pageToken={token}&maxResults=2"),
    )
    .unwrap() else {
        panic!("list");
    };
    assert_eq!(params.start_after.as_deref(), Some("test-bucket"));
    assert_eq!(params.max_results, 2);
    assert!(encode_page_token("project-1", "other-", "test-bucket").is_err());
    assert!(encode_page_token("", "", "abc").is_err());
    assert!(encode_page_token("p", "", "Bad").is_err());
    let token = encode_page_token("プロジェクト", "", "abc").unwrap();
    assert_eq!(
        decode_page_token(&token, "プロジェクト", "").unwrap(),
        "abc"
    );
}

#[test]
fn bkt_lst_002_token_tampering_and_invalid_shapes() {
    let token = encode_page_token("p", "", "abc").unwrap();
    for index in 0..token.len() {
        let mut altered = token.as_bytes().to_vec();
        altered[index] = if altered[index] == b'0' { b'1' } else { b'0' };
        let altered = String::from_utf8(altered).unwrap();
        assert!(
            decode_page_token(&altered, "p", "").is_err(),
            "byte {index}"
        );
    }
    for invalid in [
        "".to_owned(),
        "abc".to_owned(),
        "ab.0".to_owned(),
        "zz.0000000000000000".to_owned(),
        "é.0000000000000000".to_owned(),
        "a".repeat(4097),
    ] {
        assert_eq!(
            decode_page_token(&invalid, "p", "").unwrap_err().reason,
            "invalidParameter"
        );
    }
    assert!(matches!(
        request(Method::GET, "/storage/v1/b?project=p&pageToken="),
        Ok(BucketRequest::List(ListParams {
            start_after: None,
            ..
        }))
    ));
}

#[tokio::test]
async fn bkt_err_001_envelope_content_type_and_catalog_mappings() {
    for (catalog, status, reason) in [
        (
            CatalogError::AlreadyExists,
            StatusCode::CONFLICT,
            "conflict",
        ),
        (CatalogError::NotFound, StatusCode::NOT_FOUND, "notFound"),
        (CatalogError::NotEmpty, StatusCode::CONFLICT, "conflict"),
        (
            CatalogError::PreconditionFailed,
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
        ),
        (
            CatalogError::Unavailable("secret database path".to_owned()),
            StatusCode::SERVICE_UNAVAILABLE,
            "backendError",
        ),
    ] {
        let error = JsonError::from(catalog);
        assert_eq!(error.status, status);
        assert_eq!(error.reason, reason);
        let response = error.into_response();
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json; charset=UTF-8"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error"]["code"], status.as_u16());
        assert_eq!(value["error"]["errors"][0]["reason"], reason);
        assert_eq!(value["error"]["errors"][0]["domain"], "global");
        assert_eq!(
            value["error"]["errors"][0]["message"],
            value["error"]["message"]
        );
        assert_eq!(value["error"]["errors"].as_array().unwrap().len(), 1);
        assert!(
            !String::from_utf8(bytes.to_vec())
                .unwrap()
                .contains("secret")
        );
    }
    let error = JsonError::invalid_argument("a \"quoted\" message\nline");
    assert_eq!(
        serde_json::from_str::<Value>(&error.value().to_string()).unwrap()["error"]["message"],
        error.message
    );
}

struct Frames {
    frames: VecDeque<Result<Frame<Bytes>, &'static str>>,
}

impl Body for Frames {
    type Data = Bytes;
    type Error = &'static str;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Poll::Ready(self.frames.pop_front())
    }
}

#[tokio::test]
async fn bkt_body_001_empty_exact_cap_multiframe_and_trailers() {
    assert!(
        read_body(Empty::<Bytes>::new(), 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        read_body(Full::new(Bytes::from_static(b"1234")), 4)
            .await
            .unwrap(),
        b"1234"
    );
    let frames = Frames {
        frames: [
            Ok(Frame::data(Bytes::from_static(b"12"))),
            Ok(Frame::trailers(hyper::HeaderMap::new())),
            Ok(Frame::data(Bytes::from_static(b"34"))),
        ]
        .into(),
    };
    assert_eq!(read_body(frames, 4).await.unwrap(), b"1234");
    let chunks = Bytes::from_static(b"12").chain(Bytes::from_static(b"34"));
    assert_eq!(read_body(Full::new(chunks), 4).await.unwrap(), b"1234");
}

#[tokio::test]
async fn bkt_body_001_rejects_oversize_and_read_errors() {
    for cap in [0, 3] {
        let error = read_body(Full::new(Bytes::from_static(b"1234")), cap)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(error.reason, "uploadTooLarge");
    }
    let frames = Frames {
        frames: [
            Ok(Frame::data(Bytes::from_static(b"12"))),
            Ok(Frame::data(Bytes::from_static(b"345"))),
            Err("must not poll after overflow"),
        ]
        .into(),
    };
    assert_eq!(
        read_body(frames, 4).await.unwrap_err().status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let frames = Frames {
        frames: [
            Ok(Frame::data(Bytes::from_static(b"12"))),
            Err("transport failed"),
        ]
        .into(),
    };
    assert_eq!(read_body(frames, 4).await.unwrap_err().reason, "badRequest");
}
