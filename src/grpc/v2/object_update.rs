use super::{bucket_update::acl_entries, pb, status::unsupported};
use crate::{
    acl::AclKind,
    object::{
        MapMode, ObjectError, ObjectMetadataUpdate, ObjectRecord,
        retention::{ObjectRetention, RetentionMode},
        unix_nanos::UnixNanos,
    },
};
use std::collections::BTreeMap;
use tonic::Status;

pub(super) fn from_proto(
    object: &pb::Object,
    paths: &[String],
) -> Result<ObjectMetadataUpdate, Status> {
    if paths.iter().any(|path| path == "metadata")
        && paths.iter().any(|path| path.starts_with("metadata."))
    {
        return Err(Status::invalid_argument(
            "Use either metadata or metadata.KEY in update_mask",
        ));
    }
    let mut update = ObjectMetadataUpdate::default();
    for path in paths {
        match path.as_str() {
            "metadata" => {
                update.metadata = Some(Some(
                    object
                        .metadata
                        .iter()
                        .map(|(key, value)| (key.clone(), Some(value.clone())))
                        .collect(),
                ));
                update.metadata_mode = MapMode::Replace;
            }
            path if let Some(key) = path.strip_prefix("metadata.")
                && !key.is_empty() =>
            {
                update
                    .metadata
                    .get_or_insert_with(|| Some(BTreeMap::new()))
                    .get_or_insert_with(BTreeMap::new)
                    .insert(key.into(), object.metadata.get(key).cloned());
            }
            "content_type" => update.content_type = Some(Some(object.content_type.clone())),
            "content_encoding" => {
                update.content_encoding = Some(Some(object.content_encoding.clone()))
            }
            "content_disposition" => {
                update.content_disposition = Some(Some(object.content_disposition.clone()))
            }
            "content_language" => {
                update.content_language = Some(
                    (!object.content_language.is_empty()).then(|| object.content_language.clone()),
                )
            }
            "cache_control" => update.cache_control = Some(Some(object.cache_control.clone())),
            "custom_time" => {
                update.custom_time = Some(object.custom_time.map(|time| time.to_string()))
            }
            "temporary_hold" => update.temporary_hold = Some(object.temporary_hold),
            "event_based_hold" => {
                update.event_based_hold = Some(object.event_based_hold.unwrap_or(false))
            }
            "acl" => {
                update.acl = Some(acl_entries(
                    AclKind::Object,
                    object.acl.iter().map(|entry| (&entry.entity, &entry.role)),
                )?)
            }
            "retention" => update.retention = Some(retention(object.retention.as_ref())?),
            // The pinned Go SDK includes this output-only path in an ordinary empty update.
            "create_time" => {}
            _ => {
                return Err(unsupported(&format!(
                    "UpdateObject update_mask {path}; use metadata or content_type"
                )));
            }
        }
    }
    validate(&update)?;
    Ok(update)
}

pub(super) fn validate(update: &ObjectMetadataUpdate) -> Result<(), Status> {
    update.validate().map_err(input_error)
}

pub(super) fn input_error(error: ObjectError) -> Status {
    if let ObjectError::Metadata(error) = error {
        let message = error.message(super::bucket_update::proto_field);
        return if error.unsupported() {
            super::status::unsupported(&format!("unsupported metadata change: {message}"))
        } else {
            Status::invalid_argument(message)
        };
    }
    super::object_io::object_status(error)
}

pub(super) fn retention(
    value: Option<&pb::object::Retention>,
) -> Result<Option<ObjectRetention>, Status> {
    let Some(value) = value else { return Ok(None) };
    let mode = match pb::object::retention::Mode::try_from(value.mode) {
        Ok(pb::object::retention::Mode::Unlocked) => RetentionMode::Unlocked,
        Ok(pb::object::retention::Mode::Locked) => RetentionMode::Locked,
        _ => {
            return Err(Status::invalid_argument(
                "retention.mode must be UNLOCKED or LOCKED",
            ));
        }
    };
    let time = value.retain_until_time.ok_or_else(|| {
        Status::invalid_argument("retention.retain_until_time is required with retention.mode")
    })?;
    let invalid = || {
        Status::invalid_argument("retention.retain_until_time must be a valid protobuf timestamp")
    };
    if !(-62_135_596_800..=253_402_300_799).contains(&time.seconds) {
        return Err(invalid());
    }
    let nanos = u32::try_from(time.nanos).map_err(|_| invalid())?;
    let retain_until = UnixNanos::new(time.seconds, nanos).ok_or_else(invalid)?;
    Ok(Some(ObjectRetention { mode, retain_until }))
}

pub(super) fn for_write(object: &pb::Object) -> ObjectMetadataUpdate {
    ObjectMetadataUpdate {
        replace: true,
        content_type: (!object.content_type.is_empty()).then(|| Some(object.content_type.clone())),
        content_encoding: (!object.content_encoding.is_empty())
            .then(|| Some(object.content_encoding.clone())),
        content_disposition: (!object.content_disposition.is_empty())
            .then(|| Some(object.content_disposition.clone())),
        content_language: (!object.content_language.is_empty())
            .then(|| Some(object.content_language.clone())),
        cache_control: (!object.cache_control.is_empty())
            .then(|| Some(object.cache_control.clone())),
        metadata: Some(Some(
            object
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect(),
        )),
        temporary_hold: object.temporary_hold.then_some(true),
        event_based_hold: object.event_based_hold,
        custom_time: object.custom_time.map(|time| Some(time.to_string())),
        ..Default::default()
    }
}

pub(super) fn inherited(source: &ObjectRecord) -> ObjectMetadataUpdate {
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
        storage_class: Some(source.storage_class),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn overlapping_metadata_masks_are_actionable_in_both_orders() {
        for paths in [
            ["metadata", "metadata.missing"],
            ["metadata.missing", "metadata"],
        ] {
            let error = super::from_proto(&super::pb::Object::default(), &paths.map(String::from))
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert_eq!(
                error.message(),
                "Use either metadata or metadata.KEY in update_mask"
            );
        }
    }
    use super::*;
    use prost::Message;

    #[test]
    fn object_updates_preserve_map_modes_and_explicit_resets() {
        let object = pb::Object {
            metadata: [("keep".into(), "value".into())].into(),
            ..Default::default()
        };
        let update = from_proto(
            &object,
            &[
                "metadata".into(),
                "content_language".into(),
                "retention".into(),
            ],
        )
        .unwrap();
        assert_eq!(update.metadata_mode, MapMode::Replace);
        assert_eq!(update.metadata.unwrap().unwrap().len(), 1);
        assert_eq!(update.content_language, Some(None));
        assert_eq!(update.retention, Some(None));
        let update = from_proto(&object, &["metadata.missing".into()]).unwrap();
        assert_eq!(update.metadata_mode, MapMode::Merge);
        assert_eq!(update.metadata.unwrap().unwrap()["missing"], None);
    }

    #[test]
    fn object_validation_uses_proto_field_names_and_keeps_error_details() {
        let object = pb::Object {
            content_type: "invalid\r\nvalue".into(),
            ..Default::default()
        };
        let error = from_proto(&object, &["content_type".into()]).unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("content_type"));
        assert!(!error.message().contains("contentType"));
        let object = pb::Object {
            metadata: [(String::new(), "value".into())].into(),
            ..Default::default()
        };
        let error = from_proto(&object, &["metadata".into()]).unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unimplemented);
        let details = crate::grpc::generated::google::rpc::Status::decode(error.details()).unwrap();
        assert_eq!(
            details.details[0].type_url,
            "type.googleapis.com/google.rpc.ErrorInfo"
        );
    }

    #[test]
    fn retention_validates_timestamp_bounds_and_keeps_nanoseconds() {
        let value = |seconds, nanos| pb::object::Retention {
            mode: pb::object::retention::Mode::Unlocked as i32,
            retain_until_time: Some(prost_types::Timestamp { seconds, nanos }),
        };
        for (seconds, nanos) in [
            (0, -1),
            (0, 1_000_000_000),
            (-62_135_596_801, 0),
            (253_402_300_800, 0),
        ] {
            let error = retention(Some(&value(seconds, nanos))).unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(error.message().contains("retain_until_time"));
        }
        let result = retention(Some(&value(2_000_000_000, 123_456_789)))
            .unwrap()
            .unwrap();
        assert_eq!(result.retain_until.subsec_nanos(), 123_456_789);
    }
}
