//! Request context that the connection hands to every API handler.
//!
//! Handlers receive these structs instead of long parameter lists, so adding shared
//! request state changes one definition rather than every signature on the call path.

use super::{ObjectActivity, State, authorize_bucket, await_upload_catalog};
use crate::{
    auth::Caller, bucket::BucketAccess, config::Config, json::JsonError, object::ObjectError,
};
use std::{future::Future, time::Duration};
use tokio::{sync::watch, time::Instant};

/// What every handler needs from the connection that dispatched its request.
#[derive(Clone, Copy)]
pub(crate) struct RequestCtx<'a> {
    pub(crate) state: &'a State,
    pub(crate) config: &'a Config,
    /// Reports body and catalog progress to the connection's deadline watchdog.
    pub(crate) activity: &'a watch::Sender<ObjectActivity>,
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
    /// Starts an object request deadline of `object_request_timeout_ms` from now.
    pub(crate) fn start_object_deadline(self, watch: DeadlineWatch) -> Instant {
        let deadline =
            Instant::now() + Duration::from_millis(self.config.object_request_timeout_ms);
        self.activity.send_replace(ObjectActivity {
            awaiting_catalog: false,
            deadline: (watch == DeadlineWatch::Enforced).then_some(deadline),
        });
        deadline
    }

    /// Stops watching the object request deadline once the handler has answered.
    pub(crate) fn finish_object_request(self) {
        self.activity.send_replace(ObjectActivity::default());
    }

    /// The context of an authenticated object request with a running deadline.
    pub(crate) fn object(self, caller: Option<Caller<'a>>, deadline: Instant) -> ObjectCtx<'a> {
        ObjectCtx {
            state: self.state,
            config: self.config,
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
    pub(crate) activity: &'a watch::Sender<ObjectActivity>,
    /// `None` in permissive mode, where requests are not authorized.
    pub(crate) caller: Option<Caller<'a>>,
    /// The object request deadline started when the route was selected.
    pub(crate) deadline: Instant,
}

impl ObjectCtx<'_> {
    pub(crate) fn catalog_timeout(&self) -> Duration {
        Duration::from_millis(self.config.catalog_wait_timeout_ms)
    }

    /// Awaits a catalog operation within the catalog wait budget and the request deadline.
    pub(crate) async fn await_catalog<T, E: From<ObjectError>>(
        &self,
        operation: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        await_upload_catalog(
            operation,
            self.catalog_timeout(),
            self.deadline,
            self.activity,
        )
        .await
    }

    /// Authorizes `permission` on `bucket` for this request's caller.
    pub(crate) async fn authorize(
        &self,
        permission: &str,
        bucket: &str,
    ) -> Result<BucketAccess, JsonError> {
        authorize_bucket(self.state, self.caller, permission, bucket).await
    }
}
