//! MoveObject and RestoreObject.

use super::{
    Storage, convert, names, pb,
    status::{invalid_field, unsupported},
};
use crate::bucket::name::BucketKey;
use crate::{
    auth::{self, Caller},
    object::{
        ObjectConditions, ObjectTarget,
        generation::{Generation, InvalidGeneration},
    },
    storage::{MoveAccess, MoveConditions, MoveNames, RestoreAccess},
};
use tonic::{Request, Response, Status};

/// One side's conditions of a MoveObject. Unlike the other object RPCs, which compare a match
/// beside a not-match, Cloud Storage refused such a pair here, for the destination and for the
/// source, while it compared a metageneration match of 0, which failed (recorded 2026-11-18).
/// The pair is refused by its shape: pairs of different values, whether the match failed or both
/// conditions held, were refused alike (case grpc-probe-inferred-conditions).
fn move_conditions(requested: ObjectConditions) -> Result<ObjectConditions, Status> {
    let generation_pair =
        requested.if_generation_match.is_some() && requested.if_generation_not_match.is_some();
    let metageneration_pair = requested.if_metageneration_match.is_some()
        && requested.if_metageneration_not_match.is_some();
    if generation_pair || metageneration_pair {
        return Err(invalid_field(
            "Found conflicting object preconditions. Mutually exclusive fields cannot be set simultaneously.",
        ));
    }
    Ok(requested)
}

impl Storage {
    pub(super) async fn move_object(
        &self,
        request: Request<pb::MoveObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        let (bucket, source) = self.object_name(&r.bucket, &r.source_object)?;
        names::object(&r.destination_object)?;
        let read = self.access(caller, bucket, auth::OBJECTS_GET).await?;
        let delete = self.access(caller, bucket, auth::OBJECTS_DELETE).await?;
        let create = self.access(caller, bucket, auth::OBJECTS_CREATE).await?;
        let sc = move_conditions(r.source_conditions())?;
        let dc = move_conditions(ObjectConditions::from(&r))?;
        let object_names = MoveNames {
            bucket: BucketKey::from_unchecked(bucket.into()),
            source: source.into(),
            destination: r.destination_object,
        };
        let conditions = MoveConditions {
            source: sc,
            destination: dc,
        };
        let record = self
            .state
            .catalog
            .move_object(
                object_names,
                conditions,
                MoveAccess {
                    source_read: &read,
                    source_delete: &delete,
                    destination_create: &create,
                },
            )
            .await?;
        // As for ComposeObject, Cloud Storage answers without acl or owner (case
        // grpc-unsupported-options).
        Ok(Response::new(convert::object(
            &record,
            crate::csek::StoredHashes::keyless(record.customer_key_sha256),
        )?))
    }
    pub(super) async fn restore_object(
        &self,
        request: Request<pb::RestoreObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        super::customer_key::reject(r.common_object_request_params.as_ref(), "RestoreObject")?;
        if r.copy_source_acl == Some(true) || !r.restore_token.is_empty() {
            return Err(unsupported(
                "RestoreObject ACL/token; use copy_source_acl=false and omit restore_token",
            ));
        }
        let generation = Generation::try_from(r.generation).map_err(|InvalidGeneration| {
            Status::invalid_argument(
                "generation is required; select a positive soft-deleted generation",
            )
        })?;
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let restore = self.access(caller, bucket, auth::OBJECTS_RESTORE).await?;
        let create = self.access(caller, bucket, auth::OBJECTS_CREATE).await?;
        // The bucket's soft delete policy is checked before the conditions, which the catalog
        // then compares (recorded 2026-11-18 without a policy; with one, comparing is inferred
        // from the other object RPCs).
        let c = ObjectConditions::from(&r);
        let target = ObjectTarget::new(bucket, name, generation.get());
        let access = RestoreAccess {
            restore: &restore,
            create: &create,
        };
        let record = self.state.catalog.restore_object(target, c, access).await?;
        Ok(Response::new(
            self.written_object(
                &record,
                crate::csek::StoredHashes::keyless(record.customer_key_sha256),
                caller,
            )
            .await?,
        ))
    }
}

#[cfg(test)]
mod conditions;
