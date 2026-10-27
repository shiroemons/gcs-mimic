use super::{Storage, iam, pb, status::unsupported_for as unsupported};
use crate::auth::Caller;
use tonic::{Request, Response, Status};

impl Storage {
    pub(super) async fn delete_bucket(
        &self,
        _request: Request<pb::DeleteBucketRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        Err(unsupported("DeleteBucket", _caller))
    }
    pub(super) async fn get_bucket(
        &self,
        _request: Request<pb::GetBucketRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        Err(unsupported("GetBucket", _caller))
    }
    pub(super) async fn create_bucket(
        &self,
        _request: Request<pb::CreateBucketRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        Err(unsupported("CreateBucket", _caller))
    }
    pub(super) async fn list_buckets(
        &self,
        _request: Request<pb::ListBucketsRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListBucketsResponse>, Status> {
        Err(unsupported("ListBuckets", _caller))
    }
    pub(super) async fn lock_bucket_retention_policy(
        &self,
        _request: Request<pb::LockBucketRetentionPolicyRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        Err(unsupported("LockBucketRetentionPolicy", _caller))
    }
    pub(super) async fn get_iam_policy(
        &self,
        _request: Request<iam::GetIamPolicyRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::Policy>, Status> {
        Err(unsupported("GetIamPolicy", _caller))
    }
    pub(super) async fn set_iam_policy(
        &self,
        _request: Request<iam::SetIamPolicyRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::Policy>, Status> {
        Err(unsupported("SetIamPolicy", _caller))
    }
    pub(super) async fn test_iam_permissions(
        &self,
        _request: Request<iam::TestIamPermissionsRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::TestIamPermissionsResponse>, Status> {
        Err(unsupported("TestIamPermissions", _caller))
    }
    pub(super) async fn update_bucket(
        &self,
        _request: Request<pb::UpdateBucketRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        Err(unsupported("UpdateBucket", _caller))
    }
}
