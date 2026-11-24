//! Request context that the connection hands to every API handler.
//!
//! Handlers receive these structs instead of long parameter lists, so adding shared
//! request state changes one definition rather than every signature on the call path.

use super::{ObjectActivity, State, authorize_bucket, await_upload_catalog};
use crate::{
    api_error::ApiError,
    auth::{Caller, ListenerRole, signature::Resource},
    bucket::BucketAccess,
    config::{ApiBaseSource, Config, RequestEndpoints},
    json,
    object::ObjectError,
    time_limit::TimeLimit,
    xml,
};
use hyper::{HeaderMap, Request};
use serde_json::Value;
use std::{future::Future, sync::Arc};
use tokio::{sync::watch, time::Instant};

/// Moves the links of an object resource stored by an earlier request to this request's origin.
/// The stored links name the Host that finished the work, and a retry can arrive through another
/// address: a browser finishes on a published port, a peer asks through the service name. An
/// explicit base left them correct, so the stored response stays exactly as it was.
pub(crate) fn rebase_stored_links(endpoints: &RequestEndpoints, resource: &mut Value) {
    match endpoints.api_base_source() {
        ApiBaseSource::Explicit => {}
        ApiBaseSource::Derived => json::rebase_object_links(resource, endpoints.api_base_url()),
    }
}

/// What every handler needs from the connection that dispatched its request.
#[derive(Clone, Copy)]
pub(crate) struct RequestCtx<'a> {
    /// Shared rather than borrowed, for work that outlives the handler that started it: a batch
    /// runs its parts behind a response head that has already left.
    pub(crate) state: &'a Arc<State>,
    /// Shared like `state`: a batch keeps it for parts that run after the response head left.
    pub(crate) config: &'a Arc<Config>,
    pub(crate) endpoints: &'a RequestEndpoints,
    /// Reports body and catalog progress to the connection's deadline watchdog.
    pub(crate) activity: &'a watch::Sender<ObjectActivity>,
    /// The listener that accepted the connection; batch parts keep their envelope's.
    pub(crate) listener: ListenerRole,
}

/// Whether the connection answers with a timeout once an object request deadline expires.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeadlineWatch {
    /// The connection replies with a timeout when the deadline passes.
    Enforced,
    /// The deadline only bounds the handler's own catalog waits.
    CatalogOnly,
}

impl<'a> RequestCtx<'a> {
    /// Authenticates request credentials as the listener that accepted the connection would.
    /// The caller borrows from the server state, not from the request, so the request can be
    /// consumed once the future resolves.
    pub(crate) fn authenticate<'r>(
        self,
        headers: &'r HeaderMap,
        query: Option<&'r str>,
    ) -> impl Future<Output = Result<Option<Caller<'a>>, ApiError>> + Send + use<'a, 'r> {
        let RequestCtx {
            state, listener, ..
        } = self;
        async move {
            state
                .auth
                .authenticate_request(headers, query, state.clock.as_ref(), listener)
                .await
                .map_err(ApiError::from)
        }
    }

    /// The inputs of an XML authentication of `request` against the signature `resource`.
    pub(crate) fn xml_authentication<'r, B>(
        self,
        request: &'r Request<B>,
        resource: &'r Resource,
    ) -> xml::Authentication<'a, 'r>
    where
        'a: 'r,
    {
        let state: &'a State = self.state;
        xml::Authentication {
            auth: &state.auth,
            catalog: &state.catalog,
            method: request.method(),
            uri: request.uri(),
            headers: request.headers(),
            clock: state.clock.as_ref(),
            resource,
            listener: self.listener,
        }
    }

    /// Starts an object request deadline of `object_request_timeout_ms` from now.
    pub(crate) fn start_object_deadline(self, watch: DeadlineWatch) -> Instant {
        let deadline = self.config.object_request_timeout.deadline();
        let watched = match watch {
            DeadlineWatch::Enforced => Some(deadline),
            DeadlineWatch::CatalogOnly => None,
        };
        self.activity
            .send_modify(|progress| progress.start_object(watched));
        deadline
    }

    /// Stops watching the object request deadline once the handler has answered.
    pub(crate) fn finish_object_request(self) {
        self.activity.send_modify(ObjectActivity::finish_object);
    }

    /// The context of an authenticated object request with a running deadline.
    pub(crate) fn object(self, caller: Option<Caller<'a>>, deadline: Instant) -> ObjectCtx<'a> {
        ObjectCtx {
            state: self.state,
            config: self.config,
            endpoints: self.endpoints,
            activity: self.activity,
            caller,
            deadline,
        }
    }
}

/// The context of an object request after routing and authentication.
#[derive(Clone, Copy)]
pub(crate) struct ObjectCtx<'a> {
    pub(crate) state: &'a State,
    pub(crate) config: &'a Config,
    pub(crate) endpoints: &'a RequestEndpoints,
    pub(crate) activity: &'a watch::Sender<ObjectActivity>,
    /// `None` in permissive mode, where requests are not authorized.
    pub(crate) caller: Option<Caller<'a>>,
    /// The object request deadline started when the route was selected.
    pub(crate) deadline: Instant,
}

impl ObjectCtx<'_> {
    /// Awaits a catalog operation within the catalog wait budget and the request deadline.
    pub(crate) async fn await_catalog<T, E: From<ObjectError>>(
        &self,
        operation: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        await_upload_catalog(
            operation,
            self.config.catalog_wait_timeout,
            self.deadline,
            self.activity,
        )
        .await
    }

    /// Awaits an operation that first waits for a session's writer or for exclusive ownership.
    /// That wait has its own budget and reports that it changed nothing, so this outer limit
    /// leaves a full budget for the catalog work after it instead of expiring first and reporting
    /// a catalog timeout.
    pub(crate) async fn await_session_writer<T, E: From<ObjectError>>(
        &self,
        operation: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        await_upload_catalog(
            operation,
            self.config.catalog_wait_timeout.doubled(),
            self.deadline,
            self.activity,
        )
        .await
    }

    /// Awaits work whose length follows from what the caller asked for, such as a fixture tree of
    /// any size. Each catalog wait inside it has its own budget, so only the request deadline and
    /// a client that disconnects end it.
    pub(crate) async fn await_requested_work<T, E: From<ObjectError>>(
        &self,
        operation: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        await_upload_catalog(operation, TimeLimit::DISABLED, self.deadline, self.activity).await
    }

    /// Authorizes `permission` on `bucket` for this request's caller.
    pub(crate) async fn authorize(
        &self,
        permission: crate::auth::Permission,
        bucket: &str,
    ) -> Result<BucketAccess, ApiError> {
        authorize_bucket(self.state, self.caller, permission, bucket).await
    }
}
