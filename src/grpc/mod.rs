//! Storage transports share domain state, never JSON wire representations.

mod conditions;
mod convert;
#[cfg(test)]
pub(crate) mod test_support;
mod upload;
pub(crate) mod v1;
pub(crate) mod v2;
mod validate;

// Generated upstream code retains its own field names and documentation.
#[expect(
    clippy::allow_attributes,
    reason = "which lints fire in generated code varies by target and cfg(test), so an \
              expectation on them could go unfulfilled"
)]
#[allow(
    clippy::all,
    dead_code,
    unreachable_pub,
    rustdoc::broken_intra_doc_links,
    reason = "generated upstream code keeps its own style and documentation links"
)]
pub(crate) mod generated {
    include!("generated/mod.rs");
}

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
