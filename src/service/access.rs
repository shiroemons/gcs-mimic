//! The access checks every adapter runs before a request reaches the catalog. Each adapter
//! answers a refusal in its own format.

use crate::{
    auth::{self, Caller},
    bucket::{BucketAccess, CatalogError},
    service::state::State,
};

pub(crate) mod acl;
pub(crate) mod retention;

/// Resolve the owning project of `bucket` and authorize `permission` against it.
///
/// The returned access must be passed to the catalog operation, which re-validates it against
/// the owner it observes in its own transaction: the bucket can be deleted and recreated under
/// another project after this lookup. Permissive mode skips the catalog. Anonymous callers
/// have no project grants and are evaluated against bucket policy.
pub(crate) async fn bucket_access(
    state: &State,
    caller: Option<Caller<'_>>,
    permission: auth::Permission,
    bucket: &str,
) -> Result<BucketAccess, CatalogError> {
    let Some(caller) = caller else {
        return Ok(BucketAccess::Permissive);
    };
    let identity = match caller {
        Caller::Anonymous => None,
        Caller::Principal(identity) => Some(identity),
    };
    state
        .catalog
        .bucket_access(bucket, identity, permission)
        .await
}
