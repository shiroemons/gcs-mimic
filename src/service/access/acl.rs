//! Who may read an ACL, and the ACLs of object generations as a caller may read them, for every
//! API that renders ACLs as resource fields.

use super::bucket_access;
use crate::{
    acl::{AclAccess, AclKind},
    auth::Caller,
    bucket::{BucketAccess, CatalogError},
    service::state::State,
    storage::AclProjection,
};

/// Whether `caller` may read the `kind` ACL of `bucket`; a refusal is `false`, not an error.
pub(crate) async fn may_read(
    state: &State,
    caller: Option<Caller<'_>>,
    kind: AclKind,
    bucket: &str,
) -> Result<bool, CatalogError> {
    match bucket_access(state, caller, kind.permission(AclAccess::Read), bucket).await {
        Ok(access) => state.catalog.has_bucket_permission(bucket, &access).await,
        Err(error) if error.is_refusal() => Ok(false),
        Err(error) => Err(error),
    }
}

/// The ACLs and owners of the selected object generations, in input order, for every API that
/// renders them as resource fields (JSON `projection=full`, gRPC read masks).
///
/// `None` when the bucket is gone or uses uniform bucket-level access, where ACLs are not part
/// of the resource. An object whose ACL the caller may not read is marked invisible.
pub(crate) async fn readable_object_acls(
    state: &State,
    caller: Option<Caller<'_>>,
    bucket: &str,
    generations: Vec<(String, i64)>,
) -> Result<Option<AclProjection>, CatalogError> {
    object_acls(state, AclReader::Caller(caller), bucket, generations).await
}

/// Whose permission decides whether a response carries the ACL fields of `projection=full`.
#[derive(Clone, Copy)]
pub(crate) enum AclReader<'a> {
    /// The request's own caller.
    Caller(Option<Caller<'a>>),
    /// The initiator of a resumable session, as its stored access. A session URI is a capability
    /// that acts for its initiator, so requests on it carry no caller of their own, and passing
    /// none would read the ACL without any permission check.
    Initiator(&'a BucketAccess),
}

impl AclReader<'_> {
    /// The access checked against each selected generation inside the projection snapshot.
    fn object_acl_access(self) -> BucketAccess {
        let read = AclKind::Object.permission(AclAccess::Read);
        match self {
            Self::Caller(caller) => BucketAccess::for_caller(caller, read, None),
            Self::Initiator(access) => access.with_permission(read),
        }
    }
}

/// The ACLs of the selected generations of `bucket` as `reader` may read them; `None` when the
/// bucket is gone or uses uniform bucket-level access.
pub(crate) async fn object_acls(
    state: &State,
    reader: AclReader<'_>,
    bucket: &str,
    generations: Vec<(String, i64)>,
) -> Result<Option<AclProjection>, CatalogError> {
    let access = reader.object_acl_access();
    let acl = state
        .catalog
        .object_acl_projection(bucket, generations, &access)
        .await?;
    Ok(acl.filter(|acl| !acl.uniform_access))
}
