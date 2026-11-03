use super::*;

pub(in crate::storage::catalog) fn populated_options() -> ResumableOptions {
    let identity = crate::auth::Identity::from_upload_snapshot(&json!({"id":"uploader","projects":{"project":["storage.objects.create","storage.objects.delete"]}})).unwrap();
    ResumableOptions {
        content_type: "text/plain".into(),
        metadata: ObjectMetadataUpdate {
            acl: None,
            predefined_acl: Some(crate::acl::PredefinedObjectAcl::parse("publicRead").unwrap()),
            replace: true,
            content_type: Some(Some("text/custom".into())),
            content_encoding: Some(Some("gzip".into())),
            content_disposition: Some(Some("inline".into())),
            content_language: Some(Some("en".into())),
            cache_control: Some(Some("private".into())),
            metadata: Some(Some(std::collections::BTreeMap::from([
                ("alpha".into(), Some("one".into())),
                ("removed".into(), None),
            ]))),
            event_based_hold: Some(false),
            temporary_hold: Some(true),
            custom_time: Some(Some("2026-01-02T03:04:05Z".into())),
            retention: Some(Some(crate::object::retention::ObjectRetention {
                mode: crate::object::retention::RetentionMode::Unlocked,
                retain_until: crate::object::unix_nanos::UnixNanos::new(4_000_000_000, 123_456_789)
                    .unwrap(),
            })),
            override_unlocked_retention: crate::object::retention::UnlockedOverride::NotRequested,
            customer_key_sha256: Some([2; 32]),
            storage_class: Some("NEARLINE".into()),
        },
        expected: ExpectedHashes {
            md5: Some([1; 16]),
            crc32c: Some(123),
        },
        conditions: ObjectConditions {
            if_generation_match: Some(7),
            if_generation_not_match: Some(8),
            if_metageneration_match: Some(9),
            if_metageneration_not_match: Some(10),
        },
        access: BucketAccess::Policy {
            identity: Some(identity),
            permission: crate::auth::Permission::ObjectsCreate,
            project: Some("project".into()),
        },
        origin: Some("https://example.test".into()),
        projection_full: true,
        response_fields: Some("id,name".into()),
    }
}

#[test]
fn populated_options_keep_their_json_bytes() {
    let options = populated_options();
    let encoded = encode_options(&options).to_string();
    assert_eq!(
        encoded,
        r#"{"version":2,"content_type":"text/plain","metadata":{"replace":true,"eventBasedHold":false,"temporaryHold":true,"customTime":{"value":"2026-01-02T03:04:05Z"},"contentType":{"value":"text/custom"},"contentEncoding":{"value":"gzip"},"contentDisposition":{"value":"inline"},"contentLanguage":{"value":"en"},"cacheControl":{"value":"private"},"metadata":{"value":{"alpha":"one","removed":null}},"retention":{"value":{"mode":"Unlocked","retainUntilMicros":4000000000123456,"retainUntilSubMicroNanos":789}},"customerKeySha256":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"storageClass":"NEARLINE","predefinedAcl":"publicRead"},"md5":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"crc32c":123,"conditions":[7,8,9,10],"access":{"identity":{"id":"uploader","projects":{"project":["storage.objects.create","storage.objects.delete"]}},"permission":"storage.objects.create","project":"project"},"origin":"https://example.test","full":true,"response_fields":"id,name"}"#
    );
    assert_eq!(
        decode_options(&serde_json::from_str(&encoded).unwrap()).unwrap(),
        options
    );
}
