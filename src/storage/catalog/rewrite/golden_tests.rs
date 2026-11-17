use super::*;
use serde_json::json;

const TOKEN: &str = "00000000000000000000000000000000000000000000000000000000000000ab";

/// Stored responses are replayed after restarts, so both shapes keep their bytes, including the
/// gRPC adapter's `{"proto":[..]}` resource.
#[test]
fn stored_responses_keep_their_json_bytes() {
    let progress = StoredResponse::Progress {
        offset: 1048576,
        token: TOKEN.parse().unwrap(),
    };
    assert_eq!(
        serde_json::to_string(&progress).unwrap(),
        format!(r#"{{"offset":1048576,"token":"{TOKEN}"}}"#)
    );
    for (resource, text) in [
        (
            json!({"kind":"storage#object","name":"destination","size":"8"}),
            r#"{"offset":8,"resource":{"kind":"storage#object","name":"destination","size":"8"}}"#,
        ),
        (
            json!({"proto":[10,1,98]}),
            r#"{"offset":8,"resource":{"proto":[10,1,98]}}"#,
        ),
    ] {
        let complete = StoredResponse::Complete {
            offset: 8,
            resource,
        };
        let stored = serde_json::to_string(&complete).unwrap();
        assert_eq!(stored, text);
        let value: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(StoredResponse::try_from(value).unwrap(), complete);
    }
    let value: Value = serde_json::from_str(&serde_json::to_string(&progress).unwrap()).unwrap();
    assert_eq!(StoredResponse::try_from(value).unwrap(), progress);
}

/// The decoding rules sessions were always checked by: a `resource` object completes, a string
/// `token` without one continues, and anything else is an invalid session.
#[test]
fn stored_response_shapes_outside_the_two_written_ones_are_invalid() {
    let progress = || StoredResponse::Progress {
        offset: 1,
        token: TOKEN.parse().unwrap(),
    };
    for (value, expected) in [
        (json!({"offset":1,"token":TOKEN}), Some(progress())),
        (
            json!({"offset":1,"token":TOKEN,"resource":null}),
            Some(progress()),
        ),
        (
            json!({"offset":1,"resource":{}}),
            Some(StoredResponse::Complete {
                offset: 1,
                resource: json!({}),
            }),
        ),
        (json!({"offset":1,"token":null}), None),
        (json!({"offset":1,"resource":null}), None),
        (json!({"offset":1,"resource":{},"token":TOKEN}), None),
        (json!({"offset":1,"resource":{},"token":null}), None),
        (json!({"offset":1}), None),
        (json!({"token":TOKEN}), None),
        (json!({"offset":-1,"token":TOKEN}), None),
        (json!({"offset":1,"token":TOKEN.to_uppercase()}), None),
        (json!({"offset":1,"token":7}), None),
        (json!([1]), None),
        (json!(null), None),
    ] {
        assert_eq!(
            StoredResponse::try_from(value.clone()).ok(),
            expected,
            "{value}"
        );
    }
}

#[test]
fn populated_request_keeps_its_json_bytes() {
    let options = resumable::golden_tests::populated_options();
    let start = RewriteStart {
        binding: RewriteBinding {
            source: crate::object::ObjectTarget::new("source-bucket", "source", 7),
            destination: crate::object::ObjectTarget::new("destination-bucket", "destination", ()),
            parameters: json!({"fields":"name","ifSourceGenerationMatch":"7"})
                .as_object()
                .unwrap()
                .clone(),
            metadata: Some(json!({"contentType":"text/plain","metadata":{"key":"value"}})),
        },
        destination: DerivedDestination {
            bucket: "destination-bucket".into(),
            name: "destination".into(),
            content_type: options.content_type,
            metadata: options.metadata,
            declared: Default::default(),
            recorded: options.expected,
            conditions: options.conditions,
            access: BucketAccess::Permissive,
            storage_class: Some(crate::bucket::StorageClass::Coldline),
            component_count: Some(2),
        },
        source_conditions: ObjectConditions {
            if_generation_match: Some(7),
            if_generation_not_match: Some(8),
            if_metageneration_match: Some(9),
            if_metageneration_not_match: Some(10),
        },
        source_metageneration: 3,
        source_access: BucketAccess::Permissive,
        source_key_sha256: Some([3; 32]),
        budget: std::num::NonZeroU64::new(1_048_576).unwrap(),
    };
    let encoded = encode(
        &start,
        "00000000000000000000000000000001".parse().unwrap(),
        "00000000000000000000000000000002".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(
        encoded,
        r#"{"path":["source-bucket","source","destination-bucket","destination"],"generation":7,"metageneration":3,"parameters":{"fields":"name","ifSourceGenerationMatch":"7"},"body":{"contentType":"text/plain","metadata":{"key":"value"}},"source_incarnation":"00000000000000000000000000000001","destination_incarnation":"00000000000000000000000000000002","options":{"version":2,"content_type":"text/plain","metadata":{"replace":true,"eventBasedHold":false,"temporaryHold":true,"customTime":{"value":"2026-01-02T03:04:05Z"},"contentType":{"value":"text/custom"},"contentEncoding":{"value":"gzip"},"contentDisposition":{"value":"inline"},"contentLanguage":{"value":"en"},"cacheControl":{"value":"private"},"metadata":{"value":{"alpha":"one","removed":null}},"retention":{"value":{"mode":"Unlocked","retainUntilMicros":4000000000123456,"retainUntilSubMicroNanos":789}},"customerKeySha256":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"storageClass":"NEARLINE","predefinedAcl":"publicRead"},"md5":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"crc32c":123,"conditions":[7,8,9,10],"access":null,"origin":null,"full":false,"response_fields":null},"class":"COLDLINE","components":2,"source_conditions":[7,8,9,10],"budget":1048576,"source_key_sha256":[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3]}"#
    );
    let connection = Connection::open_in_memory().unwrap();
    let session = connection
        .query_row(
            "SELECT ?1,?2,NULL,?3,0,8,604801000,NULL,?1",
            params!["00000000000000000000000000000003", "0".repeat(64), &encoded],
            read,
        )
        .unwrap();
    assert_eq!(session.binding, start.binding);
    assert_eq!(session.destination.metadata, start.destination.metadata);
    assert_eq!(
        session.destination.storage_class,
        start.destination.storage_class
    );
    assert_eq!(
        session.destination.component_count,
        start.destination.component_count
    );
    assert_eq!(session.source_key_sha256, start.source_key_sha256);
    assert_eq!(session.source_conditions, start.source_conditions);
    assert_eq!(session.destination.recorded, start.destination.recorded);
    assert_eq!(session.destination.conditions, start.destination.conditions);
    assert_eq!(
        session.destination.content_type,
        start.destination.content_type
    );
    assert_eq!(session.source_metageneration, start.source_metageneration);
    assert_eq!(
        session.source_incarnation.to_string(),
        "00000000000000000000000000000001"
    );
    assert_eq!(
        session.destination_incarnation.to_string(),
        "00000000000000000000000000000002"
    );
    assert_eq!(session.budget, start.budget);
    // SQLite only stores lowercase incarnations, so another spelling is a corrupt row.
    for field in ["source_incarnation", "destination_incarnation"] {
        let mut invalid: Value = serde_json::from_str(&encoded).unwrap();
        invalid[field] = json!("0000000000000000000000000000000A");
        let error = connection
            .query_row(
                "SELECT ?1,?2,NULL,?3,0,8,604801000,NULL,?1",
                params![
                    "00000000000000000000000000000003",
                    "0".repeat(64),
                    invalid.to_string()
                ],
                read,
            )
            .err()
            .unwrap();
        assert!(
            matches!(error, rusqlite::Error::FromSqlConversionFailure(3, _, _)),
            "{field}: {error:?}"
        );
        assert!(
            matches!(
                crate::storage::failure::unavailable(error),
                CatalogError::Corrupt(_)
            ),
            "{field}"
        );
    }
    for class in ["unknown", "RAPID"] {
        let mut invalid: Value = serde_json::from_str(&encoded).unwrap();
        invalid["class"] = json!(class);
        let error = connection
            .query_row(
                "SELECT ?1,?2,NULL,?3,0,8,604801000,NULL,?1",
                params![
                    "00000000000000000000000000000003",
                    "0".repeat(64),
                    invalid.to_string()
                ],
                read,
            )
            .err()
            .unwrap();
        assert!(
            matches!(
                crate::storage::failure::unavailable(error),
                CatalogError::Corrupt(_)
            ),
            "{class}"
        );
    }
    let mut invalid: Value = serde_json::from_str(&encoded).unwrap();
    invalid["budget"] = 0.into();
    let error = connection
        .query_row(
            "SELECT ?1,?2,NULL,?3,0,8,604801000,NULL,?1",
            params![
                "00000000000000000000000000000003",
                "0".repeat(64),
                invalid.to_string()
            ],
            read,
        )
        .err()
        .unwrap();
    assert!(matches!(
        error,
        rusqlite::Error::FromSqlConversionFailure(3, _, _)
    ));
    assert_eq!(
        std::error::Error::source(&error).unwrap().to_string(),
        "invalid rewrite budget"
    );
}
