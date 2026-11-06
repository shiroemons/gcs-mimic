use super::JsonError;
use crate::object::{ExpectedHashes, ObjectMetadataUpdate, ObjectRecord};

/// Rewrite preserves composite identity in the same location/class and collapses its component
/// count to one when either changes; adapters retain the source snapshot for this decision.
/// Whether a copy's destination bucket shares the source bucket's location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DestinationLocation {
    Same,
    Different,
}

impl DestinationLocation {
    pub(crate) fn of(source: Option<&str>, destination: &str) -> Self {
        if source == Some(destination) {
            Self::Same
        } else {
            Self::Different
        }
    }
}

pub(crate) fn derived_component_count(
    source: &ObjectRecord,
    destination_class: crate::bucket::StorageClass,
    location: DestinationLocation,
) -> Option<i64> {
    source.component_count.map(|count| {
        if location == DestinationLocation::Same && destination_class == source.storage_class {
            count
        } else {
            1
        }
    })
}

/// Empty bodies inherit editable source metadata except holds. A supplied resource replaces
/// that metadata, including `{}`; path bucket/name always win over resource values.
pub(crate) fn derived_metadata(
    bytes: &[u8],
    source: &ObjectRecord,
) -> Result<
    (
        ObjectMetadataUpdate,
        Option<crate::bucket::StorageClass>,
        ExpectedHashes,
    ),
    JsonError,
> {
    // An absent, `null` or empty-object resource keeps the source metadata (recorded for `{}`).
    if bytes.is_empty()
        || std::str::from_utf8(bytes)
            .is_ok_and(|value| matches!(value.trim_matches([' ', '\t', '\r', '\n']), "null" | "{}"))
    {
        let update = ObjectMetadataUpdate::inheriting(source);
        // Publishing checks this as well, but only after the whole body has been copied.
        update.check_encoded_size().map_err(JsonError::from)?;
        return Ok((
            update,
            Some(source.storage_class),
            ExpectedHashes::default(),
        ));
    }
    let mut parsed = super::parse_object_metadata(bytes, super::WriteMode::Replace)?;
    parsed.update.validate().map_err(JsonError::from)?;
    if parsed.update.temporary_hold == Some(true) {
        return Err(JsonError::unsupported_object(
            "temporaryHold on copy/rewrite",
        ));
    }
    if parsed
        .update
        .custom_time
        .as_ref()
        .is_some_and(Option::is_some)
    {
        return Err(JsonError::unsupported_object("customTime on copy/rewrite"));
    }
    if parsed.update.retention.is_some() {
        return Err(JsonError::unsupported_object("retention on copy/rewrite"));
    }
    for key in ["name", "bucket"] {
        if let Some(value) = parsed.unchanged.remove(key)
            && !value.is_string()
        {
            return Err(JsonError::invalid_argument(format!(
                "{key} must be a string"
            )));
        }
    }
    if let Some(kind) = parsed.unchanged.remove("kind")
        && kind != "storage#object"
    {
        return Err(JsonError::unsupported_object("kind"));
    }
    let class = parsed
        .unchanged
        .remove("storageClass")
        .map(|value| {
            let class = value
                .as_str()
                .ok_or_else(|| JsonError::invalid_argument("storageClass must be a string"))?;
            crate::bucket::validate_storage_class(class).map_err(JsonError::from)
        })
        .transpose()?;
    let mut expected = ExpectedHashes::default();
    for key in ["md5Hash", "crc32c"] {
        if let Some(value) = parsed.unchanged.remove(key) {
            let value = value
                .as_str()
                .ok_or_else(|| JsonError::invalid_argument("Checksum must be a base64 string"))?;
            if key == "md5Hash" {
                expected.md5 = Some(super::objects::decode_hash(value)?);
            } else {
                expected.crc32c = Some(u32::from_be_bytes(super::objects::decode_hash(value)?));
            }
        }
    }
    if let Some((key, _)) = parsed.unchanged.iter().next() {
        return Err(JsonError::unsupported_object(key));
    }
    Ok((parsed.update, class, expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_class_patch_keeps_error_contracts() {
        for (class, reason, message) in [
            (
                "REGIONAL",
                "badRequest",
                "gcs-mimic does not support bucket API setting storageClass requires a region or zone; omit this setting (for example, create a bucket with only name).",
            ),
            ("standard", "invalid", "Invalid storage class \"standard\""),
            ("UNKNOWN", "invalid", "Invalid storage class \"UNKNOWN\""),
        ] {
            let body = serde_json::json!({"storageClass":class}).to_string();
            let error = super::super::buckets::parse_bucket_patch(
                body.as_bytes(),
                crate::config::CompatProfile::Native,
            )
            .unwrap_err();
            assert_eq!(error.status.as_u16(), 400, "{class}");
            assert_eq!(error.reason, reason, "{class}");
            assert_eq!(error.message, message, "{class}");
        }
    }

    #[test]
    fn storage_class_callers_keep_error_contracts() {
        let source = crate::json::object_tests::record();
        let mut snapshots = Vec::new();
        for class in ["REGIONAL", "standard", "UNKNOWN"] {
            let insert = serde_json::json!({"name":"bucket","storageClass":class}).to_string();
            let lifecycle = serde_json::json!({"name":"bucket","lifecycle":{"rule":[{"action":{"type":"SetStorageClass","storageClass":class},"condition":{"age":1}}]}}).to_string();
            let rewrite = serde_json::json!({"storageClass":class}).to_string();
            for (caller, error) in [
                (
                    "insert",
                    super::super::parse_insert(insert.as_bytes(), "project", std::time::UNIX_EPOCH)
                        .unwrap_err(),
                ),
                (
                    "lifecycle",
                    super::super::parse_insert(
                        lifecycle.as_bytes(),
                        "project",
                        std::time::UNIX_EPOCH,
                    )
                    .unwrap_err(),
                ),
                (
                    "rewrite",
                    derived_metadata(rewrite.as_bytes(), &source).unwrap_err(),
                ),
                (
                    "object",
                    JsonError::from(
                        crate::object::validate_object_storage_class(class).unwrap_err(),
                    ),
                ),
            ] {
                snapshots.push(serde_json::json!([
                    caller,
                    class,
                    error.status.as_u16(),
                    error.reason,
                    error.message
                ]));
            }
        }
        assert_eq!(
            serde_json::json!(snapshots),
            serde_json::json!([
                [
                    "insert",
                    "REGIONAL",
                    400,
                    "badRequest",
                    "gcs-mimic does not support bucket API setting storageClass requires a region or zone; omit this setting (for example, create a bucket with only name)."
                ],
                [
                    "lifecycle",
                    "REGIONAL",
                    400,
                    "badRequest",
                    "gcs-mimic does not support bucket API setting lifecycle.rule[0].action.storageClass=REGIONAL: storageClass requires a region or zone; omit this setting (for example, create a bucket with only name)."
                ],
                [
                    "rewrite",
                    "REGIONAL",
                    400,
                    "badRequest",
                    "gcs-mimic does not support bucket API setting storageClass requires a region or zone; omit this setting (for example, create a bucket with only name)."
                ],
                [
                    "object",
                    "REGIONAL",
                    400,
                    "badRequest",
                    "gcs-mimic does not support object API setting storageClass=REGIONAL: storageClass requires a region or zone; omit this option or header (for example, use projection=noAcl)."
                ],
                [
                    "insert",
                    "standard",
                    400,
                    "invalid",
                    "The storage class you specified is not valid."
                ],
                [
                    "lifecycle",
                    "standard",
                    400,
                    "invalid",
                    "Invalid lifecycle rule[0].action.storageClass; use a supported bucket class, for example COLDLINE"
                ],
                [
                    "rewrite",
                    "standard",
                    400,
                    "invalid",
                    "The storage class you specified is not valid."
                ],
                [
                    "object",
                    "standard",
                    400,
                    "invalid",
                    "Invalid object storageClass; use STANDARD, NEARLINE, COLDLINE or ARCHIVE"
                ],
                [
                    "insert",
                    "UNKNOWN",
                    400,
                    "invalid",
                    "The storage class you specified is not valid."
                ],
                [
                    "lifecycle",
                    "UNKNOWN",
                    400,
                    "invalid",
                    "Invalid lifecycle rule[0].action.storageClass; use a supported bucket class, for example COLDLINE"
                ],
                [
                    "rewrite",
                    "UNKNOWN",
                    400,
                    "invalid",
                    "The storage class you specified is not valid."
                ],
                [
                    "object",
                    "UNKNOWN",
                    400,
                    "invalid",
                    "Invalid object storageClass; use STANDARD, NEARLINE, COLDLINE or ARCHIVE"
                ]
            ])
        );
    }

    #[test]
    fn metadata_too_large_to_inherit_fails_before_a_copy_starts() {
        let mut source = crate::json::object_tests::record();
        source.content_disposition = Some("x".repeat(41 * 1024));
        assert!(derived_metadata(b"", &source).is_err());
        assert!(derived_metadata(b"{\"contentType\":\"text/plain\"}", &source).is_ok());
    }

    #[test]
    fn metadata_edits_cannot_add_up_past_what_a_copy_may_inherit() {
        let mut record = crate::json::object_tests::record();
        let edit = |update: ObjectMetadataUpdate, record: &mut ObjectRecord| {
            update.apply_to_existing(record)
        };
        edit(
            ObjectMetadataUpdate {
                content_disposition: Some(Some("x".repeat(30 * 1024))),
                ..Default::default()
            },
            &mut record,
        )
        .unwrap();
        let second = edit(
            ObjectMetadataUpdate {
                cache_control: Some(Some("x".repeat(30 * 1024))),
                ..Default::default()
            },
            &mut record,
        );
        assert!(matches!(
            second,
            Err(crate::object::ObjectError::InvalidFixedMetadata(_))
        ));
    }

    #[test]
    fn empty_body_inherits_and_explicit_resource_replaces_metadata_without_source_holds() {
        assert!(derived_metadata(b"\x0bnull", &crate::json::object_tests::record()).is_err());
        let mut source = crate::json::object_tests::record();
        source.metadata.insert("source".into(), "value".into());
        source.event_based_hold = crate::object::Hold::Held;
        let (inherit, class, _) = derived_metadata(b"", &source).unwrap();
        assert_eq!(
            derived_metadata(b" \nnull\t", &source).unwrap(),
            derived_metadata(b"", &source).unwrap()
        );
        source.component_count = Some(3);
        assert_eq!(
            derived_component_count(&source, source.storage_class, DestinationLocation::Same),
            Some(3)
        );
        assert_eq!(
            derived_component_count(
                &source,
                crate::bucket::StorageClass::Nearline,
                DestinationLocation::Same
            ),
            Some(1)
        );
        assert_eq!(
            derived_component_count(
                &source,
                source.storage_class,
                DestinationLocation::Different
            ),
            Some(1)
        );
        assert_eq!(class, Some(source.storage_class));
        assert_eq!(inherit.event_based_hold, None);
        let (replace, class, _) = derived_metadata(
            br#"{"metadata":{"destination":"value"},"name":"overridden"}"#,
            &source,
        )
        .unwrap();
        assert_eq!(class, None);
        let mut destination = source.clone();
        replace.apply(&mut destination).unwrap();
        assert_eq!(destination.metadata.len(), 1);
        assert!(destination.metadata.contains_key("destination"));
        assert!(destination.content_type.is_empty());
        // An empty resource keeps the source metadata, like an absent one (recorded for `{}`).
        assert_eq!(
            derived_metadata(b"{}", &source).unwrap(),
            derived_metadata(b"", &source).unwrap()
        );
    }
}
