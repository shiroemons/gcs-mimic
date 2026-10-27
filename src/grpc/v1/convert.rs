use super::{pb, unsupported};
use crate::{
    bucket::BucketRecord,
    json,
    object::{ObjectError, ObjectRecord},
};
use serde_json::json;
use tonic::Status;

pub(super) fn bucket(record: &BucketRecord) -> pb::Bucket {
    pb::Bucket {
        name: record.name.clone(),
        id: record.name.clone(),
        metageneration: record.metageneration,
        location: record.location.clone(),
        location_type: record.location_type.clone(),
        storage_class: record.storage_class.clone(),
        labels: record.labels.clone().into_iter().collect(),
        default_event_based_hold: record.default_event_based_hold,
        versioning: Some(pb::bucket::Versioning {
            enabled: record.versioning_enabled,
        }),
        time_created: Some(record.time_created.into()),
        updated: Some(record.updated.into()),
        ..Default::default()
    }
}

pub(super) fn bucket_metadata(
    bucket: &mut pb::Bucket,
    current: Option<&pb::Bucket>,
) -> Result<Vec<u8>, Status> {
    let insert = current.is_none();
    if let Some(current) = current {
        let defaults = pb::Bucket::default();
        macro_rules! unchanged { ($($field:ident),* $(,)?) => { $(
            if bucket.$field != defaults.$field && bucket.$field != current.$field {
                return Err(unsupported(concat!("changing legacy bucket read-only field ", stringify!($field), "; keep its fetched value")));
            }
            bucket.$field = Default::default();
        )* }; }
        unchanged!(
            id,
            project_number,
            metageneration,
            time_created,
            updated,
            location,
            location_type
        );
    }
    let mut body = json!({"defaultEventBasedHold":bucket.default_event_based_hold});
    if insert {
        body["name"] = json!(bucket.name);
    }
    if !bucket.storage_class.is_empty() {
        body["storageClass"] = json!(bucket.storage_class);
    }
    if !insert || !bucket.labels.is_empty() {
        body["labels"] = json!(bucket.labels);
    }
    if !bucket.location.is_empty() && insert {
        body["location"] = json!(bucket.location);
        bucket.location.clear();
    }
    if let Some(versioning) = &bucket.versioning {
        body["versioning"] = json!({"enabled":versioning.enabled});
    }
    bucket.name.clear();
    bucket.storage_class.clear();
    bucket.labels.clear();
    bucket.versioning = None;
    bucket.default_event_based_hold = false;
    if *bucket != Default::default() {
        return Err(unsupported(
            "legacy bucket metadata outside name, location, class, labels, versioning and default hold",
        ));
    }
    serde_json::to_vec(&body).map_err(|_| Status::internal("Cannot encode bucket metadata"))
}

pub(super) fn object_outputs(
    value: &mut pb::Object,
    current: Option<&pb::Object>,
) -> Result<(), ObjectError> {
    let defaults = pb::Object::default();
    macro_rules! unchanged { ($($field:ident),* $(,)?) => { $(
        if let Some(current) = current
            && value.$field != defaults.$field && value.$field != current.$field {
            return Err(ObjectError::UnsupportedMetadata(format!("read-only {} must retain its fetched value", stringify!($field))));
        }
        value.$field = Default::default();
    )* }; }
    unchanged!(
        metageneration,
        time_deleted,
        size,
        time_created,
        crc32c,
        component_count,
        md5_hash,
        etag,
        updated,
        storage_class,
        time_storage_class_updated,
        retention_expiration_time,
        name,
        id,
        bucket,
        generation,
        owner
    );
    Ok(())
}

pub(super) fn object(record: &ObjectRecord) -> Result<pb::Object, Status> {
    // Reuse JSON's checksum formatting rather than maintaining a second base64 encoder.
    let resource =
        json::object_resource(record, "", json::Projection::NoAcl).map_err(super::from_json)?;
    Ok(pb::Object {
        name: record.name.clone(),
        bucket: record.bucket.clone(),
        generation: record.generation,
        metageneration: record.metageneration,
        size: record.size as i64,
        storage_class: record.storage_class.clone(),
        content_type: record.content_type.clone(),
        content_encoding: record.content_encoding.clone().unwrap_or_default(),
        content_disposition: record.content_disposition.clone().unwrap_or_default(),
        content_language: record.content_language.clone().unwrap_or_default(),
        cache_control: record.cache_control.clone().unwrap_or_default(),
        metadata: record.metadata.clone().into_iter().collect(),
        crc32c: Some(record.crc32c),
        md5_hash: resource["md5Hash"].as_str().unwrap_or_default().to_owned(),
        component_count: record.component_count.unwrap_or_default() as i32,
        time_created: Some(record.time_created.into()),
        updated: Some(record.updated.into()),
        time_deleted: record.time_deleted.map(Into::into),
        event_based_hold: Some(record.event_based_hold),
        temporary_hold: record.temporary_hold,
        retention_expiration_time: record.retention_expiration_time.map(Into::into),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_v1_output_fields_accept_unchanged_and_reject_changes() {
        let current = pb::Object {
            generation: 7,
            size: 3,
            storage_class: "STANDARD".into(),
            ..Default::default()
        };
        let mut same = current.clone();
        object_outputs(&mut same, Some(&current)).unwrap();
        assert_eq!(same, pb::Object::default());
        let mut changed = current.clone();
        changed.generation = 8;
        assert!(matches!(
            object_outputs(&mut changed, Some(&current)),
            Err(ObjectError::UnsupportedMetadata(_))
        ));
        let current = pb::Bucket {
            name: "example-bucket".into(),
            id: "example-bucket".into(),
            metageneration: 7,
            location: "US".into(),
            ..Default::default()
        };
        let mut same = current.clone();
        assert!(bucket_metadata(&mut same, Some(&current)).is_ok());
        let mut changed = current.clone();
        changed.id = "different-bucket".into();
        assert_eq!(
            bucket_metadata(&mut changed, Some(&current))
                .unwrap_err()
                .code(),
            tonic::Code::Unimplemented
        );
    }
}
