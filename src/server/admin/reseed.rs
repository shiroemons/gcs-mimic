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
    let outcome = ctx
        .await_requested_work(async {
            crate::seed::reseed(&admin, config, clock, deadline)
                .await
                .map_err(|error| failure(&error))
        })
        .await?;
    Ok(json_response(&serde_json::to_value(outcome).map_err(
        |_| JsonError::invalid_argument("Cannot encode reseed result"),
    )?))
}

/// Classifies a reseed failure by whether another attempt can succeed. A tree that breaks a
/// configured limit, or holds content the emulator cannot import, fails the same way every time,
/// so it answers 400 without `Retry-After`. Only work a later attempt may finish answers 503.
/// Messages are fixed: they never echo a fixture path or an on-disk name.
fn failure(error: &SeedError) -> JsonError {
    match error {
        // The label names the fixture entry, which stays in diagnostics only.
        SeedError::Entry { source, .. } | SeedError::Import(source) => failure(source),
        SeedError::Admin(error) => admin_failure(error),
        SeedError::EntryLimit { limit } => JsonError::invalid(format!(
            "Reseed made no changes because the fixture tree has more than {limit} entries; raise max_seed_entries or reduce the tree"
        )),
        SeedError::DepthLimit { limit } => JsonError::invalid(format!(
            "Reseed made no changes because the fixture tree has more than {limit} directory levels below a bucket; raise max_seed_depth or flatten the tree"
        )),
        SeedError::BucketLimit { limit } => JsonError::invalid(format!(
            "Reseed made no changes because the fixture tree has more than {limit} buckets; raise max_buckets_per_batch or reduce the tree"
        )),
        SeedError::BucketName(_) => JsonError::invalid(
            "Reseed made no changes because a top-level fixture directory is not a valid bucket name",
        ),
        SeedError::ObjectName(_) => JsonError::invalid(
            "Reseed made no changes because a fixture file path is not a valid object name",
        ),
        SeedError::NonUtf8Name => {
            JsonError::invalid("Reseed made no changes because a fixture name is not UTF-8")
        }
        SeedError::UnsupportedFile => JsonError::invalid(
            "Reseed made no changes because a fixture entry is neither a regular file nor a directory, such as a symlink",
        ),
        SeedError::FileLimit => JsonError::invalid(
            "Reseed made no changes because a fixture file is not a regular file or exceeds max_object_bytes",
        ),
        SeedError::NotDirectory => JsonError::invalid(
            "Reseed made no changes because seed_dir is not a directory; it must be a directory, not a symlink",
        ),
        SeedError::Overlap => {
            JsonError::invalid("Reseed made no changes because seed_dir and data_dir overlap")
        }
        SeedError::MissingRoot => {
            JsonError::invalid("Configure seed_dir at startup before reseeding")
        }
        SeedError::Io(error) => io_failure(error.kind()),
        // A walk stops early only on shutdown or a panic. A path outside the root is an invariant
        // the walker keeps, so it is a server fault, not the fixture's.
        SeedError::WalkStopped | SeedError::RelativePath(_) => retry(),
    }
}

/// Reading the tree can succeed once a file settles or a resource frees up, but some failures
/// follow from the fixture tree itself and repeat on every attempt.
fn io_failure(kind: std::io::ErrorKind) -> JsonError {
    match kind {
        std::io::ErrorKind::InvalidFilename => JsonError::invalid(
            "Reseed made no changes because a fixture path exceeds this platform's path length limit",
        ),
        // The container runs as uid 10001, so a bind-mounted fixture readable only by its owner
        // fails here until its permissions change.
        std::io::ErrorKind::PermissionDenied => JsonError::invalid(
            "Reseed made no changes because the emulator cannot read a fixture file or directory; make seed_dir and its contents readable by the emulator's user",
        ),
        _ => retry(),
    }
}

pub(super) fn admin_failure(error: &AdminError) -> JsonError {
    match error {
        AdminError::Precondition => JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Reseed made no changes because import or synchronization state is active",
        ),
        AdminError::Hold => JsonError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Reseed made no changes because an object is protected by a hold or retention",
        ),
        AdminError::Invalid(_) | AdminError::SeedProjectMismatch => {
            JsonError::invalid("Invalid fixture state; reseed made no changes")
        }
        AdminError::SeedMetadataBudget { mib } => JsonError::invalid(format!(
            "Reseed made no changes because fixture metadata exceeds {mib} MiB; raise max_seed_entries or reduce the tree"
        )),
        AdminError::TooLarge => JsonError::invalid(
            "Reseed made no changes because a fixture file exceeds max_object_bytes",
        ),
        // A retry cannot change an authorization decision.
        AdminError::Forbidden => JsonError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Reseed made no changes because a fixture write was not authorized",
        ),
        AdminError::Busy { .. } => JsonError::service_unavailable(
            "Reseed made no changes because catalog work, an unpublished body or another publication is active; retry after it completes",
        ),
        // A source that changed while it was read, a racing delete, a closing catalog and storage
        // failures are not the fixture tree's fault; another attempt can succeed once they pass.
        AdminError::SourceChanged { .. }
        | AdminError::Checksum
        | AdminError::BodyAborted(_)
        | AdminError::NotFound
        | AdminError::Closed
        | AdminError::DiskFull(_)
        | AdminError::Io(_)
        | AdminError::Corrupt(_)
        | AdminError::Backend(_) => retry(),
    }
}

fn retry() -> JsonError {
    JsonError::service_unavailable(
        "Reseed failed; inspect local fixture/storage diagnostics and retry",
    )
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
                "invalid",
                "Invalid fixture state; reseed made no changes",
            ),
            (
                AdminError::SeedProjectMismatch,
                StatusCode::BAD_REQUEST,
                "invalid",
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
        // A permission failure follows from the fixture tree and is a 400 below; others pass.
        let error = failure(&SeedError::Io(std::io::ErrorKind::Other.into()));
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Limits and fixture content arrive wrapped in the entry that met them, and the startup
    /// import wraps everything once more; the wrappers decide nothing and the label never leaks.
    #[test]
    fn failures_retrying_cannot_fix_answer_400_through_their_wrappers() {
        use std::io::ErrorKind;
        let wrapped = |error: SeedError| SeedError::Entry {
            label: "secret-bucket/private-name".into(),
            source: Box::new(error),
        };
        let invalid_name = || AdminError::Invalid("bad name".into());
        for (error, names) in [
            (SeedError::EntryLimit { limit: 3 }, "max_seed_entries"),
            (SeedError::DepthLimit { limit: 2 }, "max_seed_depth"),
            (
                wrapped(SeedError::BucketLimit { limit: 1 }),
                "max_buckets_per_batch",
            ),
            (
                wrapped(SeedError::BucketName(invalid_name())),
                "bucket name",
            ),
            (
                wrapped(SeedError::ObjectName(JsonError::invalid("bad"))),
                "object name",
            ),
            (wrapped(SeedError::NonUtf8Name), "UTF-8"),
            (wrapped(SeedError::UnsupportedFile), "regular file"),
            (wrapped(SeedError::FileLimit), "max_object_bytes"),
            (
                wrapped(SeedError::Admin(AdminError::TooLarge)),
                "max_object_bytes",
            ),
            (
                wrapped(SeedError::Admin(AdminError::SeedMetadataBudget { mib: 8 })),
                "exceeds 8 MiB; raise max_seed_entries",
            ),
            (
                wrapped(SeedError::Admin(AdminError::Invalid("bad metadata".into()))),
                "Invalid fixture state",
            ),
            (SeedError::NotDirectory, "not a directory"),
            (SeedError::Overlap, "overlap"),
            (SeedError::MissingRoot, "seed_dir"),
            (
                SeedError::Import(Box::new(wrapped(SeedError::Io(
                    ErrorKind::InvalidFilename.into(),
                )))),
                "path length",
            ),
            (
                wrapped(SeedError::Io(ErrorKind::PermissionDenied.into())),
                "cannot read a fixture",
            ),
            (
                SeedError::Io(ErrorKind::PermissionDenied.into()),
                "cannot read a fixture",
            ),
        ] {
            let reply = failure(&error);
            assert_eq!(
                (reply.status, reply.reason),
                (StatusCode::BAD_REQUEST, "invalid"),
                "{error}"
            );
            assert!(reply.message.contains(names), "{error}: {}", reply.message);
            assert!(!reply.message.contains("secret"), "{}", reply.message);
            assert!(!reply.message.contains("private"), "{}", reply.message);
        }
    }

    /// An authorization decision is the same on every attempt, so it is no retryable 503.
    #[test]
    fn a_refused_fixture_write_answers_403() {
        let reply = failure(&SeedError::Entry {
            label: "test-bucket/value".into(),
            source: Box::new(SeedError::Admin(AdminError::Forbidden)),
        });
        assert_eq!(
            (reply.status, reply.reason),
            (StatusCode::FORBIDDEN, "forbidden")
        );
        assert!(!reply.message.contains("test-bucket"), "{}", reply.message);
    }

    #[test]
    fn only_failures_a_later_attempt_can_finish_answer_503() {
        for error in [
            SeedError::Io(std::io::ErrorKind::Interrupted.into()),
            SeedError::Io(std::io::ErrorKind::NotFound.into()),
            SeedError::Io(std::io::ErrorKind::NotADirectory.into()),
            SeedError::Io(std::io::ErrorKind::IsADirectory.into()),
            SeedError::Admin(AdminError::NotFound),
            SeedError::Admin(AdminError::Io(std::io::ErrorKind::PermissionDenied.into())),
            SeedError::WalkStopped,
            SeedError::Entry {
                label: "test-bucket/value".into(),
                source: Box::new(SeedError::Admin(AdminError::SourceChanged {
                    fields: vec!["size".into()],
                })),
            },
            SeedError::Admin(AdminError::Busy {
                retry_after: std::time::Duration::from_secs(1),
            }),
            SeedError::Admin(AdminError::DiskFull(std::io::ErrorKind::StorageFull.into())),
            SeedError::Admin(AdminError::Closed),
        ] {
            assert_eq!(
                failure(&error).status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{error}"
            );
        }
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
