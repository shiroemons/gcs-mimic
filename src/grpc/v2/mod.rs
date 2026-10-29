mod append;
mod bidi_read;
mod buckets;
pub mod convert;
pub mod names;
mod object_io;
pub mod status;
#[cfg(test)]
mod tests;
mod update_copy;

use crate::grpc::generated::google::{iam::v1 as iam, storage::v2 as pb};
use crate::{
    auth::{AuthFailure, Caller},
    server::State,
};
use std::{pin::Pin, sync::Arc};
use tonic::{Request, Response, Status, Streaming};

pub type ResponseStream<T> =
    Pin<Box<dyn tonic::codegen::tokio_stream::Stream<Item = Result<T, Status>> + Send>>;

pub struct Storage {
    pub state: Arc<State>,
    pub max_object_bytes: u64,
}

pub fn service(
    state: Arc<State>,
    max_object_bytes: u64,
) -> pb::storage_server::StorageServer<Storage> {
    pb::storage_server::StorageServer::new(Storage {
        state,
        max_object_bytes,
    })
    .max_decoding_message_size(4 * 1024 * 1024)
    .max_encoding_message_size(4 * 1024 * 1024)
}

pub fn known_method(path: &str) -> bool {
    path.strip_prefix("/google.storage.v2.Storage/")
        .is_some_and(|method| {
            matches!(
                method,
                "DeleteBucket"
                    | "GetBucket"
                    | "CreateBucket"
                    | "ListBuckets"
                    | "LockBucketRetentionPolicy"
                    | "GetIamPolicy"
                    | "SetIamPolicy"
                    | "TestIamPermissions"
                    | "UpdateBucket"
                    | "ComposeObject"
                    | "DeleteObject"
                    | "RestoreObject"
                    | "CancelResumableWrite"
                    | "GetObject"
                    | "ReadObject"
                    | "BidiReadObject"
                    | "UpdateObject"
                    | "WriteObject"
                    | "BidiWriteObject"
                    | "ListObjects"
                    | "RewriteObject"
                    | "StartResumableWrite"
                    | "QueryWriteStatus"
                    | "MoveObject"
            )
        })
}

impl Storage {
    pub fn authenticate<T>(&self, request: &Request<T>) -> Result<Option<Caller<'_>>, Status> {
        let headers = request.metadata().clone().into_headers();
        let caller = self
            .state
            .auth
            .authenticate(&headers, None)
            .map_err(auth_error)?;
        Ok(caller)
    }
}

fn auth_error(_: AuthFailure) -> Status {
    Status::unauthenticated(
        "gcs-mimic strict authentication requires a registered local Bearer token; supply authorization: Bearer local-admin-token with a matching identity_file",
    )
}

#[tonic::async_trait]
impl pb::storage_server::Storage for Storage {
    type ReadObjectStream = ResponseStream<pb::ReadObjectResponse>;
    type BidiReadObjectStream = ResponseStream<pb::BidiReadObjectResponse>;
    type BidiWriteObjectStream = ResponseStream<pb::BidiWriteObjectResponse>;

    async fn delete_bucket(
        &self,
        request: Request<pb::DeleteBucketRequest>,
    ) -> Result<Response<()>, Status> {
        let caller = self.authenticate(&request)?;
        self.delete_bucket(request, caller).await
    }
    async fn get_bucket(
        &self,
        request: Request<pb::GetBucketRequest>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let caller = self.authenticate(&request)?;
        self.get_bucket(request, caller).await
    }
    async fn create_bucket(
        &self,
        request: Request<pb::CreateBucketRequest>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let caller = self.authenticate(&request)?;
        self.create_bucket(request, caller).await
    }
    async fn list_buckets(
        &self,
        request: Request<pb::ListBucketsRequest>,
    ) -> Result<Response<pb::ListBucketsResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.list_buckets(request, caller).await
    }
    async fn lock_bucket_retention_policy(
        &self,
        request: Request<pb::LockBucketRetentionPolicyRequest>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let caller = self.authenticate(&request)?;
        self.lock_bucket_retention_policy(request, caller).await
    }
    async fn get_iam_policy(
        &self,
        request: Request<iam::GetIamPolicyRequest>,
    ) -> Result<Response<iam::Policy>, Status> {
        let caller = self.authenticate(&request)?;
        self.get_iam_policy(request, caller).await
    }
    async fn set_iam_policy(
        &self,
        request: Request<iam::SetIamPolicyRequest>,
    ) -> Result<Response<iam::Policy>, Status> {
        let caller = self.authenticate(&request)?;
        self.set_iam_policy(request, caller).await
    }
    async fn test_iam_permissions(
        &self,
        request: Request<iam::TestIamPermissionsRequest>,
    ) -> Result<Response<iam::TestIamPermissionsResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.test_iam_permissions(request, caller).await
    }
    async fn update_bucket(
        &self,
        request: Request<pb::UpdateBucketRequest>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let caller = self.authenticate(&request)?;
        self.update_bucket(request, caller).await
    }
    async fn compose_object(
        &self,
        request: Request<pb::ComposeObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        self.compose_object(request, caller).await
    }
    async fn delete_object(
        &self,
        request: Request<pb::DeleteObjectRequest>,
    ) -> Result<Response<()>, Status> {
        let caller = self.authenticate(&request)?;
        self.delete_object(request, caller).await
    }
    async fn restore_object(
        &self,
        request: Request<pb::RestoreObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        self.restore_object(request, caller).await
    }
    async fn cancel_resumable_write(
        &self,
        request: Request<pb::CancelResumableWriteRequest>,
    ) -> Result<Response<pb::CancelResumableWriteResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.cancel_resumable_write(request, caller).await
    }
    async fn get_object(
        &self,
        request: Request<pb::GetObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        self.get_object(request, caller).await
    }
    async fn read_object(
        &self,
        request: Request<pb::ReadObjectRequest>,
    ) -> Result<Response<Self::ReadObjectStream>, Status> {
        let caller = self.authenticate(&request)?;
        self.read_object(request, caller).await
    }
    async fn bidi_read_object(
        &self,
        request: Request<Streaming<pb::BidiReadObjectRequest>>,
    ) -> Result<Response<Self::BidiReadObjectStream>, Status> {
        let caller = self.authenticate(&request)?;
        self.bidi_read_object(request, caller).await
    }
    async fn update_object(
        &self,
        request: Request<pb::UpdateObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        self.update_object(request, caller).await
    }
    async fn write_object(
        &self,
        request: Request<Streaming<pb::WriteObjectRequest>>,
    ) -> Result<Response<pb::WriteObjectResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.write_object(request, caller).await
    }
    async fn bidi_write_object(
        &self,
        request: Request<Streaming<pb::BidiWriteObjectRequest>>,
    ) -> Result<Response<Self::BidiWriteObjectStream>, Status> {
        let caller = self.authenticate(&request)?;
        self.bidi_write_object(request, caller).await
    }
    async fn list_objects(
        &self,
        request: Request<pb::ListObjectsRequest>,
    ) -> Result<Response<pb::ListObjectsResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.list_objects(request, caller).await
    }
    async fn rewrite_object(
        &self,
        request: Request<pb::RewriteObjectRequest>,
    ) -> Result<Response<pb::RewriteResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.rewrite_object(request, caller).await
    }
    async fn start_resumable_write(
        &self,
        request: Request<pb::StartResumableWriteRequest>,
    ) -> Result<Response<pb::StartResumableWriteResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.start_resumable_write(request, caller).await
    }
    async fn query_write_status(
        &self,
        request: Request<pb::QueryWriteStatusRequest>,
    ) -> Result<Response<pb::QueryWriteStatusResponse>, Status> {
        let caller = self.authenticate(&request)?;
        self.query_write_status(request, caller).await
    }
    async fn move_object(
        &self,
        request: Request<pb::MoveObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        self.move_object(request, caller).await
    }
}
