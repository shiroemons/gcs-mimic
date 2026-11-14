//! Opt-in local management never confers Google API permissions; sync retains its ownership authority.

use super::{RESOURCE_BODY_LIMIT, RequestBody, RequestCtx, ResponseBody, boxed, empty_response};
use crate::{
    auth,
    config::EndpointError,
    json::{self, JsonError},
};
use hyper::{Method, Request, StatusCode};
use serde::Deserialize;

mod fixtures;
mod guard;
mod reseed;
mod reset;
mod sync;

/// Every management route shares the same browser and host checks, including sync controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AdminRoute {
    Config,
    Objects,
    Reset,
    DeleteAll,
    Reseed,
    Lifecycle,
    Sync,
}

impl AdminRoute {
    pub(super) fn parse(path: &str) -> Option<Self> {
        Some(match path {
            "/_internal/config" => Self::Config,
            "/_internal/objects" => Self::Objects,
            "/_internal/reset" => Self::Reset,
            "/_internal/delete_all" => Self::DeleteAll,
            "/_internal/reseed" => Self::Reseed,
            "/_internal/lifecycle/run" => Self::Lifecycle,
            "/_internal/sync" => Self::Sync,
            path if path.starts_with("/_internal/sync/") => Self::Sync,
            _ => return None,
        })
    }
}

/// Proof that startup opt-in and instance administrator authentication both passed.
pub(super) struct Authorized(crate::storage::admin::Admin);

impl Authorized {
    fn into_admin(self) -> crate::storage::admin::Admin {
        self.0
    }
}

enum LocalRoute {
    Config,
    Objects,
    Reset(reset::Form),
    Reseed,
    Lifecycle,
}

struct SyncRoute;

impl TryFrom<AdminRoute> for LocalRoute {
    type Error = SyncRoute;
    fn try_from(route: AdminRoute) -> Result<Self, SyncRoute> {
        Ok(match route {
            AdminRoute::Config => Self::Config,
            AdminRoute::Objects => Self::Objects,
            AdminRoute::Reset => Self::Reset(reset::Form::Confirmed),
            AdminRoute::DeleteAll => Self::Reset(reset::Form::SourceAlias),
            AdminRoute::Reseed => Self::Reseed,
            AdminRoute::Lifecycle => Self::Lifecycle,
            AdminRoute::Sync => return Err(SyncRoute),
        })
    }
}

async fn authority(
    request: &Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Authorized, JsonError> {
    let admin = ctx.state.catalog.local_admin().ok_or_else(|| {
        JsonError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "Local administration is disabled at startup",
        )
    })?;
    if let Some(caller) = ctx
        .state
        .auth
        .authenticate_request(
            request.headers(),
            request.uri().query(),
            ctx.state.clock.as_ref(),
            ctx.listener,
        )
        .await?
    {
        let principal = caller.principal()?;
        if ctx.config.admin_principal.as_deref() != Some(principal.id()) {
            return Err(auth::AuthFailure::Forbidden.into());
        }
    }
    Ok(Authorized(admin))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Update {
    external_url: Option<String>,
    public_host: Option<String>,
}

pub(super) async fn respond(
    route: AdminRoute,
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> hyper::Response<ResponseBody> {
    if let Err(error) = guard::browser_or_misdirected(&request, ctx) {
        return boxed(error.into_response());
    }
    let route = match LocalRoute::try_from(route) {
        Ok(route) => route,
        Err(SyncRoute) => return boxed(sync::respond(request, ctx).await),
    };
    let authorized = match authority(&request, ctx).await {
        Ok(authorized) => authorized,
        Err(error) => return boxed(error.into_response()),
    };
    boxed(match route {
        LocalRoute::Config => update(request, ctx, &authorized)
            .await
            .unwrap_or_else(JsonError::into_response),
        LocalRoute::Objects => fixtures::insert(request, ctx, authorized)
            .await
            .unwrap_or_else(JsonError::into_response),
        LocalRoute::Reset(form) => reset::respond(request, ctx, authorized, form).await,
        LocalRoute::Reseed => reseed::respond(request, ctx, authorized).await,
        LocalRoute::Lifecycle => run_lifecycle(request, ctx, authorized)
            .await
            .unwrap_or_else(JsonError::into_response),
    })
}

async fn run_lifecycle(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    _authorized: Authorized,
) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>, JsonError> {
    if request.method() != Method::POST {
        return Err(JsonError::method_not_allowed(
            "Use POST /_internal/lifecycle/run with an empty body",
            Some("POST"),
        ));
    }
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Lifecycle execution does not accept query parameters or time travel",
        ));
    }
    if !json::read_body(request.into_body(), RESOURCE_BODY_LIMIT)
        .await?
        .is_empty()
    {
        return Err(JsonError::invalid_argument(
            "Lifecycle execution accepts only an empty body",
        ));
    }
    let outcome = ctx.state.catalog.run_lifecycle_now().await?;
    Ok(super::json_response(
        &serde_json::to_value(outcome).map_err(|error| {
            JsonError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                error.to_string(),
            )
        })?,
    ))
}

async fn update(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    _authorized: &Authorized,
) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>, JsonError> {
    if request.method() != Method::PUT {
        return Ok(
            JsonError::method_not_allowed("Use PUT /_internal/config", Some("PUT")).into_response(),
        );
    }
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Local configuration does not accept query parameters",
        ));
    }
    let bytes = json::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let update: Update = serde_json::from_slice(&bytes).map_err(|_| {
        JsonError::invalid_argument(
            "Send one JSON object with string externalUrl and publicHost fields",
        )
    })?;
    ctx.config
        .runtime_endpoints
        .update(
            ctx.config,
            &ctx.state.api_base_url,
            update.external_url.as_deref(),
            update.public_host.as_deref(),
        )
        .map_err(JsonError::from)?;
    Ok(empty_response(StatusCode::OK))
}

impl From<EndpointError> for JsonError {
    fn from(error: EndpointError) -> Self {
        let (status, reason) = if matches!(error, EndpointError::Poisoned) {
            (StatusCode::SERVICE_UNAVAILABLE, "backendError")
        } else {
            (StatusCode::BAD_REQUEST, "invalidArgument")
        };
        Self::new(status, reason, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        admin::AdminError,
        failure::BackendCause,
        sync_state::{conflicts, root::RootError},
    };
    use std::{io, time::Duration};

    #[tokio::test]
    async fn config_uses_the_instance_principal_instead_of_project_roles() {
        let dir = crate::test_support::TestDir::new();
        let clock = std::sync::Arc::new(crate::clock::SystemClock);
        let catalog = crate::storage::Catalog::open_for_server(
            &dir.0,
            clock,
            crate::storage::admin::LocalAdmin::Enabled,
        )
        .await
        .unwrap();
        let registry = auth::Registry::parse(br#"{"identities":[{"id":"manager","tokens":["manager"]},{"id":"ordinary","tokens":["ordinary"],"project_roles":{"local":["roles/storage.admin"]}}]}"#).unwrap();
        let mut auth = auth::Auth::strict(registry);
        auth.trust(&crate::config::TrustedCaller::Named("manager".into()));
        assert_eq!(auth.trusted_principal(), Some("manager"));
        let state = std::sync::Arc::new(super::super::State::for_test(catalog, auth));
        let config = crate::config::Config {
            admin_enabled: true,
            admin_principal: Some("manager".into()),
            ..Default::default()
        };
        let endpoints = config
            .runtime_endpoints
            .snapshot(&config, "http://localhost")
            .unwrap()
            .without_request();
        let (activity, _) = tokio::sync::watch::channel(super::super::ObjectActivity::default());
        let public = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
            listener: auth::ListenerRole::Public,
        };
        let trusted = RequestCtx {
            listener: auth::ListenerRole::Trusted,
            ..public
        };
        // The trusted principal names the administrator here, so only the trusted listener
        // admits a request without a token; a presented token still decides on both.
        for (ctx, token, status) in [
            (public, None, StatusCode::UNAUTHORIZED),
            (public, Some("ordinary"), StatusCode::FORBIDDEN),
            (public, Some("manager"), StatusCode::OK),
            (trusted, None, StatusCode::OK),
            (trusted, Some("ordinary"), StatusCode::FORBIDDEN),
        ] {
            for (route, path, method, body) in [
                (
                    AdminRoute::Config,
                    "/_internal/config",
                    Method::PUT,
                    b"{}".as_slice(),
                ),
                (
                    AdminRoute::Lifecycle,
                    "/_internal/lifecycle/run",
                    Method::POST,
                    b"".as_slice(),
                ),
            ] {
                let mut request = Request::builder().method(method).uri(path);
                if let Some(token) = token {
                    request = request.header("authorization", format!("Bearer {token}"));
                }
                let request = request
                    .body(RequestBody::Buffered(http_body_util::Full::new(
                        hyper::body::Bytes::from_static(body),
                    )))
                    .unwrap();
                assert_eq!(respond(route, request, ctx).await.status(), status);
            }
        }
        state.catalog.close().await.unwrap();
    }

    /// A management route and the conversion it applies to a failed catalog operation.
    #[derive(Clone, Copy, Debug)]
    enum Route {
        Fixtures,
        Reset,
        Reseed,
        SyncRoot,
        SyncControl,
        Resolve,
    }

    impl Route {
        const ALL: [Self; 6] = [
            Self::Fixtures,
            Self::Reset,
            Self::Reseed,
            Self::SyncRoot,
            Self::SyncControl,
            Self::Resolve,
        ];

        fn answer(self, error: AdminError) -> JsonError {
            match self {
                Self::Fixtures => fixtures::failure(error),
                Self::Reset => reset::failure(&error),
                Self::Reseed => reseed::admin_failure(&error),
                Self::SyncRoot => sync::root_error(&RootError::Admin(error)),
                // A queued control reports the failure inside an I/O error.
                Self::SyncControl => sync::control_error(&io::Error::other(error)),
                Self::Resolve => sync::conflicts::error(conflicts::Error::Admin(error)),
            }
        }
    }

    type Answer = (StatusCode, &'static str, &'static str);

    const FIXTURE_FAILED: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Fixture publication failed; inspect local storage and retry",
    );
    const RESET_UNCONFIRMED: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Reset could not confirm completion; inspect local storage and instance state before retrying",
    );
    const RESEED_RETRY: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Reseed failed; inspect local fixture/storage diagnostics and retry",
    );
    const RESEED_INVALID: Answer = (
        StatusCode::BAD_REQUEST,
        "invalid",
        "Invalid fixture state; reseed made no changes",
    );
    const SYNC_BUSY: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Synchronization control is busy or closed; retry after the active operation finishes",
    );
    const SYNC_STALE: Answer = (
        StatusCode::CONFLICT,
        "conflict",
        "Synchronization revision changed; rescan, fetch status and inspect the new revision before approval",
    );
    const SYNC_DISK_FULL: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "disk_full",
        "Free disk space without deleting synchronization data, then retry",
    );
    const SYNC_IO_FAILURE: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "io_failure",
        "Restore storage access, then retry",
    );
    const SYNC_CORRUPT: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Object storage is unavailable: stored data is corrupt; restore a valid data directory",
    );
    const RESOLVE_IO_FAILURE: Answer = (
        StatusCode::SERVICE_UNAVAILABLE,
        "io_failure",
        "Preserve retained bytes, restore storage access and retry the same operation ID",
    );
    const FORBIDDEN: Answer = (StatusCode::FORBIDDEN, "forbidden", "Forbidden");

    /// One value of every variant, in declaration order.
    fn admin_errors() -> [AdminError; 17] {
        [
            AdminError::Busy {
                retry_after: Duration::from_secs(1),
            },
            AdminError::DiskFull(io::ErrorKind::StorageFull.into()),
            AdminError::Closed,
            AdminError::Precondition,
            AdminError::Hold,
            AdminError::SeedProjectMismatch,
            AdminError::SeedMetadataBudget { mib: 64 },
            AdminError::SourceChanged {
                fields: vec!["size".into()],
            },
            AdminError::Corrupt(BackendCause::new("row")),
            AdminError::Io(io::ErrorKind::Other.into()),
            AdminError::NotFound,
            AdminError::Invalid("bad fixture".into()),
            AdminError::Checksum,
            AdminError::TooLarge,
            AdminError::Forbidden,
            AdminError::BodyAborted(BackendCause::new("aborted")),
            AdminError::Backend(BackendCause::new("pool")),
        ]
    }

    /// Each route's answer in `Route::ALL` order. The match has no wildcard arm, so a new variant
    /// does not compile until every route's answer to it is decided here.
    fn answers(error: &AdminError) -> [Answer; 6] {
        match error {
            AdminError::Busy { .. } => [
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Fixture publication waited longer than catalog_wait_timeout_ms for other catalog work and made no changes; retry after that work completes, or raise catalog_wait_timeout_ms (0 disables it)",
                ),
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Reset made no changes because catalog work or an unpublished body is active; retry after it completes",
                ),
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Reseed made no changes because catalog work, an unpublished body or another publication is active; retry after it completes",
                ),
                SYNC_BUSY,
                SYNC_BUSY,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::DiskFull(_) => [
                FIXTURE_FAILED,
                RESET_UNCONFIRMED,
                RESEED_RETRY,
                SYNC_DISK_FULL,
                SYNC_DISK_FULL,
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "disk_full",
                    "Free disk space without deleting pending synchronization data, then retry the same operation ID",
                ),
            ],
            AdminError::Precondition => [
                (
                    StatusCode::CONFLICT,
                    "conflict",
                    "Fixture generation already exists; choose a new generation",
                ),
                (
                    StatusCode::CONFLICT,
                    "conflict",
                    "Reset made no changes because an import is incomplete; complete or repair the import first",
                ),
                (
                    StatusCode::CONFLICT,
                    "conflict",
                    "Reseed made no changes because import or synchronization state is active",
                ),
                SYNC_STALE,
                SYNC_STALE,
                SYNC_STALE,
            ],
            AdminError::Hold => [
                (
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Existing object is protected by a hold or retention",
                ),
                RESET_UNCONFIRMED,
                (
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Reseed made no changes because an object is protected by a hold or retention",
                ),
                SYNC_BUSY,
                SYNC_STALE,
                SYNC_STALE,
            ],
            AdminError::SeedProjectMismatch => [
                FIXTURE_FAILED,
                RESET_UNCONFIRMED,
                RESEED_INVALID,
                SYNC_BUSY,
                SYNC_BUSY,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::SeedMetadataBudget { .. } => [
                FIXTURE_FAILED,
                RESET_UNCONFIRMED,
                (
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    "Reseed made no changes because fixture metadata exceeds 64 MiB; raise max_seed_entries or reduce the tree",
                ),
                SYNC_BUSY,
                SYNC_BUSY,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::Corrupt(_) => [
                FIXTURE_FAILED,
                RESET_UNCONFIRMED,
                RESEED_RETRY,
                SYNC_CORRUPT,
                SYNC_CORRUPT,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::Io(_) => [
                FIXTURE_FAILED,
                RESET_UNCONFIRMED,
                RESEED_RETRY,
                SYNC_IO_FAILURE,
                SYNC_IO_FAILURE,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::NotFound => [
                (
                    StatusCode::NOT_FOUND,
                    "notFound",
                    "Create the fixture bucket before publishing objects",
                ),
                RESET_UNCONFIRMED,
                RESEED_RETRY,
                SYNC_BUSY,
                SYNC_STALE,
                SYNC_STALE,
            ],
            AdminError::Invalid(_) => [
                (StatusCode::BAD_REQUEST, "invalidArgument", "bad fixture"),
                RESET_UNCONFIRMED,
                RESEED_INVALID,
                SYNC_BUSY,
                SYNC_BUSY,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::Checksum => [
                (
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    "Fixture checksum does not match the body",
                ),
                RESET_UNCONFIRMED,
                RESEED_RETRY,
                SYNC_BUSY,
                SYNC_BUSY,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::TooLarge => [
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "uploadTooLarge",
                    "Object exceeds the upload size limit",
                ),
                RESET_UNCONFIRMED,
                (
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    "Reseed made no changes because a fixture file exceeds max_object_bytes",
                ),
                SYNC_BUSY,
                SYNC_BUSY,
                RESOLVE_IO_FAILURE,
            ],
            AdminError::Forbidden => [
                FORBIDDEN,
                RESET_UNCONFIRMED,
                (
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Reseed made no changes because a fixture write was not authorized",
                ),
                FORBIDDEN,
                FORBIDDEN,
                FORBIDDEN,
            ],
            AdminError::Closed
            | AdminError::SourceChanged { .. }
            | AdminError::BodyAborted(_)
            | AdminError::Backend(_) => [
                FIXTURE_FAILED,
                RESET_UNCONFIRMED,
                RESEED_RETRY,
                SYNC_BUSY,
                SYNC_BUSY,
                RESOLVE_IO_FAILURE,
            ],
        }
    }

    #[test]
    fn every_admin_failure_keeps_each_routes_response() {
        let mut mismatches = Vec::new();
        for (column, route) in Route::ALL.into_iter().enumerate() {
            for error in admin_errors() {
                let expected = answers(&error)[column];
                let label = format!("{route:?} {error:?}");
                let answer = route.answer(error);
                if (answer.status, answer.reason, answer.message.as_str()) != expected {
                    mismatches.push(format!(
                        "{label}: {} {} {:?}",
                        answer.status, answer.reason, answer.message
                    ));
                }
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }
}
