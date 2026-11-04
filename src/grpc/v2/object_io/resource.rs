use super::*;

/// The object fields that a default read mask omits, selected by an explicit mask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OwnerFields {
    acl: bool,
    owner: bool,
}

impl OwnerFields {
    /// Every owner-related field, which Cloud Storage returns on the resource of a write or
    /// update (observed 2026-11-01) though a read returns them only when its mask asks.
    const ALL: Self = Self {
        acl: true,
        owner: true,
    };

    /// `*` selects every field; other paths name top-level fields, optionally under `prefix`
    /// (ReadObject nests the object in `metadata`, where `metadata` alone selects all of it).
    pub(super) fn selected(mask: Option<&prost_types::FieldMask>, prefix: Option<&str>) -> Self {
        let Some(mask) = mask else {
            return Self::default();
        };
        let path = |field: &str| {
            mask.paths.iter().any(|path| {
                path == "*"
                    || match prefix {
                        None => path == field,
                        Some(prefix) => {
                            path == prefix
                                || path
                                    .strip_prefix(prefix)
                                    .and_then(|rest| rest.strip_prefix('.'))
                                    .is_some_and(|rest| rest == field)
                        }
                    }
            })
        };
        Self {
            acl: path("acl"),
            owner: path("owner"),
        }
    }

    fn any(self) -> bool {
        self.acl || self.owner
    }
}

/// Keeps the top-level `Object` fields a read mask names. Every field is accepted; fields the
/// local object cannot hold (KMS key, contexts, restore token of a live object) stay empty.
/// ACL and owner are filled beforehand by [`Storage::add_owner_fields`].
pub(super) fn mask_object(
    mut object: pb::Object,
    mask: Option<&prost_types::FieldMask>,
) -> Result<pb::Object, Status> {
    let Some(mask) = mask.filter(|m| !m.paths.is_empty() && !m.paths.iter().any(|p| p == "*"))
    else {
        return Ok(object);
    };
    // The Java SDK lists with `checksums.crc32c` and `checksums.md5_hash`; a sub-path keeps only
    // that digest, as protobuf field-mask semantics require.
    let digest = |name: &str| {
        mask.paths
            .iter()
            .any(|p| p == "checksums" || p.strip_prefix("checksums.") == Some(name))
    };
    let kept_checksums = object.checksums.take().map(|mut sums| {
        if !digest("crc32c") {
            sums.crc32c = None;
        }
        if !digest("md5_hash") {
            sums.md5_hash = Default::default();
        }
        sums
    });
    macro_rules! fields { ($($field:ident),+ $(,)?) => {{
        for path in &mask.paths {
            if !matches!(path.as_str(), $(stringify!($field))|+ | "checksums.crc32c" | "checksums.md5_hash") {
                // A name that is no Object field is a client error. A sub-path of a real field
                // (`owner.entity`) is only unsupported here: the recording cannot tell whether
                // Cloud Storage accepts it, so it stays an error instead of a false success.
                let root = path.split('.').next().unwrap_or_default();
                return Err(if matches!(root, $(stringify!($field))|+) {
                    unsupported_for(&format!("read_mask path {path}; request a supported top-level object field"), None)
                } else {
                    Status::invalid_argument(format!("There is no field for this resource named '{path}'."))
                });
            }
        }
        $(if !mask.paths.iter().any(|p| p == stringify!($field)) { object.$field = Default::default(); })+
    }} }
    fields!(
        name,
        bucket,
        etag,
        generation,
        restore_token,
        metageneration,
        storage_class,
        size,
        content_encoding,
        content_disposition,
        cache_control,
        acl,
        content_language,
        delete_time,
        finalize_time,
        content_type,
        create_time,
        component_count,
        checksums,
        update_time,
        kms_key,
        update_storage_class_time,
        temporary_hold,
        retention_expire_time,
        metadata,
        contexts,
        event_based_hold,
        owner,
        customer_encryption,
        custom_time,
        soft_delete_time,
        hard_delete_time,
        retention
    );
    if digest("crc32c") || digest("md5_hash") {
        object.checksums = kept_checksums;
    }
    Ok(object)
}

impl Storage {
    /// Fills ACL and owner on the resource a write or update answers with. A caller that may
    /// write but not read the ACL still gets its result, with both fields empty.
    pub(in crate::grpc::v2) async fn written_object(
        &self,
        record: &ObjectRecord,
        caller: Option<Caller<'_>>,
    ) -> Result<pb::Object, Status> {
        let mut object = convert::object(record)?;
        self.add_written_owner_fields(caller, &record.bucket, &mut object)
            .await?;
        Ok(object)
    }

    pub(in crate::grpc::v2) async fn add_written_owner_fields(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        object: &mut pb::Object,
    ) -> Result<(), Status> {
        match self
            .add_owner_fields(
                caller,
                bucket,
                std::slice::from_mut(object),
                OwnerFields::ALL,
            )
            .await
        {
            Err(status)
                if matches!(
                    status.code(),
                    tonic::Code::PermissionDenied | tonic::Code::Unauthenticated
                ) =>
            {
                Ok(())
            }
            other => other,
        }
    }
    /// Fills the ACL and owner a read mask selects, from the same shared projection as JSON
    /// `projection=full`: under uniform bucket-level access, or for an object whose ACL the
    /// caller may not read (`storage.objects.getIamPolicy`), both stay empty and the read still
    /// succeeds.
    pub(super) async fn add_owner_fields(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        objects: &mut [pb::Object],
        fields: OwnerFields,
    ) -> Result<(), Status> {
        if !fields.any() || objects.is_empty() {
            return Ok(());
        }
        let generations = objects
            .iter()
            .map(|object| (object.name.clone(), object.generation))
            .collect();
        let Some(acl) =
            crate::server::readable_object_acls(&self.state, caller, bucket, generations)
                .await
                .map_err(|e| access_status(e, caller))?
        else {
            return Ok(());
        };
        for (((object, entries), owner), visible) in objects
            .iter_mut()
            .zip(acl.objects)
            .zip(acl.owners)
            .zip(acl.visible)
        {
            if !visible {
                continue;
            }
            if fields.acl {
                object.acl = convert::object_acl(object, bucket, &acl.project, &entries);
            }
            if fields.owner {
                object.owner = owner
                    .and_then(|owner| owner.entity())
                    .map(|entity| pb::Owner {
                        entity,
                        entity_id: String::new(),
                    });
            }
        }
        Ok(())
    }
}
