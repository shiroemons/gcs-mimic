//! Request context that the connection hands to every API handler.
//!
//! Handlers receive these structs instead of long parameter lists, so adding shared
//! request state changes one definition rather than every signature on the call path.

use crate::{
    auth::{AuthError, Caller, ListenerRole},
    bucket::{BucketAccess, CatalogError},
    config::{Config, RequestEndpoints},
    object::ObjectError,
    service::{
        access::bucket_access,
        activity::{ObjectActivity, await_upload_catalog},
        state::State,
    },
    time_limit::TimeLimit,
};
use hyper::HeaderMap;
use std::{future::Future, sync::Arc};
use tokio::{sync::watch, time::Instant};

/// What every handler needs from the connection that dispatched its request.
#[derive(Clone, Copy)]
pub(crate) struct RequestCtx<'a> {
    pub(crate) uri: &'a crate::percent::RequestUri,
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
    ) -> impl Future<Output = Result<Option<Caller<'a>>, AuthError>> + Send + use<'a, 'r> {
        let RequestCtx {
            state, listener, ..
        } = self;
        async move {
            state
                .auth
                .authenticate_request(headers, query, state.clock.as_ref(), listener)
                .await
        }
    }

    /// Starts an object request deadline of `object_request_timeout_ms` from now.
    pub(crate) fn start_object_deadline(self, watch: DeadlineWatch) -> Instant {
        let deadline = self.config.object_request_timeout().deadline();
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
            uri: self.uri,
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
    pub(crate) uri: &'a crate::percent::RequestUri,
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
            self.config.catalog_wait_timeout(),
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
            self.config.catalog_wait_timeout().doubled(),
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
    ) -> Result<BucketAccess, CatalogError> {
        bucket_access(self.state, self.caller, permission, bucket).await
    }
}
