//! The persisted session document keeps its field order and nullable update wrappers.

use super::{InvalidOptions, ResumableOptions};
use crate::storage::failure::unavailable;
use crate::{
    acl::Projection,
    auth::{Identity, OBJECTS_CREATE},
    bucket::{BucketAccess, CatalogError},
    object::{
        CustomTime, ExpectedHashes, Hold, MapPatch, ObjectConditions, ObjectMetadataUpdate, Patch,
        WriteMode,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

type CustomMetadata = BTreeMap<String, Option<String>>;

#[derive(Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Field<T> {
    #[serde(deserialize_with = "T::deserialize")]
    value: T,
}

#[derive(Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>, C: Deserialize<'de>"))]
#[serde(rename_all = "camelCase")]
struct Metadata<T, C> {
    replace: Option<bool>,
    #[serde(default)]
    event_based_hold: Hold,
    #[serde(default)]
    temporary_hold: Hold,
    custom_time: Option<T>,
    content_type: Option<T>,
    content_encoding: Option<T>,
    content_disposition: Option<T>,
    content_language: Option<T>,
    cache_control: Option<T>,
    metadata: Option<C>,
    #[serde(default)]
    retention: Value,
    customer_key_sha256: Option<[u8; 32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_class: Option<crate::bucket::StorageClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predefined_acl: Option<Value>,
}

impl<T, C> Metadata<T, C> {
    fn into_update(
        self,
        string: impl Fn(T) -> Option<String>,
        custom: impl Fn(C) -> Option<CustomMetadata>,
    ) -> Result<ObjectMetadataUpdate, InvalidOptions> {
        Ok(ObjectMetadataUpdate {
            write: if self.replace == Some(true) {
                WriteMode::Replace
            } else {
                WriteMode::Patch
            },
            event_based_hold: self.event_based_hold.into(),
            temporary_hold: self.temporary_hold.into(),
            custom_time: restored(self.custom_time, &string).map(CustomTime::from_text),
            content_type: restored(self.content_type, &string),
            content_encoding: restored(self.content_encoding, &string),
            content_disposition: restored(self.content_disposition, &string),
            content_language: restored(self.content_language, &string),
            cache_control: restored(self.cache_control, string),
            metadata: self.metadata.map_or(MapPatch::Keep, |stored| {
                custom(stored).map_or(MapPatch::Clear, MapPatch::Merge)
            }),
            retention: super::super::objects::retention::from_session(&self.retention)?,
            customer_key_sha256: self.customer_key_sha256,
            storage_class: self.storage_class,
            acl: self
                .predefined_acl
                .map(|value| {
                    value
                        .as_str()
                        .and_then(|value| value.parse::<crate::acl::PredefinedObjectAcl>().ok())
                        .ok_or_else(|| InvalidOptions::new("invalid resumable predefinedAcl"))
                })
                .transpose()?
                .into(),
            override_unlocked_retention: crate::object::retention::UnlockedOverride::NotRequested,
        })
    }
}

/// A nullable update field as the session document stores it: absent keeps, `{"value":null}`
/// clears, `{"value":v}` sets.
fn stored<T, U>(patch: &Patch<T>, convert: impl FnOnce(&T) -> U) -> Option<Field<Option<U>>> {
    match patch {
        Patch::Keep => None,
        Patch::Clear => Some(Field { value: None }),
        Patch::Set(value) => Some(Field {
            value: Some(convert(value)),
        }),
    }
}

/// Custom metadata as the document stores it. The document has no map mode: no session holds a
/// replacement (only UpdateObject builds one), and one is written as a merge of its values.
fn stored_metadata(patch: &MapPatch) -> Option<Field<Option<CustomMetadata>>> {
    match patch {
        MapPatch::Keep => None,
        MapPatch::Clear => Some(Field { value: None }),
        MapPatch::Merge(entries) => Some(Field {
            value: Some(entries.clone()),
        }),
        MapPatch::Replace(entries) => Some(Field {
            value: Some(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), Some(value.clone())))
                    .collect(),
            ),
        }),
    }
}

/// A stored field read back; `nullable` unwraps the version's wrapper (v1 stores a bare value).
fn restored<S>(stored: Option<S>, nullable: impl Fn(S) -> Option<String>) -> Patch<String> {
    stored.map_or(Patch::Keep, |stored| Patch::nullable(nullable(stored)))
}

#[derive(Serialize, Deserialize)]
struct Access {
    identity: Option<Value>,
    permission: String,
    project: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct StoredOptions {
    #[serde(default)]
    version: Value,
    content_type: String,
    metadata: Value,
    md5: Option<[u8; 16]>,
    crc32c: Option<u32>,
    conditions: [Option<i64>; 4],
    access: Option<Access>,
    origin: Option<String>,
    full: bool,
    response_fields: Option<String>,
}

impl StoredOptions {
    /// `bucket_project` narrows the principal's stored grants to the one project that can
    /// authorize a new upload; without it they are stored as they are.
    pub(super) fn from_options(
        options: &ResumableOptions,
        bucket_project: Option<&str>,
    ) -> Result<Self, CatalogError> {
        let m = &options.metadata;
        let predefined_acl = match &m.acl {
            crate::acl::AclRequest::Omitted => None,
            crate::acl::AclRequest::Predefined(acl) => Some(acl.as_str().into()),
            // No upload names explicit entries, and the session document has no place for them.
            crate::acl::AclRequest::Entries(_) => {
                return Err(unavailable(
                    "explicit ACL entries cannot be persisted with a resumable session",
                ));
            }
        };
        let metadata = Metadata {
            replace: Some(m.write == WriteMode::Replace),
            event_based_hold: m.event_based_hold.into(),
            temporary_hold: m.temporary_hold.into(),
            custom_time: stored(&m.custom_time, |time| time.as_str().to_owned()),
            content_type: stored(&m.content_type, String::clone),
            content_encoding: stored(&m.content_encoding, String::clone),
            content_disposition: stored(&m.content_disposition, String::clone),
            content_language: stored(&m.content_language, String::clone),
            cache_control: stored(&m.cache_control, String::clone),
            metadata: stored_metadata(&m.metadata),
            retention: super::super::objects::retention::session_value(&m.retention)
                .map_err(unavailable)?,
            customer_key_sha256: m.customer_key_sha256,
            storage_class: m.storage_class,
            predefined_acl,
        };
        let access = match &options.access {
            BucketAccess::Permissive => None,
            // Administrative calls never start a resumable session.
            BucketAccess::Admin(_) => {
                return Err(unavailable(
                    "administrative grants cannot be persisted as resumable credentials",
                ));
            }
            BucketAccess::Policy {
                identity,
                permission,
                project,
            } => Some(Access {
                identity: identity.as_ref().map(|identity| match bucket_project {
                    Some(project) => identity.upload_snapshot_for(project),
                    None => identity.upload_snapshot(),
                }),
                permission: permission.as_str().into(),
                project: project.clone(),
            }),
        };
        let c = options.conditions;
        Ok(Self {
            version: 2.into(),
            content_type: options.content_type.clone(),
            metadata: serde_json::to_value(metadata).map_err(unavailable)?,
            md5: options.expected.md5,
            crc32c: options.expected.crc32c,
            conditions: [
                c.if_generation_match,
                c.if_generation_not_match,
                c.if_metageneration_match,
                c.if_metageneration_not_match,
            ],
            access,
            origin: options.origin.clone(),
            full: options.projection == Projection::Full,
            response_fields: options.response_fields.clone(),
        })
    }

    fn into_options(self) -> Result<ResumableOptions, InvalidOptions> {
        let metadata = if self.version == 2 {
            let metadata: Metadata<Field<Option<String>>, Field<Option<CustomMetadata>>> =
                serde_json::from_value(self.metadata).map_err(invalid)?;
            if metadata.replace.is_none() {
                return Err(InvalidOptions::new("invalid resumable options"));
            }
            metadata.into_update(|field| field.value, |field| field.value)?
        } else {
            let mut metadata: Metadata<String, CustomMetadata> =
                serde_json::from_value(self.metadata).map_err(invalid)?;
            metadata.replace = None;
            metadata.into_update(Some, Some)?
        };
        let access = match self.access {
            None => BucketAccess::Permissive,
            Some(access) => {
                if access.permission != OBJECTS_CREATE.as_str() {
                    return Err(InvalidOptions::new("invalid resumable options"));
                }
                BucketAccess::Policy {
                    identity: access
                        .identity
                        .as_ref()
                        .map(Identity::from_upload_snapshot)
                        .transpose()
                        .map_err(InvalidOptions::from_error)?,
                    permission: crate::auth::Permission::ObjectsCreate,
                    project: access.project,
                }
            }
        };
        let [
            if_generation_match,
            if_generation_not_match,
            if_metageneration_match,
            if_metageneration_not_match,
        ] = self.conditions;
        Ok(ResumableOptions {
            content_type: self.content_type,
            metadata,
            expected: ExpectedHashes {
                md5: self.md5,
                crc32c: self.crc32c,
            },
            conditions: ObjectConditions {
                if_generation_match,
                if_generation_not_match,
                if_metageneration_match,
                if_metageneration_not_match,
            },
            access,
            origin: self.origin,
            projection: if self.full {
                Projection::Full
            } else {
                Projection::NoAcl
            },
            response_fields: self.response_fields,
        })
    }
}

fn invalid(error: serde_json::Error) -> InvalidOptions {
    InvalidOptions::with_source("invalid resumable options", error)
}

pub(super) fn decode(value: &Value) -> Result<ResumableOptions, InvalidOptions> {
    StoredOptions::deserialize(value)
        .map_err(invalid)?
        .into_options()
}
