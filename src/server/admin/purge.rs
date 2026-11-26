//! Purging one bucket deletes it with everything stored under its name, past the holds and
//! retention the API enforces, for test helpers that clean up after themselves. The route lives
//! outside the Google namespaces; `DELETE /storage/v1/b/{bucket}` keeps refusing a bucket with
//! contents or protections.

use crate::json::JsonErrorResponse;
use crate::{
    api_error::ApiError,
    json,
    server::{DeadlineWatch, RESOURCE_BODY_LIMIT, RequestBody, RequestCtx, json_response},
    storage::admin::{AdminError, BucketName, InvalidBucketName, PurgeError},
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};

/// The path prefix before the bucket's encoded name.
pub(super) const PREFIX: &str = "/_internal/buckets/";

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    authorized: super::Authorized,
) -> Response<Full<Bytes>> {
    super::retry_soon(
        purge(request, ctx, authorized)
            .await
            .unwrap_or_else(ApiError::into_response),
    )
}

async fn purge(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    authorized: super::Authorized,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let admin = authorized.into_admin();
    if request.method() != Method::DELETE {
        return Err(ApiError::method_not_allowed(
            "Use DELETE /_internal/buckets/{bucket} with an empty body",
            Some("DELETE"),
        ));
    }
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "Bucket purge does not accept query parameters",
        ));
    }
    let bucket = bucket(request.uri().path())?;
    if !crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT)
        .await?
        .is_empty()
    {
        return Err(ApiError::invalid_argument(
            "Bucket purge accepts only an empty body",
        ));
    }
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let ctx = ctx.object(None, deadline);
    // Expiry while the purge waits for the writer connection still confirms it changed nothing.
    let wait_deadline = deadline.min(ctx.config.catalog_wait_timeout().deadline());
    let outcome = ctx
        .await_session_writer(admin.purge_bucket_until(&bucket, wait_deadline))
        .await
        .map_err(|error| failure(&error))?;
    Ok(json_response(&super::encode(&outcome)?))
}

/// The bucket the path names: its segment decoded as the JSON API decodes one, then held to the
/// native bucket-name rules that every local administration route uses.
fn bucket(path: &str) -> Result<BucketName, ApiError> {
    let segment = path.strip_prefix(PREFIX).unwrap_or_default();
    BucketName::try_from(json::decode_bucket_segment(segment)?)
        .map_err(|InvalidBucketName| ApiError::invalid_parameter("Invalid bucket name"))
}

fn failure(error: &PurgeError) -> ApiError {
    match error {
        PurgeError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "No live or soft-deleted bucket has this name",
        ),
        PurgeError::Importing => ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Purge made no changes because an import is incomplete; complete or repair the import first",
        ),
        PurgeError::WriterBusy => ApiError::service_unavailable(
            "Purge made no changes because a request is writing to an upload session or appendable object of this bucket; retry after it completes",
        ),
        PurgeError::Admin(AdminError::Busy { .. }) => ApiError::service_unavailable(
            "Purge waited longer than catalog_wait_timeout_ms for other catalog work and made no changes; retry after that work completes, or raise catalog_wait_timeout_ms (0 disables it)",
        ),
        // The purge can only say that it changed nothing or that it cannot confirm what it changed.
        PurgeError::Admin(
            AdminError::DiskFull(_)
            | AdminError::Closed
            | AdminError::Precondition
            | AdminError::Hold
            | AdminError::SeedProjectMismatch
            | AdminError::SeedMetadataBudget { .. }
            | AdminError::SourceChanged { .. }
            | AdminError::Corrupt(_)
            | AdminError::Io(_)
            | AdminError::NotFound
            | AdminError::Invalid(_)
            | AdminError::Checksum
            | AdminError::TooLarge
            | AdminError::Forbidden
            | AdminError::BodyAborted(_)
            | AdminError::Backend(_),
        ) => ApiError::service_unavailable(
            "Purge could not confirm completion; inspect local storage and instance state before retrying",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::failure::BackendCause;
    use std::{io, time::Duration};

    #[test]
    fn the_path_names_one_bucket_decoded_as_the_json_api_decodes_it() {
        assert_eq!(
            bucket("/_internal/buckets/test-bucket").unwrap().as_str(),
            "test-bucket"
        );
        assert_eq!(
            bucket("/_internal/buckets/test%2Dbucket").unwrap().as_str(),
            "test-bucket"
        );
        for path in [
            "/_internal/buckets/",
            "/_internal/buckets/Upper",
            "/_internal/buckets/a/b",
            "/_internal/buckets/a%2Fb",
            "/_internal/buckets/%zz",
            "/_internal/buckets/googbucket",
        ] {
            let error = bucket(path).unwrap_err();
            assert_eq!(
                (error.status, error.reason),
                (StatusCode::BAD_REQUEST, "invalidParameter"),
                "{path}"
            );
        }
        let long = format!("/_internal/buckets/{}", "a".repeat(3 * 222 + 1));
        assert_eq!(bucket(&long).unwrap_err().status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn every_purge_failure_keeps_its_response() {
        let unconfirmed = (
            StatusCode::SERVICE_UNAVAILABLE,
            "backendError",
            "Purge could not confirm completion; inspect local storage and instance state before retrying",
        );
        for (error, expected) in [
            (
                PurgeError::NotFound,
                (
                    StatusCode::NOT_FOUND,
                    "notFound",
                    "No live or soft-deleted bucket has this name",
                ),
            ),
            (
                PurgeError::Importing,
                (
                    StatusCode::CONFLICT,
                    "conflict",
                    "Purge made no changes because an import is incomplete; complete or repair the import first",
                ),
            ),
            (
                PurgeError::WriterBusy,
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Purge made no changes because a request is writing to an upload session or appendable object of this bucket; retry after it completes",
                ),
            ),
            (
                PurgeError::Admin(AdminError::Busy {
                    retry_after: Duration::from_secs(1),
                }),
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Purge waited longer than catalog_wait_timeout_ms for other catalog work and made no changes; retry after that work completes, or raise catalog_wait_timeout_ms (0 disables it)",
                ),
            ),
            (
                PurgeError::Admin(AdminError::DiskFull(io::ErrorKind::StorageFull.into())),
                unconfirmed,
            ),
            (PurgeError::Admin(AdminError::Closed), unconfirmed),
            (
                PurgeError::Admin(AdminError::Backend(BackendCause::new("pool"))),
                unconfirmed,
            ),
        ] {
            let label = format!("{error:?}");
            let answer = failure(&error);
            assert_eq!(
                (answer.status, answer.reason, answer.message.as_str()),
                expected,
                "{label}"
            );
        }
    }
}
