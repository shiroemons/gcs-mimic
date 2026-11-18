mod append;
mod bidi_read;
pub use bidi_read::ReadHandles;
mod bucket_iam;
mod bucket_update;
mod buckets;
pub(crate) mod convert;
mod customer_key;
mod mask;
mod move_restore;
pub(crate) mod names;
mod object_io;
mod object_update;
mod routing;
pub(crate) mod status;
mod stored;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod update_bucket;
mod update_copy;
mod update_object;

use crate::grpc::generated::google::{iam::v1 as iam, storage::v2 as pb};
use crate::{
    auth::{AuthError, Caller, ListenerRole},
    server::State,
};
use routing::{Route, Routes};
use std::{pin::Pin, sync::Arc};
use tonic::{Request, Response, Status, Streaming};

pub(crate) type ResponseStream<T> =
    Pin<Box<dyn tonic::codegen::tokio_stream::Stream<Item = Result<T, Status>> + Send>>;

#[derive(Clone)]
pub(crate) struct Storage {
    pub state: Arc<State>,
    pub max_object_bytes: u64,
    pub compat_profile: crate::config::CompatProfile,
}

pub(crate) fn service(
    state: Arc<State>,
    max_object_bytes: u64,
    compat_profile: crate::config::CompatProfile,
) -> pb::storage_server::StorageServer<Storage> {
    pb::storage_server::StorageServer::new(Storage {
        state,
        max_object_bytes,
        compat_profile,
    })
    .max_decoding_message_size(4 * 1024 * 1024)
    .max_encoding_message_size(MAX_RESPONSE_BYTES)
}

/// The largest response message: one bucket resource, which the JSON API can store above the 4 MiB
/// of a request message. Its CORS and lifecycle rules can each reach the JSON body limit in separate
/// writes, protobuf takes no more than the JSON they came from, and the rest of a resource is small
/// beside them. Lists stay far below this by paging on their size. What a client agrees to receive
/// is its own setting, so one left at a common default of 4 MiB fails such a read on its side.
const MAX_RESPONSE_BYTES: usize = 2 * crate::bounded_body::RESOURCE_BODY_LIMIT + 1024 * 1024;

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
    fn bucket_id<'a>(&self, resource: &'a str) -> Result<&'a str, Status> {
        let id = names::bucket_name(resource)?;
        crate::bucket::name::validate_for_profile(id, self.compat_profile)?;
        Ok(id)
    }

    fn object_name<'a>(
        &self,
        bucket: &'a str,
        object: &'a str,
    ) -> Result<(&'a str, &'a str), Status> {
        let bucket = self.bucket_id(bucket)?;
        names::object(object)?;
        Ok((bucket, object))
    }

    /// Callers validate resource names before authorization.
    async fn access(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        permission: crate::auth::Permission,
    ) -> Result<crate::bucket::BucketAccess, Status> {
        crate::server::authorize_bucket(&self.state, caller, permission, bucket)
            .await
            .map_err(|error| access_status(error, caller))
    }

    pub(crate) fn authenticate<T>(
        &self,
        request: &Request<T>,
    ) -> impl Future<Output = Result<Option<Caller<'_>>, Status>> + Send {
        let headers = request.metadata().clone().into_headers();
        async move {
            // gRPC has no trusted listener, so a call without credentials is always anonymous.
            let caller = self
                .state
                .auth
                .authenticate_request(
                    &headers,
                    None,
                    self.state.clock.as_ref(),
                    ListenerRole::Public,
                )
                .await
                .map_err(auth_error)?;
            Ok(caller)
        }
    }
}

/// Anonymous denials ask for credentials, matching the shared JSON authorization rules.
fn access_status(error: impl Into<Status>, caller: Option<Caller<'_>>) -> Status {
    status::anonymous(error.into(), caller)
}

pub(super) fn auth_error(error: AuthError) -> Status {
    if let AuthError::Unavailable(cause) = error {
        let mut status = Status::unavailable("The credential verifier is unavailable");
        status.set_source(Arc::new(cause));
        return status;
    }
    Status::unauthenticated(
        "gcs-mimic strict authentication requires a registered local Bearer token; supply authorization: Bearer local-admin-token with a matching identity_file",
    )
}

// Keep dispatch and forwarding in sync; a misspelled wire name rejects that RPC as unsupported.
macro_rules! storage_methods {
    ($($wire:ident => $method:ident($request:ty $(,)?) -> $response:ty, route |$r:ident| $routes:expr;)*) => {
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
                    mut request: Request<$request>,
                ) -> Result<Response<$response>, Status> {
                    let caller = self.authenticate(&request).await?;
                    let $r = request.get_ref();
                    let admitted = routing::admit(request.metadata(), $routes)?;
                    // A handler that learns its resources later compares what was admitted.
                    request.extensions_mut().insert(admitted);
                    self.$method(request, caller)
                        .await
                        .map_err(|status| status::anonymous(status, caller))
                }
            )*
        }
    };
}

storage_methods! {
    DeleteBucket => delete_bucket(pb::DeleteBucketRequest) -> (), route |r| Routes::Request(&[Route::Bucket(Some(r.name.as_str()))]);
    GetBucket => get_bucket(pb::GetBucketRequest) -> pb::Bucket, route |r| Routes::Request(&[Route::Bucket(Some(r.name.as_str()))]);
    CreateBucket => create_bucket(pb::CreateBucketRequest) -> pb::Bucket, route |r| Routes::Request(&[Route::Project(r.bucket.as_ref().map_or(r.parent.as_str(), |b| b.project.as_str()))]);
    ListBuckets => list_buckets(pb::ListBucketsRequest) -> pb::ListBucketsResponse, route |r| Routes::Request(&[Route::Project(&r.parent)]);
    LockBucketRetentionPolicy => lock_bucket_retention_policy(
        pb::LockBucketRetentionPolicyRequest,
    ) -> pb::Bucket, route |r| Routes::Request(&[Route::Bucket(Some(r.bucket.as_str()))]);
    GetIamPolicy => get_iam_policy(iam::GetIamPolicyRequest) -> iam::Policy, route |r| Routes::Request(&[Route::Bucket(Some(routing::iam_bucket(&r.resource)))]);
    SetIamPolicy => set_iam_policy(iam::SetIamPolicyRequest) -> iam::Policy, route |r| Routes::Request(&[Route::Bucket(Some(routing::iam_bucket(&r.resource)))]);
    TestIamPermissions => test_iam_permissions(
        iam::TestIamPermissionsRequest,
    ) -> iam::TestIamPermissionsResponse, route |r| Routes::Request(&[Route::Bucket(Some(routing::iam_bucket(&r.resource)))]);
    UpdateBucket => update_bucket(pb::UpdateBucketRequest) -> pb::Bucket, route |r| Routes::Request(&[Route::Bucket(r.bucket.as_ref().map(|b| b.name.as_str()))]);
    ComposeObject => compose_object(pb::ComposeObjectRequest) -> pb::Object, route |r| Routes::Request(&[Route::Bucket(r.destination.as_ref().map(|o| o.bucket.as_str()))]);
    DeleteObject => delete_object(pb::DeleteObjectRequest) -> (), route |r| Routes::Request(&[Route::Bucket(Some(r.bucket.as_str()))]);
    RestoreObject => restore_object(pb::RestoreObjectRequest) -> pb::Object, route |r| Routes::Request(&[Route::Bucket(Some(r.bucket.as_str()))]);
    CancelResumableWrite => cancel_resumable_write(
        pb::CancelResumableWriteRequest,
    ) -> pb::CancelResumableWriteResponse, route |_r| Routes::Upload;
    GetObject => get_object(pb::GetObjectRequest) -> pb::Object, route |r| Routes::Request(&[Route::Bucket(Some(r.bucket.as_str()))]);
    ReadObject => read_object(pb::ReadObjectRequest) -> Self::ReadObjectStream, route |r| Routes::Request(&[Route::Bucket(Some(r.bucket.as_str()))]);
    BidiReadObject => bidi_read_object(
        Streaming<pb::BidiReadObjectRequest>,
    ) -> Self::BidiReadObjectStream, route |_r| Routes::Deferred;
    UpdateObject => update_object(pb::UpdateObjectRequest) -> pb::Object, route |r| Routes::Request(&[Route::Bucket(r.object.as_ref().map(|o| o.bucket.as_str()))]);
    WriteObject => write_object(Streaming<pb::WriteObjectRequest>) -> pb::WriteObjectResponse, route |_r| Routes::Deferred;
    BidiWriteObject => bidi_write_object(
        Streaming<pb::BidiWriteObjectRequest>,
    ) -> Self::BidiWriteObjectStream, route |_r| Routes::Deferred;
    ListObjects => list_objects(pb::ListObjectsRequest) -> pb::ListObjectsResponse, route |r| Routes::Request(&[Route::Bucket(Some(r.parent.as_str()))]);
    RewriteObject => rewrite_object(pb::RewriteObjectRequest) -> pb::RewriteResponse, route |_r| Routes::Deferred;
    StartResumableWrite => start_resumable_write(
        pb::StartResumableWriteRequest,
    ) -> pb::StartResumableWriteResponse, route |r| Routes::Request(&[Route::Bucket(r.write_object_spec.as_ref().and_then(|s| s.resource.as_ref()).map(|o| o.bucket.as_str()))]);
    QueryWriteStatus => query_write_status(
        pb::QueryWriteStatusRequest,
    ) -> pb::QueryWriteStatusResponse, route |_r| Routes::Upload;
    MoveObject => move_object(pb::MoveObjectRequest) -> pb::Object, route |r| Routes::Request(&[Route::Bucket(Some(r.bucket.as_str()))]);
}
