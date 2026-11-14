use super::*;

#[test]
fn populated_request_keeps_its_json_bytes() {
    let options = resumable::golden_tests::populated_options();
    let start = RewriteStart {
        binding: RewriteBinding {
            source_bucket: "source-bucket".into(),
            source_object: "source".into(),
            source_generation: 7,
            destination_bucket: "destination-bucket".into(),
            destination_object: "destination".into(),
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
            expected: options.expected,
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
        budget: 1048576,
    };
    let encoded = encode(
        &start,
        "00000000000000000000000000000001".parse().unwrap(),
        "00000000000000000000000000000002".parse().unwrap(),
    )
    .to_string();
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
    assert_eq!(session.destination.expected, start.destination.expected);
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
    for class in ["unknown", "REGIONAL", "RAPID"] {
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
