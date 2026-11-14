use super::*;
use crate::object::Patch;

#[test]
fn unknown_stored_storage_class_is_catalog_corruption() {
    let connection = Connection::open_in_memory().unwrap();
    for class in ["unknown", "REGIONAL", "RAPID"] {
        let mut options = encode_options(&populated_options());
        options["metadata"]["storageClass"] = json!(class);
        let error = connection.query_row("SELECT ?1,?2,'bucket','object','00000000000000000000000000000001',?3,NULL,0,0,604800000,'ACTIVE',NULL", params!["0".repeat(64), "0".repeat(32), options.to_string()], read_session).unwrap_err();
        assert!(
            matches!(
                crate::storage::failure::unavailable(error),
                CatalogError::Corrupt(_)
            ),
            "{class}"
        );
    }
}

#[test]
fn hold_update_states_keep_their_stored_json_presence() {
    for (hold, expected) in [
        (None, json!(null)),
        (Some(false), json!(false)),
        (Some(true), json!(true)),
    ] {
        let mut options = populated_options();
        options.metadata.event_based_hold = hold;
        options.metadata.temporary_hold = hold;
        let encoded = encode_options(&options);
        assert_eq!(
            encoded["metadata"].get("eventBasedHold"),
            Some(&expected),
            "{hold:?}"
        );
        assert_eq!(
            encoded["metadata"].get("temporaryHold"),
            Some(&expected),
            "{hold:?}"
        );
        assert_eq!(decode_options(&encoded).unwrap(), options, "{hold:?}");
        if hold.is_none() {
            let mut absent = encoded;
            absent["metadata"]
                .as_object_mut()
                .unwrap()
                .remove("eventBasedHold");
            absent["metadata"]
                .as_object_mut()
                .unwrap()
                .remove("temporaryHold");
            assert_eq!(decode_options(&absent).unwrap(), options, "omitted holds");
        }
    }
}

pub(in crate::storage::catalog) fn populated_options() -> ResumableOptions {
    let identity = crate::auth::Identity::from_upload_snapshot(&json!({"id":"uploader","projects":{"project":["storage.objects.create","storage.objects.delete"]}})).unwrap();
    ResumableOptions {
        content_type: "text/plain".into(),
        metadata: ObjectMetadataUpdate {
            metadata_mode: crate::object::MapMode::Merge,
            acl: None,
            predefined_acl: Some(crate::acl::PredefinedObjectAcl::parse("publicRead").unwrap()),
            write: crate::object::WriteMode::Replace,
            content_type: Patch::Set("text/custom".into()),
            content_encoding: Patch::Set("gzip".into()),
            content_disposition: Patch::Set("inline".into()),
            content_language: Patch::Set("en".into()),
            cache_control: Patch::Set("private".into()),
            metadata: Some(Some(std::collections::BTreeMap::from([
                ("alpha".into(), Some("one".into())),
                ("removed".into(), None),
            ]))),
            event_based_hold: Some(false),
            temporary_hold: Some(true),
            custom_time: Patch::Set("2026-01-02T03:04:05Z".into()),
            retention: Some(Some(crate::object::retention::ObjectRetention {
                mode: crate::object::retention::RetentionMode::Unlocked,
                retain_until: crate::object::unix_nanos::UnixNanos::new(4_000_000_000, 123_456_789)
                    .unwrap(),
            })),
            override_unlocked_retention: crate::object::retention::UnlockedOverride::NotRequested,
            customer_key_sha256: Some([2; 32]),
            storage_class: Some(crate::bucket::StorageClass::Nearline),
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

#[test]
fn invalid_predefined_acl_keeps_its_corruption_reason() {
    for predefined in [json!(5), json!("bogus")] {
        let mut encoded = encode_options(&populated_options());
        encoded["metadata"]["predefinedAcl"] = predefined;
        assert_eq!(
            decode_options(&encoded).unwrap_err().to_string(),
            "invalid resumable predefinedAcl"
        );
    }
}

#[test]
fn invalid_session_identifiers_report_their_own_columns() {
    let connection = Connection::open_in_memory().unwrap();
    let options = encode_options(&populated_options()).to_string();
    for (id, blob, column) in [
        ("invalid".into(), "0".repeat(32), 0),
        ("0".repeat(64), "invalid".into(), 1),
        ("0".repeat(64), "A".repeat(32), 1),
    ] {
        let error = connection.query_row("SELECT ?1,?2,'bucket','object','00000000000000000000000000000001',?3,NULL,0,0,604800000,'ACTIVE',NULL", params![id,blob,options], read_session).unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, _) if index == column)
        );
        assert!(matches!(
            crate::storage::failure::unavailable(error),
            CatalogError::Corrupt(_)
        ));
    }
}
