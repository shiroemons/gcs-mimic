use super::ApiError;
use crate::bucket::StorageClass;
use crate::object::{ExpectedHashes, ObjectMetadataUpdate, ObjectRecord};
use serde_json::{Map, Value};

/// The resource a copy or rewrite body supplies, or `None` when the destination inherits the
/// source metadata. Cloud Storage treats a blank or `null` body, `{}`, and a resource whose only
/// field is a `null` or empty `metadata` map as no resource at all (recorded 2026-11-13 for a
/// single-space body, `{"metadata":null}` and `{"metadata":{}}` on copy and rewrite).
pub(crate) fn derived_resource(bytes: &[u8]) -> Result<Option<Value>, ApiError> {
    if bytes
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
    {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|cause| crate::json::parse_error(bytes, &cause))?;
    let inherits = match &value {
        Value::Null => true,
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            key == "metadata" && (value.is_null() || value.as_object().is_some_and(Map::is_empty))
        }),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => false,
    };
    Ok((!inherits).then_some(value))
}

/// The metadata a copy or rewrite gives its destination.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DerivedMetadata {
    pub(crate) update: ObjectMetadataUpdate,
    /// The class the resource names; the source's class when the destination inherits.
    pub(crate) storage_class: Option<StorageClass>,
    pub(crate) expected: ExpectedHashes,
}

/// Empty bodies inherit editable source metadata except holds. A supplied resource replaces
/// that metadata; path bucket/name always win over resource values.
pub(crate) fn derived_metadata(
    bytes: &[u8],
    source: &ObjectRecord,
) -> Result<DerivedMetadata, ApiError> {
    derived_metadata_of(derived_resource(bytes)?.as_ref(), source)
}

/// [`derived_metadata`] for a body already reduced by [`derived_resource`].
pub(crate) fn derived_metadata_of(
    resource: Option<&Value>,
    source: &ObjectRecord,
) -> Result<DerivedMetadata, ApiError> {
    let Some(resource) = resource else {
        let update = ObjectMetadataUpdate::inheriting(source);
        // Publishing checks this as well, but only after the whole body has been copied.
        update.check_encoded_size().map_err(ApiError::from)?;
        return Ok(DerivedMetadata {
            update,
            storage_class: Some(source.storage_class),
            expected: ExpectedHashes::default(),
        });
    };
    let mut parsed = super::object_metadata_from_map(
        super::resource_object(resource)?,
        super::WriteMode::Replace,
    )?;
    // Cloud Storage answers a resource naming a generation but no name this way, before it
    // looks at any other field (recorded 2026-11-13 for a copy body with output-only fields).
    if parsed
        .unchanged
        .get("generation")
        .is_some_and(|value| !value.is_null())
        && !parsed.unchanged.contains_key("name")
    {
        return Err(ApiError::required("You must specify an object name."));
    }
    // Holds, a custom time and a retention apply as on an upload: Cloud Storage copied and
    // rewrote an object held and with the requested custom time, and refused a retention in a
    // bucket without object retention as publication does (recorded 2026-11-13, case
    // obj-unsupported-derived).
    parsed.update.validate().map_err(ApiError::from)?;
    for key in ["name", "bucket"] {
        if let Some(value) = parsed.unchanged.remove(key)
            && !value.is_string()
        {
            return Err(ApiError::invalid_argument(format!(
                "{key} must be a string"
            )));
        }
    }
    if let Some(kind) = parsed.unchanged.remove("kind")
        && kind != "storage#object"
    {
        return Err(ApiError::unsupported_object("kind"));
    }
    let storage_class = parsed
        .unchanged
        .remove("storageClass")
        .map(|value| {
            let class = value
                .as_str()
                .ok_or_else(|| ApiError::invalid_argument("storageClass must be a string"))?;
            crate::bucket::validate_storage_class(class).map_err(ApiError::from)
        })
        .transpose()?;
    // Cloud Storage ignores a supplied size (recorded 2026-11-13: a rewrite body with a wrong
    // size completes with the source's size).
    parsed.unchanged.remove("size");
    // Starts empty, so nothing can conflict.
    let mut expected = ExpectedHashes::default();
    super::checksum::take_resource_hashes(
        &mut parsed.unchanged,
        || ApiError::invalid_argument(super::checksum::NOT_BASE64),
        |declared| {
            expected.set(declared);
            Ok(())
        },
    )?;
    if let Some((key, _)) = parsed.unchanged.iter().next() {
        return Err(ApiError::unsupported_object(key));
    }
    Ok(DerivedMetadata {
        update: parsed.update,
        storage_class,
        expected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Patch;
    use std::assert_matches;

    #[test]
    fn bucket_class_patch_keeps_error_contracts() {
        for (class, reason, message) in [
            (
                "RAPID",
                "badRequest",
                "gcs-mimic does not support bucket API setting storageClass RAPID requires a zonal bucket; omit this setting (for example, create a bucket with only name).",
            ),
            ("standard", "invalid", "Invalid storage class \"standard\""),
            ("UNKNOWN", "invalid", "Invalid storage class \"UNKNOWN\""),
        ] {
            let body = serde_json::json!({"storageClass":class}).to_string();
            let error = super::super::buckets::parse_bucket_patch(
                body.as_bytes(),
                crate::profile::CompatProfile::Native,
            )
            .unwrap_err();
            assert_eq!(error.status().as_u16(), 400, "{class}");
            assert_eq!(error.reason(), reason, "{class}");
            assert_eq!(error.message(), message, "{class}");
        }
    }

    #[test]
    fn storage_class_callers_keep_error_contracts() {
        let source = crate::json::object_tests::record();
        let mut snapshots = Vec::new();
        for class in ["RAPID", "standard", "UNKNOWN"] {
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
                    ApiError::from(
                        crate::object::validate_object_storage_class(class).unwrap_err(),
                    ),
                ),
            ] {
                snapshots.push(serde_json::json!([
                    caller,
                    class,
                    error.status().as_u16(),
                    error.reason(),
                    error.message()
                ]));
            }
        }
        assert_eq!(
            serde_json::json!(snapshots),
            serde_json::json!([
                [
                    "insert",
                    "RAPID",
                    400,
                    "badRequest",
                    "gcs-mimic does not support bucket API setting storageClass RAPID requires a zonal bucket; omit this setting (for example, create a bucket with only name)."
                ],
                [
                    "lifecycle",
                    "RAPID",
                    400,
                    "badRequest",
                    "gcs-mimic does not support bucket API setting lifecycle.rule[0].action.storageClass=RAPID: storageClass requires a zonal bucket; omit this setting (for example, create a bucket with only name)."
                ],
                [
                    "rewrite",
                    "RAPID",
                    400,
                    "badRequest",
                    "gcs-mimic does not support bucket API setting storageClass RAPID requires a zonal bucket; omit this setting (for example, create a bucket with only name)."
                ],
                [
                    "object",
                    "RAPID",
                    400,
                    "badRequest",
                    "gcs-mimic does not support object API setting storageClass=RAPID: storageClass requires a zonal bucket; omit this option or header (for example, use projection=noAcl)."
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
            update.apply_to_existing(record, std::time::SystemTime::UNIX_EPOCH)
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
        assert_matches!(
            second,
            Err(crate::object::ObjectError::InvalidFixedMetadata(_))
        );
    }

    #[test]
    fn empty_body_inherits_and_explicit_resource_replaces_metadata_without_source_holds() {
        assert!(derived_metadata(b"\x0bnull", &crate::json::object_tests::record()).is_err());
        let mut source = crate::json::object_tests::record();
        source.metadata.insert("source".into(), "value".into());
        source.event_based_hold = crate::object::Hold::Held;
        let DerivedMetadata {
            update: inherit,
            storage_class: class,
            ..
        } = derived_metadata(b"", &source).unwrap();
        assert_eq!(
            derived_metadata(b" \nnull\t", &source).unwrap(),
            derived_metadata(b"", &source).unwrap()
        );
        assert_eq!(class, Some(source.storage_class));
        assert_eq!(inherit.event_based_hold, None);
        let DerivedMetadata {
            update: replace,
            storage_class: class,
            ..
        } = derived_metadata(
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
        let replaced = derived_metadata(br#"{"metadata":{},"contentType":""}"#, &source)
            .unwrap()
            .update;
        let mut destination = source.clone();
        replaced.apply(&mut destination).unwrap();
        assert!(destination.metadata.is_empty());
        assert_eq!(destination.content_language, None);
        // A non-JSON blank byte is still malformed.
        assert_eq!(
            derived_metadata(b"\x0c", &source).unwrap_err().reason(),
            "invalid"
        );
    }

    /// A rewrite body that the server already holds as a value reads the same as its text
    /// would, which the rewrite route once serialized again and parsed.
    #[test]
    fn a_held_resource_reads_like_its_serialized_text() {
        let source = crate::json::object_tests::record();
        for text in [
            r#"{"contentType":"a/b","metadata":{"a":0.1,"b":-0.0,"c":1E7,"d":1.0E-4,"e":18446744073709551615,"g":"é\u0001\"","h":[1],"i":{"x":1},"j":null}}"#,
            r#"{"md5Hash":"AAAAAAAAAAAAAAAAAAAAAA==","crc32c":"AAAAAA==","storageClass":"NEARLINE"}"#,
            r#"{"name":"n","bucket":"b","kind":"storage#object","size":"9"}"#,
            r#"{"generation":"1"}"#,
            r#"{"storageClass":"standard"}"#,
            r#"{"metadata":{"":"empty key"}}"#,
            r#"[1]"#,
            r#""text""#,
        ] {
            let value: Value = serde_json::from_str(text).unwrap();
            assert_eq!(
                derived_metadata_of(Some(&value), &source),
                derived_metadata(value.to_string().as_bytes(), &source),
                "{text}"
            );
        }
    }

    #[test]
    fn output_only_size_is_ignored_and_a_generation_needs_a_name() {
        let source = crate::json::object_tests::record();
        let sized = derived_metadata(br#"{"size":"999","contentType":"a/b"}"#, &source)
            .unwrap()
            .update;
        let plain = derived_metadata(br#"{"contentType":"a/b"}"#, &source)
            .unwrap()
            .update;
        assert_eq!(sized, plain);
        let error = derived_metadata(
            br#"{"size":"999","generation":"1","md5Hash":"AAAAAAAAAAAAAAAAAAAAAA==","timeCreated":"2000-01-01T00:00:00Z"}"#,
            &source,
        )
        .unwrap_err();
        assert_eq!(
            (error.status().as_u16(), error.reason(), error.message()),
            (400, "required", "You must specify an object name.")
        );
        // Other output-only fields keep their unsupported-setting answer.
        assert_eq!(
            derived_metadata(br#"{"timeCreated":"2000-01-01T00:00:00Z"}"#, &source)
                .unwrap_err()
                .reason(),
            "badRequest"
        );
    }
}
