//! Storage transports share domain state, never JSON wire representations.

mod conditions;
mod convert;
#[cfg(test)]
pub(crate) mod test_support;
mod upload;
pub(crate) mod v1;
pub(crate) mod v2;
mod validate;

// The generated protobuf crate belongs to this adapter. The server names it here, and the
// layer rules count a direct use of the crate as naming `grpc`, so they see which modules use
// the wire types.
pub(crate) use grpc_generated as generated;

/// What an object list page aims to take. gRPC clients commonly accept 4 MiB in one message unless
/// told otherwise, and a page's size is an estimate, so this leaves room below that.
pub(crate) const LIST_PAGE_BYTES: usize = 2 * 1024 * 1024;

use crate::{config::Config, service::state::State};
use std::{convert::Infallible, sync::Arc};
use tonic::codegen::Service;

/// The Storage services a gRPC connection serves, chosen per call by its path. The gRPC
/// listener in `server::grpc_listener` drives the connection and hands each call to `route`.
#[derive(Clone)]
pub(crate) struct StorageRouter {
    legacy: v1::pb::storage_server::StorageServer<v1::Storage>,
    service: generated::google::storage::v2::storage_server::StorageServer<v2::Storage>,
}

impl StorageRouter {
    pub(crate) fn new(config: &Config, state: Arc<State>) -> Self {
        Self {
            legacy: v1::service(
                state.clone(),
                config.max_object_bytes(),
                config.compat_profile(),
            ),
            service: v2::service(state, config.max_object_bytes(), config.compat_profile()),
        }
    }

    /// Serves legacy v1 calls by their path prefix and everything else as Storage v2.
    pub(crate) async fn route(
        mut self,
        request: hyper::Request<tonic::body::Body>,
    ) -> Result<hyper::Response<tonic::body::Body>, Infallible> {
        if request.uri().path().starts_with(v1::SERVICE_PREFIX) {
            if !v1::known_method(request.uri().path()) {
                return Ok(v2::status::unsupported(
                    "legacy v1 method outside the reference inventory",
                )
                .into_http());
            }
            return self.legacy.call(request).await;
        }
        v2::dispatch(self.service, request).await
    }
}

#[cfg(test)]
mod tests;
