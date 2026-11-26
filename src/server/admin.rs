//! Opt-in local management never confers Google API permissions.

use super::{RESOURCE_BODY_LIMIT, RequestBody, RequestCtx, ResponseBody, boxed, empty_response};
use crate::json::JsonErrorResponse;
use crate::{auth, config::EndpointError, json::ApiError};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};
use serde::Deserialize;

mod fixtures;
mod guard;
mod purge;
mod reseed;
mod reset;
mod seed_watch;

/// Every management route shares the same browser and host checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AdminRoute {
    Config,
    Objects,
    Reset,
    DeleteAll,
    Reseed,
    SeedStatus,
    SeedWait,
    Lifecycle,
    /// `/_internal/buckets/{bucket}`; the handler reads the name from the path.
    Bucket,
}

impl AdminRoute {
    pub(super) fn parse(path: &str) -> Option<Self> {
        Some(match path {
            "/_internal/config" => Self::Config,
            "/_internal/objects" => Self::Objects,
            "/_internal/reset" => Self::Reset,
            "/_internal/delete_all" => Self::DeleteAll,
            "/_internal/reseed" => Self::Reseed,
            "/_internal/seed" => Self::SeedStatus,
            "/_internal/seed/wait" => Self::SeedWait,
            "/_internal/lifecycle/run" => Self::Lifecycle,
            path if path.starts_with(purge::PREFIX) => Self::Bucket,
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

/// Tells a client to retry, after one second, an administrative request that answered 503: a
/// reset, a reseed, a purge, or a seed wait that timed out or met a closing server.
fn retry_soon(mut response: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
    if response.status() == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert("retry-after", hyper::header::HeaderValue::from_static("1"));
    }
    response
}

enum LocalRoute {
    Config,
    Objects,
    Reset(reset::Form),
    Reseed,
    SeedStatus,
    SeedWait,
    Lifecycle,
    Bucket,
}

impl From<AdminRoute> for LocalRoute {
    fn from(route: AdminRoute) -> Self {
        match route {
            AdminRoute::Config => Self::Config,
            AdminRoute::Objects => Self::Objects,
            AdminRoute::Reset => Self::Reset(reset::Form::Confirmed),
            AdminRoute::DeleteAll => Self::Reset(reset::Form::SourceAlias),
            AdminRoute::Reseed => Self::Reseed,
            AdminRoute::SeedStatus => Self::SeedStatus,
            AdminRoute::SeedWait => Self::SeedWait,
            AdminRoute::Lifecycle => Self::Lifecycle,
            AdminRoute::Bucket => Self::Bucket,
        }
    }
}

async fn authority(
    request: &Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Authorized, ApiError> {
    let admin = ctx.state.catalog.local_admin().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "Local administration is disabled at startup",
        )
    })?;
    let caller = ctx
        .authenticate(request.headers(), request.uri().query())
        .await?;
    if let Some(caller) = caller {
        let principal = caller.principal()?;
        if ctx.config.admin_principal() != Some(principal.id()) {
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
    let route = LocalRoute::from(route);
    let authorized = match authority(&request, ctx).await {
        Ok(authorized) => authorized,
        Err(error) => return boxed(error.into_response()),
    };
    boxed(match route {
        LocalRoute::Config => update(request, ctx, &authorized)
            .await
            .unwrap_or_else(ApiError::into_response),
        LocalRoute::Objects => fixtures::insert(request, ctx, authorized)
            .await
            .unwrap_or_else(ApiError::into_response),
        LocalRoute::Reset(form) => reset::respond(request, ctx, authorized, form).await,
        LocalRoute::Reseed => reseed::respond(request, ctx, authorized).await,
        LocalRoute::SeedStatus => {
            seed_watch::status(&request, ctx).unwrap_or_else(ApiError::into_response)
        }
        LocalRoute::SeedWait => seed_watch::respond_wait(request, ctx).await,
        LocalRoute::Lifecycle => run_lifecycle(request, ctx, authorized)
            .await
            .unwrap_or_else(ApiError::into_response),
        LocalRoute::Bucket => purge::respond(request, ctx, authorized).await,
    })
}

async fn run_lifecycle(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    _authorized: Authorized,
) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>, ApiError> {
    if request.method() != Method::POST {
        return Err(ApiError::method_not_allowed(
            "Use POST /_internal/lifecycle/run with an empty body",
            Some("POST"),
        ));
    }
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "Lifecycle execution does not accept query parameters or time travel",
        ));
    }
    if !crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT)
        .await?
        .is_empty()
    {
        return Err(ApiError::invalid_argument(
            "Lifecycle execution accepts only an empty body",
        ));
    }
    let outcome = ctx.state.catalog.run_lifecycle_now().await?;
    Ok(super::json_response(&encode(&outcome)?))
}

/// Renders a management result. The outcome types hold only integers, booleans and fixed
/// strings, so this cannot fail today; if a future field makes it fail, the server is at fault,
/// and the client gets a server-side failure instead of a rejected request.
fn encode(value: &impl serde::Serialize) -> Result<serde_json::Value, ApiError> {
    serde_json::to_value(value).map_err(|error| {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Error,
            crate::diagnostics::Event::AdminResultUnencodable,
            serde_json::json!({"reason": error.to_string()}),
        );
        ApiError::service_unavailable(
            "Cannot encode the management result; inspect server diagnostics",
        )
    })
}

async fn update(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    _authorized: &Authorized,
) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>, ApiError> {
    if request.method() != Method::PUT {
        return Ok(
            ApiError::method_not_allowed("Use PUT /_internal/config", Some("PUT")).into_response(),
        );
    }
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "Local configuration does not accept query parameters",
        ));
    }
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let update: Update = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::invalid_argument(
            "Send one JSON object with string externalUrl and publicHost fields",
        )
    })?;
    ctx.config
        .runtime_endpoints()
        .update(
            ctx.config,
            &ctx.state.api_base_url,
            update.external_url.as_deref(),
            update.public_host.as_deref(),
        )
        .map_err(ApiError::from)?;
    Ok(empty_response(StatusCode::OK))
}

impl From<EndpointError> for ApiError {
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
    use crate::storage::{admin::AdminError, failure::BackendCause};
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
        let config = std::sync::Arc::new(
            crate::config::ConfigDraft {
                admin_enabled: true,
                admin_principal: Some("manager".into()),
                ..Default::default()
            }
            .build(),
        );
        let endpoints = config
            .runtime_endpoints()
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
    }

    impl Route {
        const ALL: [Self; 3] = [Self::Fixtures, Self::Reset, Self::Reseed];

        fn answer(self, error: AdminError) -> ApiError {
            match self {
                Self::Fixtures => fixtures::failure(error),
                Self::Reset => reset::failure(&error),
                Self::Reseed => reseed::admin_failure(&error, reseed::Operation::Reseed),
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
                fields: vec![crate::fsutil::IdentityField::Size],
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
    fn answers(error: &AdminError) -> [Answer; 3] {
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
                    "Reseed made no changes because import state is active",
                ),
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
            ],
            AdminError::SeedProjectMismatch => [FIXTURE_FAILED, RESET_UNCONFIRMED, RESEED_INVALID],
            AdminError::SeedMetadataBudget { .. } => [
                FIXTURE_FAILED,
                RESET_UNCONFIRMED,
                (
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    "Reseed made no changes because fixture metadata exceeds 64 MiB; raise max_seed_entries or reduce the tree",
                ),
            ],
            AdminError::Corrupt(_) | AdminError::Io(_) => {
                [FIXTURE_FAILED, RESET_UNCONFIRMED, RESEED_RETRY]
            }
            AdminError::NotFound => [
                (
                    StatusCode::NOT_FOUND,
                    "notFound",
                    "Create the fixture bucket before publishing objects",
                ),
                RESET_UNCONFIRMED,
                RESEED_RETRY,
            ],
            AdminError::Invalid(_) => [
                (StatusCode::BAD_REQUEST, "invalidArgument", "bad fixture"),
                RESET_UNCONFIRMED,
                RESEED_INVALID,
            ],
            AdminError::Checksum => [
                (
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    "Fixture checksum does not match the body",
                ),
                RESET_UNCONFIRMED,
                RESEED_RETRY,
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
            ],
            AdminError::Forbidden => [
                FORBIDDEN,
                RESET_UNCONFIRMED,
                (
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Reseed made no changes because a fixture write was not authorized",
                ),
            ],
            AdminError::DiskFull(_)
            | AdminError::Closed
            | AdminError::SourceChanged { .. }
            | AdminError::BodyAborted(_)
            | AdminError::Backend(_) => [FIXTURE_FAILED, RESET_UNCONFIRMED, RESEED_RETRY],
        }
    }

    #[test]
    fn an_unencodable_management_result_answers_as_a_server_failure() {
        struct Unencodable;
        impl serde::Serialize for Unencodable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("serializer internals"))
            }
        }
        let error = encode(&Unencodable).unwrap_err();
        assert_eq!(
            (error.status(), error.reason(), error.message()),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Cannot encode the management result; inspect server diagnostics",
            )
        );
    }

    #[test]
    fn every_admin_failure_keeps_each_routes_response() {
        let mut mismatches = Vec::new();
        for (column, route) in Route::ALL.into_iter().enumerate() {
            for error in admin_errors() {
                let expected = answers(&error)[column];
                let label = format!("{route:?} {error:?}");
                let answer = route.answer(error);
                if (answer.status(), answer.reason(), answer.message()) != expected {
                    mismatches.push(format!(
                        "{label}: {} {} {:?}",
                        answer.status(),
                        answer.reason(),
                        answer.message()
                    ));
                }
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }
}
