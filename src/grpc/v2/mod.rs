mod append;
mod bidi_read;
pub use bidi_read::ReadHandles;
mod bucket_update;
mod buckets;
pub(crate) mod convert;
pub(crate) mod names;
mod object_io;
pub(crate) mod status;
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

pub(crate) type ResponseStream<T> =
    Pin<Box<dyn tonic::codegen::tokio_stream::Stream<Item = Result<T, Status>> + Send>>;

pub(crate) struct Storage {
    pub state: Arc<State>,
    pub max_object_bytes: u64,
}

pub(crate) fn service(
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

/// The Storage v2 method that serves a Storage Control bucket IAM call.
///
/// The Rust SDK sends bucket IAM calls only to `google.storage.control.v2.StorageControl`.
/// Both services declare these methods with the same `google.iam.v1` request and response
/// messages, so the Storage v2 handlers, with their shared policy store and authorization,
/// serve both services without a second protobuf closure.
fn control_iam_path(path: &str) -> Option<&'static str> {
    match path.strip_prefix("/google.storage.control.v2.StorageControl/")? {
        "GetIamPolicy" => Some("/google.storage.v2.Storage/GetIamPolicy"),
        "SetIamPolicy" => Some("/google.storage.v2.Storage/SetIamPolicy"),
        "TestIamPermissions" => Some("/google.storage.v2.Storage/TestIamPermissions"),
        _ => None,
    }
}

/// Serves a request outside the legacy v1 service, rejecting methods without a handler.
pub(crate) async fn dispatch(
    mut service: pb::storage_server::StorageServer<Storage>,
    mut request: hyper::Request<tonic::body::Body>,
) -> Result<hyper::Response<tonic::body::Body>, std::convert::Infallible> {
    use tonic::codegen::Service;
    if let Some(path) = control_iam_path(request.uri().path()) {
        let mut parts = request.uri().clone().into_parts();
        parts.path_and_query = Some(hyper::http::uri::PathAndQuery::from_static(path));
        match hyper::Uri::from_parts(parts) {
            Ok(uri) => *request.uri_mut() = uri,
            Err(_) => return Ok(Status::internal("Cannot route the IAM request").into_http()),
        }
    }
    if !known_method(request.uri().path()) {
        return Ok(status::unsupported("unknown service or method").into_http());
    }
    service.call(request).await
}

impl Storage {
    /// Callers validate resource names before authorization.
    async fn access(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        permission: &str,
    ) -> Result<crate::bucket::BucketAccess, Status> {
        crate::server::authorize_bucket(&self.state, caller, permission, bucket)
            .await
            .map_err(|error| access_status(error, caller))
    }

    pub(crate) fn authenticate<T>(
        &self,
        request: &Request<T>,
    ) -> Result<Option<Caller<'_>>, Status> {
        let headers = request.metadata().clone().into_headers();
        let caller = self
            .state
            .auth
            .authenticate_at(&headers, None, self.state.clock.as_ref())
            .map_err(auth_error)?;
        Ok(caller)
    }
}

/// Anonymous denials ask for credentials, matching the shared JSON authorization rules.
fn access_status(error: impl Into<Status>, caller: Option<Caller<'_>>) -> Status {
    status::anonymous(error.into(), caller)
}

fn auth_error(_: AuthFailure) -> Status {
    Status::unauthenticated(
        "gcs-mimic strict authentication requires a registered local Bearer token; supply authorization: Bearer local-admin-token with a matching identity_file",
    )
}

// Keep dispatch and forwarding in sync; a misspelled wire name rejects that RPC as unsupported.
macro_rules! storage_methods {
    ($($wire:ident => $method:ident($request:ty $(,)?) -> $response:ty;)*) => {
        pub(crate) fn known_method(path: &str) -> bool {
            path.strip_prefix("/google.storage.v2.Storage/")
                .is_some_and(|method| matches!(method, $(stringify!($wire))|*))
        }

        #[tonic::async_trait]
        impl pb::storage_server::Storage for Storage {
            type ReadObjectStream = ResponseStream<pb::ReadObjectResponse>;
            type BidiReadObjectStream = ResponseStream<pb::BidiReadObjectResponse>;
            type BidiWriteObjectStream = ResponseStream<pb::BidiWriteObjectResponse>;

            $(
                async fn $method(
                    &self,
                    request: Request<$request>,
                ) -> Result<Response<$response>, Status> {
                    let caller = self.authenticate(&request)?;
                    self.$method(request, caller)
                        .await
                        .map_err(|status| status::anonymous(status, caller))
                }
            )*
        }
    };
}

storage_methods! {
    DeleteBucket => delete_bucket(pb::DeleteBucketRequest) -> ();
    GetBucket => get_bucket(pb::GetBucketRequest) -> pb::Bucket;
    CreateBucket => create_bucket(pb::CreateBucketRequest) -> pb::Bucket;
    ListBuckets => list_buckets(pb::ListBucketsRequest) -> pb::ListBucketsResponse;
    LockBucketRetentionPolicy => lock_bucket_retention_policy(
        pb::LockBucketRetentionPolicyRequest,
    ) -> pb::Bucket;
    GetIamPolicy => get_iam_policy(iam::GetIamPolicyRequest) -> iam::Policy;
    SetIamPolicy => set_iam_policy(iam::SetIamPolicyRequest) -> iam::Policy;
    TestIamPermissions => test_iam_permissions(
        iam::TestIamPermissionsRequest,
    ) -> iam::TestIamPermissionsResponse;
    UpdateBucket => update_bucket(pb::UpdateBucketRequest) -> pb::Bucket;
    ComposeObject => compose_object(pb::ComposeObjectRequest) -> pb::Object;
    DeleteObject => delete_object(pb::DeleteObjectRequest) -> ();
    RestoreObject => restore_object(pb::RestoreObjectRequest) -> pb::Object;
    CancelResumableWrite => cancel_resumable_write(
        pb::CancelResumableWriteRequest,
    ) -> pb::CancelResumableWriteResponse;
    GetObject => get_object(pb::GetObjectRequest) -> pb::Object;
    ReadObject => read_object(pb::ReadObjectRequest) -> Self::ReadObjectStream;
    BidiReadObject => bidi_read_object(
        Streaming<pb::BidiReadObjectRequest>,
    ) -> Self::BidiReadObjectStream;
    UpdateObject => update_object(pb::UpdateObjectRequest) -> pb::Object;
    WriteObject => write_object(Streaming<pb::WriteObjectRequest>) -> pb::WriteObjectResponse;
    BidiWriteObject => bidi_write_object(
        Streaming<pb::BidiWriteObjectRequest>,
    ) -> Self::BidiWriteObjectStream;
    ListObjects => list_objects(pb::ListObjectsRequest) -> pb::ListObjectsResponse;
    RewriteObject => rewrite_object(pb::RewriteObjectRequest) -> pb::RewriteResponse;
    StartResumableWrite => start_resumable_write(
        pb::StartResumableWriteRequest,
    ) -> pb::StartResumableWriteResponse;
    QueryWriteStatus => query_write_status(
        pb::QueryWriteStatusRequest,
    ) -> pb::QueryWriteStatusResponse;
    MoveObject => move_object(pb::MoveObjectRequest) -> pb::Object;
}
