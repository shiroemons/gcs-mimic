use super::*;
use crate::storage::failure::{test_support::denied, wording::safe_backend_cause};
use serde_json::{Value, json};

/// A tester whose Docker disk filled under `backend=disk` was told about RAM budgets first;
/// each storage that can run out now names only its own remedy.
#[test]
fn storage_capacity_names_only_the_remedy_of_the_storage_that_ran_out() {
    use crate::cause::{IoCause, MemoryBudget, MemoryExhausted};
    let memory = |budget| IoCause::from(MemoryExhausted::new(budget));
    let disk = || IoCause::from(std::io::ErrorKind::StorageFull);
    for (cause, names, omits) in [
        (
            disk(),
            &[
                "data directory",
                "softDeletePolicy.retentionDurationSeconds",
                "generation preconditions",
            ][..],
            &[
                "memory_max_bytes",
                "memory_catalog_max_bytes",
                "backend=",
                "default_soft_delete_retention_seconds",
            ][..],
        ),
        (
            memory(MemoryBudget::Blobs),
            &[
                "delete objects",
                "softDeletePolicy.retentionDurationSeconds",
                "memory_max_bytes",
                "restart loses RAM data",
            ][..],
            &[
                "memory_catalog_max_bytes",
                "data directory",
                "default_soft_delete_retention_seconds",
            ][..],
        ),
        (
            memory(MemoryBudget::Catalog),
            &["memory_catalog_max_bytes", "restart loses RAM data"][..],
            &["memory_max_bytes", "data directory", "softDeletePolicy"][..],
        ),
    ] {
        let object = ApiError::from(ObjectError::Io(cause.clone()));
        let catalog = ApiError::from(CatalogError::Io(cause));
        assert_eq!(object.message, catalog.message);
        assert_eq!(object.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(object.reason, "backendError");
        let message = &object.message;
        assert!(
            message.starts_with("Storage capacity exhausted;"),
            "{message}"
        );
        for name in names {
            assert!(message.contains(name), "{name}: {message}");
        }
        for name in omits {
            assert!(!message.contains(name), "{name}: {message}");
        }
    }
}

#[test]
fn error_classification_does_not_depend_on_the_public_wording() {
    let mut bucket = ApiError::from(CatalogError::NotFound);
    bucket.message = "different bucket wording".into();
    bucket.reason = "differentReason";
    assert!(bucket.is_missing_bucket());
    let mut object = ApiError::from(ObjectError::ObjectNotFound).naming_object("b", "o");
    assert_eq!(object.message, "No such object: b/o");
    object.message = "different object wording".into();
    assert!(object.is_missing_object());
    assert_eq!(
        object.naming_object("other", "other").message,
        "different object wording"
    );
    let unrelated = ApiError::invalid_parameter(OBJECT_NOT_FOUND);
    assert!(!unrelated.is_missing_object());
}

/// The logged event names the anonymous caller the response answers, with the refused
/// permission and resource, even when the refusal named a stored principal.
#[test]
fn an_anonymous_callers_refusal_logs_the_anonymous_caller() {
    let event = |error: ApiError| {
        error
            .for_anonymous_caller()
            .denied
            .map(|denied| denied.event_value())
    };
    let anonymous = json!({"anonymous": true, "permission": "storage.buckets.get",
        "resource": "bucket", "bucket": "bucket"});
    assert_eq!(
        event(ApiError::from(CatalogError::Denied(denied(None)))),
        Some(anonymous.clone())
    );
    assert_eq!(
        event(ApiError::from(ObjectError::Denied(denied(Some(
            "snapshot@example.invalid"
        ))))),
        Some(anonymous)
    );
    assert_eq!(event(ApiError::from(ObjectError::LockedRetention)), None);
}

#[test]
fn holds_and_compose_components_are_named_in_their_errors() {
    use crate::object::{ComponentFailure, ComponentFailureKind, HeldObject, HoldKind};
    let held = |kind| {
        ApiError::from(ObjectError::ActiveHold(Box::new(HeldObject {
            bucket: "b".into(),
            name: "o".into(),
            kind,
        })))
    };
    let error = held(HoldKind::EventBased);
    assert_eq!((error.status.as_u16(), error.reason), (403, "forbidden"));
    assert_eq!(
        error.message,
        "Object 'b/o' is under active Event-Based hold and cannot be deleted, overwritten or archived until hold is removed."
    );

    let component = |kind| {
        ApiError::from(ObjectError::Component(Box::new(ComponentFailure {
            bucket: "b".into(),
            name: "a".into(),
            kind,
        })))
    };
    assert_eq!(
        component(ComponentFailureKind::NotFound).message,
        "No such object: b/a"
    );
    let mismatch = component(ComponentFailureKind::GenerationMismatch {
        required: 1,
        actual: 7,
    });
    assert_eq!(mismatch.status.as_u16(), 412);
    assert_eq!(
        mismatch.message,
        "Generation 1 required for component object (b/a), but actual generation is 7."
    );
}

#[test]
fn backend_causes_are_actionable_and_private_details_stay_private() {
    if std::env::var_os("GCSM_TEST_BACKEND_LOG").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "json::error::domain_tests::backend_causes_are_actionable_and_private_details_stay_private",
                "--nocapture",
            ])
            .env("GCSM_TEST_BACKEND_LOG", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains("/secret"));
        assert!(!stderr.contains("token="));
        let events: Vec<Value> = stderr
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 14);
        assert!(events.iter().all(|event| {
            event["event"] == "object_backend_failure" && event["level"] == "error"
        }));
        assert_eq!(events[1]["cause"], "database is locked");
        assert_eq!(events[2]["cause"], "Permission denied");
        assert_eq!(events[6]["cause"], "database is locked");
        assert_eq!(events[7]["cause"], "invalid opaque blob ID");
        assert_eq!(events[10]["cause"], "Acknowledged upload prefix is missing");
        assert_eq!(events[12]["cause"], "database is locked");
        return;
    }
    for cause in [
        BackendFailure::CatalogClosed,
        BackendFailure::UploadWaitTimeout,
        BackendFailure::RetainedSessionLimit,
        BackendFailure::RewriteSessionLimit,
        BackendFailure::FinalizationTimeout,
        BackendFailure::CopyDeadline,
        BackendFailure::SessionBusy,
    ] {
        let error = ApiError::from(ObjectError::Unavailable(cause.into()));
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains(cause.message()));
    }
    let error = ApiError::from(ObjectError::Unavailable(
        "failure at /secret/catalog with token=private".into(),
    ));
    assert!(!error.message.contains("/secret"));
    assert!(!error.message.contains("token="));
    for (cause, expected) in [
        (
            "catalog unavailable: database is locked",
            "database is locked",
        ),
        (
            "Permission denied (os error 13): token=private",
            "Permission denied",
        ),
        (
            "database is locked: /secret/catalog token=private",
            "database is locked",
        ),
        (
            "database is locked token=private",
            "unrecognized backend failure (private details withheld)",
        ),
        (
            "failure at /secret/catalog with token=private",
            "unrecognized backend failure (private details withheld)",
        ),
    ] {
        let cause = cause.trim_prefix("catalog unavailable: ");
        assert_eq!(safe_backend_cause(cause), expected);
        let error = ApiError::backend(&cause.into());
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            error.message,
            "Object storage is unavailable: internal storage failure (private backend details withheld)"
        );
    }
    use crate::storage::hmac_keys::HmacKeyError;
    let catalog = "Bucket catalog is unavailable";
    let corrupt =
        "Object storage is unavailable: stored data is corrupt; restore a valid data directory";
    for (error, expected) in [
        (
            ApiError::from(CatalogError::Unavailable(
                "database is locked: /secret token=private".into(),
            )),
            catalog,
        ),
        (
            ApiError::from(CatalogError::Corrupt(
                "invalid opaque blob ID: /secret token=private".into(),
            )),
            catalog,
        ),
        (
            ApiError::from(CatalogError::Io(
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Permission denied: /secret token=private",
                )
                .into(),
            )),
            catalog,
        ),
        (
            ApiError::from(CatalogError::NewerSchema {
                version: 99,
                supported: 50,
            }),
            catalog,
        ),
        (
            ApiError::from(ObjectError::Corrupt(
                "Acknowledged upload prefix is missing: /secret token=private".into(),
            )),
            corrupt,
        ),
        (
            ApiError::from(ObjectError::Corrupt("/secret token=private".into())),
            corrupt,
        ),
        (
            ApiError::from(HmacKeyError::Catalog(CatalogError::Unavailable(
                "database is locked: /secret token=private".into(),
            ))),
            catalog,
        ),
        (
            ApiError::from(HmacKeyError::Catalog(CatalogError::Corrupt(
                "/secret token=private".into(),
            ))),
            catalog,
        ),
    ] {
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.reason, "backendError");
        assert_eq!(error.message, expected);
    }
}

/// Each fixed failure keeps its remedy word for word, after the object or the bucket subject.
#[test]
fn backend_remedies_keep_their_object_and_bucket_wording() {
    for (failure, remedy) in [
        (
            BackendFailure::CatalogClosed,
            "catalog is closed; wait for the server to restart before retrying",
        ),
        (
            BackendFailure::UploadWaitTimeout,
            "upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 300000; 0 disables it); a completed upload may still commit",
        ),
        (
            BackendFailure::RetainedSessionLimit,
            "retained upload session limit reached; raise max_resumable_sessions (0 is unlimited), wait for expired sessions to be reclaimed or use single-request media uploads",
        ),
        (
            BackendFailure::RewriteSessionLimit,
            "rewrite session limit reached; raise max_rewrite_sessions (0 is unlimited) or wait for unfinished rewrites to expire",
        ),
        (
            BackendFailure::FinalizationTimeout,
            "upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
        ),
        (
            BackendFailure::CopyDeadline,
            "copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
        ),
        (
            BackendFailure::SessionBusy,
            "upload session is busy; wait for the current request to finish before sending another request for this session, or increase catalog_wait_timeout_ms (for example, 60000)",
        ),
    ] {
        let object = ApiError::from(ObjectError::Unavailable(failure.into()));
        assert_eq!(
            object.message,
            format!("Object storage is unavailable: {remedy}")
        );
        assert_eq!(object.kind, ErrorKind::Backend(failure));
        let bucket = ApiError::from(CatalogError::Unavailable(failure.into()));
        assert_eq!(
            bucket.message,
            format!("Bucket catalog is unavailable: {remedy}")
        );
        assert_eq!(bucket.kind, ErrorKind::Backend(failure));
        let corrupt = ApiError::from(ObjectError::Corrupt(failure.into()));
        assert_eq!(
            corrupt.message,
            "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
        );
        assert_eq!(corrupt.kind, ErrorKind::Backend(failure));
    }
}

#[tokio::test]
async fn backend_guidance_keeps_its_json_headers_and_body() {
    use http_body_util::BodyExt;
    use hyper::HeaderMap;
    for (json, message) in crate::storage::failure::test_support::guidance_failures() {
        let json = ApiError::from(json).into_response();
        assert_eq!(json.status(), StatusCode::SERVICE_UNAVAILABLE);
        let expected: HeaderMap = [
            ("content-type", "application/json; charset=UTF-8"),
            (
                "cache-control",
                "no-cache, no-store, max-age=0, must-revalidate",
            ),
            ("pragma", "no-cache"),
        ]
        .into_iter()
        .map(|(key, value)| {
            (
                key.parse::<hyper::header::HeaderName>().unwrap(),
                value.parse().unwrap(),
            )
        })
        .collect();
        assert_eq!(json.headers(), &expected);
        let body = format!(
            "{{\n  \"error\": {{\n    \"code\": 503,\n    \"message\": \"{message}\",\n    \"errors\": [\n      {{\n        \"message\": \"{message}\",\n        \"domain\": \"global\",\n        \"reason\": \"backendError\"\n      }}\n    ]\n  }}\n}}"
        );
        assert_eq!(json.into_body().collect().await.unwrap().to_bytes(), body);
    }
}

#[test]
fn bucket_validation_preserves_the_rejected_setting() {
    for value in [
        "storageClass",
        "isLive",
        "storageClass.isLive",
        "terminalStorageClass",
    ] {
        let error = crate::bucket::validate_update_storage_class(value).unwrap_err();
        assert_eq!(
            ApiError::from(error).message(),
            format!("Invalid storage class \"{value}\"")
        );
    }
    let error = CatalogError::Metadata(crate::metadata_error::MetadataError::AutoclassTerminal);
    assert_eq!(
        ApiError::from(error).message(),
        "Invalid value for terminalStorageClass field in Autoclass config. Valid values are NEARLINE and ARCHIVE"
    );
    let patch = br#"{"lifecycle":{"rule":[{"action":{"type":"Delete"},"condition":{"age":-1}}]}, "defaultEventBasedHold":true}"#;
    let error =
        crate::json::parse_bucket_patch(patch, crate::profile::CompatProfile::Native).unwrap_err();
    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn verifier_and_corrupt_storage_keep_their_json_responses() {
    let unavailable = crate::auth::AuthError::Unavailable(BackendFailure::CatalogClosed.into());
    assert_eq!(
        ApiError::from(unavailable).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let catalog = ApiError::from(CatalogError::Corrupt("private detail".into()));
    assert_eq!(catalog.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(catalog.message(), "Bucket catalog is unavailable");
    assert_eq!(
        ApiError::from(ObjectError::Corrupt("private detail".into())).message(),
        "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
    );
}

#[test]
fn upload_validation_retains_json_status_and_reason() {
    for case in crate::storage::failure::test_support::csek_cases() {
        assert_eq!(ApiError::from(case.error).reason(), case.error.reason());
    }
    for error in [
        MultipartError::Framing,
        MultipartError::BadContentType,
        MultipartError::UnsupportedParameter("start".into()),
    ] {
        assert_eq!(ApiError::from(error).status(), StatusCode::BAD_REQUEST);
    }
    for (_, error) in crate::error::golden_tests::upload::hash_cases() {
        assert_eq!(ApiError::from(error).status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        ApiError::from(UploadHeaderError::DuplicateContentType).status(),
        StatusCode::BAD_REQUEST
    );
    let mut record = crate::storage::test_fixtures::bare_object_record();
    record.content_type = "text/plain\r\nx: no".to_owned();
    let error = crate::object::media_headers::object_media_headers(&record).unwrap_err();
    assert_eq!(
        ApiError::from(error).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
