use super::*;
use crate::grpc::v2::mask::ReadMask;

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
    pub(super) fn selected(mask: ReadMask<'_>, prefix: Option<&str>) -> Self {
        let paths = match mask {
            ReadMask::Default => return Self::default(),
            ReadMask::Wildcard | ReadMask::MixedWildcard(_) => return Self::ALL,
            ReadMask::Paths(paths) => paths,
        };
        let path = |field: &str| {
            paths.iter().any(|path| match prefix {
                None => path == field,
                Some(prefix) => {
                    path == prefix
                        || path
                            .strip_prefix(prefix)
                            .and_then(|rest| rest.strip_prefix('.'))
                            .is_some_and(|rest| rest == field)
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

impl Storage {
    /// Fills ACL and owner on the resource a write or update answers with. A caller that may
    /// write but not read the ACL still gets its result, with both fields empty.
    pub(in crate::grpc::v2) async fn written_object(
        &self,
        record: &ObjectRecord,
        hashes: crate::csek::StoredHashes,
        caller: Option<Caller<'_>>,
    ) -> Result<pb::Object, Status> {
        let mut object = convert::object(record, hashes)?;
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
        for (object, object_acl) in objects.iter_mut().zip(acl.objects) {
            let crate::storage::ObjectAcl::Visible { entries, owner } = object_acl else {
                continue;
            };
            if fields.acl {
                object.acl = convert::object_acl(object, bucket, &acl.project, &entries);
            }
            if fields.owner {
                object.owner = owner
                    .and_then(|owner| owner.entity())
                    .map(|entity| pb::Owner {
                        entity: entity.into_string(),
                        entity_id: String::new(),
                    });
            }
        }
        Ok(())
    }
}
