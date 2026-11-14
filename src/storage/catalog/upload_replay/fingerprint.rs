use crate::object::{
    MapMode, ObjectConditions, ObjectError, ObjectMetadataUpdate, Patch, WriteMode,
};
use ring::digest::{Context, SHA256};

/// Versioned request identity, independent of diagnostic and serialization formats.
pub(in crate::storage::catalog) struct UploadFingerprint<'a> {
    pub(in crate::storage::catalog) bucket: &'a str,
    pub(in crate::storage::catalog) name: &'a str,
    pub(in crate::storage::catalog) content_type: &'a str,
    pub(in crate::storage::catalog) metadata: &'a ObjectMetadataUpdate,
    pub(in crate::storage::catalog) conditions: ObjectConditions,
    pub(in crate::storage::catalog) size: u64,
    pub(in crate::storage::catalog) sha256: [u8; 32],
}

impl UploadFingerprint<'_> {
    pub(in crate::storage::catalog) fn digest(&self) -> Result<[u8; 32], ObjectError> {
        let mut encoded = Canonical::new(b"upload-v2");
        encoded.text(self.bucket);
        encoded.text(self.name);
        encoded.text(self.content_type);
        encoded.metadata(self.metadata);
        let ObjectConditions {
            if_generation_match,
            if_generation_not_match,
            if_metageneration_match,
            if_metageneration_not_match,
        } = self.conditions;
        for condition in [
            if_generation_match,
            if_generation_not_match,
            if_metageneration_match,
            if_metageneration_not_match,
        ] {
            encoded.optional(condition, |encoded, value| encoded.i64(value));
        }
        encoded.u64(self.size);
        encoded.bytes(&self.sha256);
        encoded.finish()
    }
}

pub(in crate::storage::catalog) fn copy_fingerprint(
    request: &str,
) -> Result<[u8; 32], ObjectError> {
    let mut encoded = Canonical::new(b"copy-v2");
    encoded.text(request);
    encoded.finish()
}

struct Canonical(Context);

impl Canonical {
    fn new(domain: &[u8]) -> Self {
        let mut encoded = Self(Context::new(&SHA256));
        encoded.bytes(domain);
        encoded
    }

    fn finish(self) -> Result<[u8; 32], ObjectError> {
        self.0
            .finish()
            .as_ref()
            .try_into()
            .map_err(crate::storage::failure::object_unavailable)
    }

    fn u64(&mut self, value: u64) {
        self.0.update(&value.to_le_bytes());
    }
    fn i64(&mut self, value: i64) {
        self.0.update(&value.to_le_bytes());
    }
    fn bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.0.update(value);
    }
    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "encodes stored boolean values, without selecting an operation mode"
    )]
    fn flag(&mut self, value: bool) {
        self.0.update(&[u8::from(value)]);
    }
    fn optional<T>(&mut self, value: Option<T>, write: impl FnOnce(&mut Self, T)) {
        self.0.update(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            write(self, value);
        }
    }
    /// The bytes the nested `Option` wrote before the field became a [`Patch`]: `0` keeps,
    /// `1 0` clears, `1 1` and the value sets.
    fn patch<T>(&mut self, value: &Patch<T>, write: impl FnOnce(&mut Self, &T)) {
        match value {
            Patch::Keep => self.0.update(&[0]),
            Patch::Clear => self.0.update(&[1, 0]),
            Patch::Set(value) => {
                self.0.update(&[1, 1]);
                write(self, value);
            }
        }
    }

    fn metadata(&mut self, metadata: &ObjectMetadataUpdate) {
        // Exhaustive fields make changes to the persisted request format a deliberate choice.
        let ObjectMetadataUpdate {
            acl,
            predefined_acl,
            write,
            content_type,
            content_encoding,
            content_disposition,
            content_language,
            cache_control,
            metadata,
            metadata_mode,
            event_based_hold,
            temporary_hold,
            custom_time,
            retention,
            override_unlocked_retention,
            customer_key_sha256,
            storage_class,
        } = metadata;
        self.optional(acl.as_ref(), |encoded, entries| {
            encoded.u64(entries.len() as u64);
            for entry in entries {
                encoded.text(&entry.entity);
                encoded.text(entry.role.as_str());
            }
        });
        self.optional(*predefined_acl, |encoded, acl| encoded.text(acl.as_str()));
        self.0.update(&[match write {
            WriteMode::Patch => 0,
            WriteMode::Replace => 1,
        }]);
        for field in [
            content_type,
            content_encoding,
            content_disposition,
            content_language,
            cache_control,
            custom_time,
        ] {
            self.patch(field, |encoded, value| encoded.text(value));
        }
        self.optional(metadata.as_ref(), |encoded, value| {
            encoded.optional(value.as_ref(), |encoded, entries| {
                encoded.u64(entries.len() as u64);
                for (key, value) in entries {
                    encoded.text(key);
                    encoded.optional(value.as_deref(), Self::text);
                }
            })
        });
        self.optional(*event_based_hold, Self::flag);
        self.optional(*temporary_hold, Self::flag);
        self.optional(retention.as_ref(), |encoded, value| {
            encoded.optional(*value, |encoded, value| {
                encoded.text(value.mode.as_str());
                encoded.i64(value.retain_until.secs());
                encoded.u64(u64::from(value.retain_until.subsec_nanos()));
            })
        });
        self.0.update(&[match override_unlocked_retention {
            crate::object::retention::UnlockedOverride::NotRequested => 0,
            crate::object::retention::UnlockedOverride::Requested => 1,
        }]);
        self.optional(customer_key_sha256.as_ref(), |encoded, key| {
            encoded.bytes(key)
        });
        self.optional(*storage_class, |encoded, class| {
            encoded.text(class.as_str())
        });
        // Merge keeps the original v2 bytes; only replacement adds a tagged extension.
        match metadata_mode {
            MapMode::Merge => {}
            MapMode::Replace => {
                self.bytes(b"metadata-map-mode");
                self.bytes(b"replace");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(metadata: &ObjectMetadataUpdate) -> UploadFingerprint<'_> {
        UploadFingerprint {
            bucket: "bucket",
            name: "object",
            content_type: "text/plain",
            metadata,
            conditions: ObjectConditions::default(),
            size: 5,
            sha256: [7; 32],
        }
    }

    #[test]
    fn every_metadata_field_contributes_to_request_identity() {
        let mut metadata =
            crate::storage::catalog::resumable::golden_tests::populated_options().metadata;
        metadata.acl = Some(vec![crate::acl::AclEntry {
            entity: "allUsers".into(),
            role: crate::acl::AclRole::Reader,
        }]);
        metadata.override_unlocked_retention =
            crate::object::retention::UnlockedOverride::Requested;
        let expected = request(&metadata).digest().unwrap();
        let mutations: [fn(&mut ObjectMetadataUpdate); 17] = [
            |value| value.acl = None,
            |value| value.predefined_acl = None,
            |value| value.write = WriteMode::Patch,
            |value| value.content_type = Patch::Keep,
            |value| value.content_encoding = Patch::Keep,
            |value| value.content_disposition = Patch::Keep,
            |value| value.content_language = Patch::Keep,
            |value| value.cache_control = Patch::Keep,
            |value| value.metadata = None,
            |value| value.metadata_mode = MapMode::Replace,
            |value| value.event_based_hold = None,
            |value| value.temporary_hold = None,
            |value| value.custom_time = Patch::Keep,
            |value| value.retention = None,
            |value| {
                value.override_unlocked_retention =
                    crate::object::retention::UnlockedOverride::NotRequested
            },
            |value| value.customer_key_sha256 = None,
            |value| value.storage_class = None,
        ];
        for (index, mutate) in mutations.into_iter().enumerate() {
            let mut changed = metadata.clone();
            mutate(&mut changed);
            assert_ne!(
                request(&changed).digest().unwrap(),
                expected,
                "field {index}"
            );
        }
    }

    #[test]
    fn metadata_map_modes_have_distinct_request_identities() {
        for metadata in [None, Some(None), Some(Some(Default::default()))] {
            let mut update = ObjectMetadataUpdate {
                metadata,
                ..Default::default()
            };
            let default = request(&update).digest().unwrap();
            update.metadata_mode = MapMode::Merge;
            assert_eq!(request(&update).digest().unwrap(), default);
            update.metadata_mode = MapMode::Replace;
            assert_ne!(request(&update).digest().unwrap(), default);
        }
    }

    #[test]
    fn nullable_fields_and_empty_strings_have_distinct_identities() {
        let mut metadata = ObjectMetadataUpdate::default();
        let absent = request(&metadata).digest().unwrap();
        metadata.content_type = Patch::Clear;
        let removed = request(&metadata).digest().unwrap();
        metadata.content_type = Patch::Set(String::new());
        let empty = request(&metadata).digest().unwrap();
        assert_ne!(absent, removed);
        assert_ne!(absent, empty);
        assert_ne!(removed, empty);
        assert_ne!(empty, copy_fingerprint("object").unwrap());
    }

    /// Stored fingerprints survive an upgrade, so clearing a field keeps the bytes earlier
    /// releases wrote for it.
    #[test]
    fn cleared_fields_keep_their_golden_digest() {
        let metadata = ObjectMetadataUpdate {
            content_type: Patch::Clear,
            content_encoding: Patch::Clear,
            content_disposition: Patch::Clear,
            content_language: Patch::Clear,
            cache_control: Patch::Clear,
            metadata: Some(None),
            custom_time: Patch::Clear,
            retention: Some(None),
            ..Default::default()
        };
        let digest = request(&metadata).digest().unwrap();
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            "90ae3438bc8dea7a5af82941eb5b7ce99c32bf859364ce28ffb71fdc2b954582"
        );
    }

    #[test]
    fn every_request_condition_contributes_to_identity() {
        let metadata = ObjectMetadataUpdate::default();
        let expected = request(&metadata).digest().unwrap();
        let variants = [
            ObjectConditions {
                if_generation_match: Some(1),
                ..Default::default()
            },
            ObjectConditions {
                if_generation_not_match: Some(1),
                ..Default::default()
            },
            ObjectConditions {
                if_metageneration_match: Some(1),
                ..Default::default()
            },
            ObjectConditions {
                if_metageneration_not_match: Some(1),
                ..Default::default()
            },
        ];
        for conditions in variants {
            let mut changed = request(&metadata);
            changed.conditions = conditions;
            assert_ne!(changed.digest().unwrap(), expected);
        }
    }
}
