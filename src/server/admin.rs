//! Opt-in local management never confers Google API permissions; sync retains its ownership authority.

use super::{BUCKET_BODY_LIMIT, RequestBody, RequestCtx, ResponseBody, boxed, empty_response};
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
    })
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
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
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
        let state = super::super::State::for_test(catalog, auth::Auth::strict(registry));
        let config = crate::config::Config {
            admin_enabled: true,
            admin_principal: Some("manager".into()),
            ..Default::default()
        };
        let endpoints = config
            .runtime_endpoints
            .snapshot(&config, "http://localhost")
            .unwrap();
        let (activity, _) = tokio::sync::watch::channel(super::super::ObjectActivity::default());
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
        };
        for (token, status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some("ordinary"), StatusCode::FORBIDDEN),
            (Some("manager"), StatusCode::OK),
        ] {
            let mut request = Request::builder()
                .method(Method::PUT)
                .uri("/_internal/config");
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let request = request
                .body(RequestBody::Buffered(http_body_util::Full::new(
                    hyper::body::Bytes::from_static(b"{}"),
                )))
                .unwrap();
            assert_eq!(
                respond(AdminRoute::Config, request, ctx).await.status(),
                status
            );
        }
        state.catalog.close().await.unwrap();
    }
}
