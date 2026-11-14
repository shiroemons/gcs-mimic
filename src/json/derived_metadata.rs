use super::JsonError;
use crate::object::{ExpectedHashes, ObjectMetadataUpdate, ObjectRecord};
use serde_json::{Map, Value};

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

/// The resource a copy or rewrite body supplies, or `None` when the destination inherits the
/// source metadata. Cloud Storage treats a blank or `null` body, `{}`, and a resource whose only
/// field is a `null` or empty `metadata` map as no resource at all (recorded 2026-11-13 for a
/// single-space body, `{"metadata":null}` and `{"metadata":{}}` on copy and rewrite).
pub(crate) fn derived_resource(bytes: &[u8]) -> Result<Option<Value>, JsonError> {
    if bytes
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
    {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|cause| JsonError::parse_error(bytes, &cause))?;
    let inherits = match &value {
        Value::Null => true,
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            key == "metadata" && (value.is_null() || value.as_object().is_some_and(Map::is_empty))
        }),
        _ => false,
    };
    Ok((!inherits).then_some(value))
}

/// Empty bodies inherit editable source metadata except holds. A supplied resource replaces
/// that metadata; path bucket/name always win over resource values.
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
    let Some(resource) = derived_resource(bytes)? else {
        let update = ObjectMetadataUpdate::inheriting(source);
        // Publishing checks this as well, but only after the whole body has been copied.
        update.check_encoded_size().map_err(JsonError::from)?;
        return Ok((
            update,
            Some(source.storage_class),
            ExpectedHashes::default(),
        ));
    };
    let mut parsed =
        super::parse_object_metadata(resource.to_string().as_bytes(), super::WriteMode::Replace)?;
    // Cloud Storage answers a resource naming a generation but no name this way, before it
    // looks at any other field (recorded 2026-11-13 for a copy body with output-only fields).
    if parsed
        .unchanged
        .get("generation")
        .is_some_and(|value| !value.is_null())
        && !parsed.unchanged.contains_key("name")
    {
        return Err(JsonError::required("You must specify an object name."));
    }
    parsed.update.validate().map_err(JsonError::from)?;
    if parsed.update.temporary_hold == Some(true) {
        return Err(JsonError::unsupported_object(
            "temporaryHold on copy/rewrite",
        ));
    }
    if parsed.update.custom_time.value().is_some() {
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
    // Cloud Storage ignores a supplied size (recorded 2026-11-13: a rewrite body with a wrong
    // size completes with the source's size).
    parsed.unchanged.remove("size");
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
    use crate::object::Patch;

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
                content_disposition: Patch::Set("x".repeat(30 * 1024)),
                ..Default::default()
            },
            &mut record,
        )
        .unwrap();
        let second = edit(
            ObjectMetadataUpdate {
                cache_control: Patch::Set("x".repeat(30 * 1024)),
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

    #[test]
    fn blank_and_empty_metadata_resources_inherit_like_an_absent_body() {
        let mut source = crate::json::object_tests::record();
        source.metadata.insert("k".into(), "v".into());
        source.content_language = Some("en".into());
        let inherited = derived_metadata(b"", &source).unwrap();
        for body in [
            &b" "[..],
            b"\r\n\t ",
            br#"{"metadata":null}"#,
            br#" {"metadata":{}} "#,
        ] {
            assert_eq!(
                derived_metadata(body, &source).unwrap(),
                inherited,
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        // Any other field still replaces the editable metadata.
        let (replaced, ..) =
            derived_metadata(br#"{"metadata":{},"contentType":""}"#, &source).unwrap();
        let mut destination = source.clone();
        replaced.apply(&mut destination).unwrap();
        assert!(destination.metadata.is_empty());
        assert_eq!(destination.content_language, None);
        // A non-JSON blank byte is still malformed.
        assert_eq!(
            derived_metadata(b"\x0c", &source).unwrap_err().reason,
            "invalid"
        );
    }

    #[test]
    fn output_only_size_is_ignored_and_a_generation_needs_a_name() {
        let source = crate::json::object_tests::record();
        let (sized, ..) =
            derived_metadata(br#"{"size":"999","contentType":"a/b"}"#, &source).unwrap();
        let (plain, ..) = derived_metadata(br#"{"contentType":"a/b"}"#, &source).unwrap();
        assert_eq!(sized, plain);
        let error = derived_metadata(
            br#"{"size":"999","generation":"1","md5Hash":"AAAAAAAAAAAAAAAAAAAAAA==","timeCreated":"2000-01-01T00:00:00Z"}"#,
            &source,
        )
        .unwrap_err();
        assert_eq!(
            (error.status.as_u16(), error.reason, error.message.as_str()),
            (400, "required", "You must specify an object name.")
        );
        // Other output-only fields keep their unsupported-setting answer.
        assert_eq!(
            derived_metadata(br#"{"timeCreated":"2000-01-01T00:00:00Z"}"#, &source)
                .unwrap_err()
                .reason,
            "badRequest"
        );
    }
}
