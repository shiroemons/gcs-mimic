use super::JsonError;
use crate::object::{ExpectedHashes, ObjectMetadataUpdate, ObjectRecord};

/// Rewrite preserves composite identity in the same location/class and collapses its component
/// count to one when either changes; adapters retain the source snapshot for this decision.
pub fn derived_component_count(
    source: &ObjectRecord,
    destination_class: &str,
    same_location: bool,
) -> Option<i64> {
    source.component_count.map(|count| {
        if same_location && destination_class == source.storage_class {
            count
        } else {
            1
        }
    })
}

/// Empty bodies inherit editable source metadata except holds. A supplied resource replaces
/// that metadata, including `{}`; path bucket/name always win over resource values.
pub fn derived_metadata(
    bytes: &[u8],
    source: &ObjectRecord,
) -> Result<(ObjectMetadataUpdate, Option<String>, ExpectedHashes), JsonError> {
    if bytes.is_empty()
        || std::str::from_utf8(bytes)
            .is_ok_and(|value| value.trim_matches([' ', '\t', '\r', '\n']) == "null")
    {
        return Ok((
            ObjectMetadataUpdate {
                content_type: Some(Some(source.content_type.clone())),
                content_encoding: Some(source.content_encoding.clone()),
                content_disposition: Some(source.content_disposition.clone()),
                content_language: Some(source.content_language.clone()),
                cache_control: Some(source.cache_control.clone()),
                metadata: Some(Some(
                    source
                        .metadata
                        .iter()
                        .map(|(key, value)| (key.clone(), Some(value.clone())))
                        .collect(),
                )),
                ..Default::default()
            },
            Some(source.storage_class.clone()),
            ExpectedHashes::default(),
        ));
    }
    let mut parsed = super::parse_object_metadata(bytes, true)?;
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
            crate::bucket::validate_storage_class(class).map_err(JsonError::from)?;
            Ok::<_, JsonError>(class.to_owned())
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
    fn empty_body_inherits_and_explicit_resource_replaces_metadata_without_source_holds() {
        assert!(derived_metadata(b"\x0bnull", &crate::json::object_tests::record()).is_err());
        let mut source = crate::json::object_tests::record();
        source.metadata.insert("source".into(), "value".into());
        source.event_based_hold = true;
        let (inherit, class, _) = derived_metadata(b"", &source).unwrap();
        assert_eq!(
            derived_metadata(b" \nnull\t", &source).unwrap(),
            derived_metadata(b"", &source).unwrap()
        );
        source.component_count = Some(3);
        assert_eq!(
            derived_component_count(&source, &source.storage_class, true),
            Some(3)
        );
        assert_eq!(derived_component_count(&source, "NEARLINE", true), Some(1));
        assert_eq!(
            derived_component_count(&source, &source.storage_class, false),
            Some(1)
        );
        assert_eq!(class, Some(source.storage_class.clone()));
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
        assert_eq!(derived_metadata(b"{}", &source).unwrap().0.metadata, None);
    }
}
