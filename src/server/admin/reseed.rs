//! Replays only the startup-selected read-only fixtures, never a request-supplied path.

use crate::{
    json::{self, JsonError},
    seed::SeedError,
    server::{DeadlineWatch, RESOURCE_BODY_LIMIT, RequestBody, RequestCtx, json_response},
    storage::admin::AdminError,
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    authorized: super::Authorized,
) -> Response<Full<Bytes>> {
    let mut response = reseed(request, ctx, authorized)
        .await
        .unwrap_or_else(JsonError::into_response);
    if response.status() == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert("retry-after", hyper::header::HeaderValue::from_static("1"));
    }
    response
}

async fn reseed(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    authorized: super::Authorized,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let admin = authorized.into_admin();
    if !matches!(*request.method(), Method::POST | Method::PUT) {
        return Err(JsonError::method_not_allowed(
            "Use POST or PUT /_internal/reseed with an empty body",
            Some("POST, PUT"),
        ));
    }
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Reseed does not accept query parameters",
        ));
    }
    if ctx.config.seed_dir.is_none() {
        return Err(JsonError::invalid_argument(
            "Configure seed_dir at startup before reseeding",
        ));
    }
    if ctx.config.sync_dir.is_some() {
        return Err(JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Stop synchronization and restart without sync_dir before reseeding; external files are never reset",
        ));
    }
    let bytes = json::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    if !bytes.is_empty() {
        return Err(JsonError::invalid_argument(
            "Reseed accepts only an empty body; the source is configured at startup",
        ));
    }
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let clock = ctx.state.clock.as_ref();
    let config = ctx.config;
    let ctx = ctx.object(None, deadline);
    // Expiry before exclusive ownership can still confirm that reseed made no changes.
    let wait_limit = deadline.min(config.catalog_wait_timeout.deadline());
    let outcome = ctx
        .await_catalog(async {
            crate::seed::reseed(&admin, config, clock, wait_limit)
                .await
                .map_err(|error| failure(&error))
        })
        .await?;
    Ok(json_response(&serde_json::to_value(outcome).map_err(
        |_| JsonError::invalid_argument("Cannot encode reseed result"),
    )?))
}

fn failure(error: &SeedError) -> JsonError {
    match error {
        SeedError::Admin(AdminError::Precondition) => JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Reseed made no changes because import or synchronization state is active",
        ),
        SeedError::Admin(AdminError::Hold) => JsonError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Reseed made no changes because an object is protected by a hold or retention",
        ),
        SeedError::Admin(AdminError::Invalid(_) | AdminError::SeedProjectMismatch) => {
            JsonError::invalid_argument("Invalid fixture state; reseed made no changes")
        }
        SeedError::Admin(AdminError::Busy { .. }) => JsonError::service_unavailable(
            "Reseed made no changes because catalog work, an unpublished body or another publication is active; retry after it completes",
        ),
        _ => JsonError::service_unavailable(
            "Reseed failed; inspect local fixture/storage diagnostics and retry",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_reseed_failures_keep_the_existing_http_contract() {
        for (cause, status, reason, message) in [
            (
                AdminError::Precondition,
                StatusCode::CONFLICT,
                "conflict",
                "Reseed made no changes because import or synchronization state is active",
            ),
            (
                AdminError::Hold,
                StatusCode::FORBIDDEN,
                "forbidden",
                "Reseed made no changes because an object is protected by a hold or retention",
            ),
            (
                AdminError::Invalid("invalid".into()),
                StatusCode::BAD_REQUEST,
                "invalidArgument",
                "Invalid fixture state; reseed made no changes",
            ),
            (
                AdminError::SeedProjectMismatch,
                StatusCode::BAD_REQUEST,
                "invalidArgument",
                "Invalid fixture state; reseed made no changes",
            ),
            (
                AdminError::Closed,
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Reseed failed; inspect local fixture/storage diagnostics and retry",
            ),
        ] {
            let error = failure(&SeedError::Admin(cause));
            assert_eq!(
                (error.status, error.reason, error.message.as_str()),
                (status, reason, message)
            );
        }
        let error = failure(&SeedError::Io(std::io::ErrorKind::PermissionDenied.into()));
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn reseed_keeps_project_mismatch_typed_until_http_classification() {
        use crate::{
            clock::SystemClock,
            config::Config,
            storage::{
                Catalog,
                admin::{BucketDefinition, BucketName, Origin},
            },
            test_support::TestDir,
        };
        let data = TestDir::new();
        let source = TestDir::new();
        std::fs::create_dir(source.0.join("test-bucket")).unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let admin = catalog.admin(Origin::Seed);
        admin
            .ensure_bucket(
                BucketDefinition::default_for(
                    &BucketName::new("test-bucket").unwrap(),
                    "other-project",
                    &SystemClock,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let config = Config {
            data_dir: data.0.clone(),
            seed_dir: Some(source.0.clone()),
            ..Default::default()
        };
        let error = crate::seed::reseed(
            &admin,
            &config,
            &SystemClock,
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            SeedError::Admin(AdminError::SeedProjectMismatch)
        ));
        assert_eq!(failure(&error).status, StatusCode::BAD_REQUEST);
        drop(admin);
        catalog.close().await.unwrap();
    }
}
