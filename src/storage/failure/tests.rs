use super::*;
use crate::json::JsonErrorResponse;

#[test]
fn object_error_keeps_authorization_from_catalog() {
    let denied = || {
        crate::auth::Denied::new(
            None,
            crate::auth::OBJECTS_GET,
            crate::auth::denied::Resource::in_bucket(crate::auth::OBJECTS_GET, "bucket"),
        )
    };
    for (input, expected) in [
        (CatalogError::Forbidden, ObjectError::Forbidden),
        (
            CatalogError::Denied(denied()),
            ObjectError::Denied(denied()),
        ),
        (
            CatalogError::PublicAccessPrevention,
            ObjectError::PublicAccessPrevention,
        ),
    ] {
        assert_eq!(ObjectError::from(input), expected);
    }
    assert_eq!(
        ObjectError::from(CatalogError::Forbidden),
        ObjectError::Forbidden
    );
}

#[test]
fn contextual_recovery_failures_keep_classification_and_sources() {
    let error = object_context(
        "staging cleanup",
        io::Error::from(io::ErrorKind::StorageFull),
    );
    assert!(
        matches!(error, ObjectError::Io(ref cause) if cause.kind() == io::ErrorKind::StorageFull)
    );
    assert!(crate::error::causes(&error).any(|error| error.downcast_ref::<io::Error>().is_some()));
    assert!(error.to_string().contains("staging cleanup"));
    let corrupt = object_unavailable(sqlite_failure(rusqlite::ffi::SQLITE_CORRUPT));
    let error = object_context("deletion journal", corrupt);
    assert!(matches!(error, ObjectError::Corrupt(_)));
    assert!(
        crate::error::causes(&error).any(|error| error.downcast_ref::<rusqlite::Error>().is_some())
    );
}

#[test]
fn contextual_catalog_failures_keep_classification_and_sources() {
    let error = catalog_context(
        "retired synchronization manifest",
        io::Error::from(io::ErrorKind::StorageFull),
    );
    assert!(
        matches!(error, CatalogError::Io(ref cause) if cause.kind() == io::ErrorKind::StorageFull)
    );
    assert!(crate::error::causes(&error).any(|error| error.downcast_ref::<io::Error>().is_some()));
    assert!(
        error
            .to_string()
            .contains("retired synchronization manifest")
    );
    let corrupt = unavailable(sqlite_failure(rusqlite::ffi::SQLITE_CORRUPT));
    let error = catalog_context("reset epoch", corrupt);
    assert!(matches!(error, CatalogError::Corrupt(_)));
    assert!(
        crate::error::causes(&error).any(|error| error.downcast_ref::<rusqlite::Error>().is_some())
    );
    let busy = catalog_context("manifest rows", sqlite_failure(rusqlite::ffi::SQLITE_BUSY));
    assert!(matches!(busy, CatalogError::Unavailable(_)));
    assert!(busy.to_string().contains("manifest rows"));
}

/// SQLite reports a memory catalog's page budget and a full disk alike, so only a job on a
/// memory catalog's connection marks the failure with that budget, and only while it runs.
#[test]
fn a_full_database_names_the_memory_catalog_budget_only_inside_its_job() {
    let full = || match unavailable(sqlite_failure(rusqlite::ffi::SQLITE_FULL)) {
        CatalogError::Io(cause) => cause,
        error => panic!("{error:?}"),
    };
    assert_eq!(full().memory_budget(), None);
    {
        let _job = MemoryCatalogJob::enter();
        let cause = full();
        assert_eq!(cause.kind(), io::ErrorKind::StorageFull);
        assert_eq!(cause.memory_budget(), Some(MemoryBudget::Catalog));
        assert!(
            crate::error::causes(cause.source().unwrap())
                .any(|error| error.downcast_ref::<rusqlite::Error>().is_some()),
            "the SQLite failure stays in the cause chain"
        );
        assert_eq!(
            crate::error::Report(&CatalogError::Io(cause)).to_string(),
            "catalog I/O: no storage space: RAM catalog page budget exhausted; increase memory_catalog_max_bytes: Error code 13: database or disk is full"
        );
    }
    assert_eq!(full().memory_budget(), None, "the job's scope ended");
}

#[test]
fn an_exhausted_blob_budget_survives_io_and_recovery_context_wrappers() {
    assert_eq!(
        IoCause::from(MemoryExhausted::new(MemoryBudget::Blobs)).memory_budget(),
        Some(MemoryBudget::Blobs)
    );
    let full = IoCause::from(BlobError::Exhausted);
    assert_eq!(full.kind(), io::ErrorKind::StorageFull);
    assert_eq!(full.memory_budget(), Some(MemoryBudget::Blobs));
    let ObjectError::Io(cause) = object_context("staging cleanup", BlobError::Exhausted) else {
        panic!("a full blob budget is I/O");
    };
    assert_eq!(cause.memory_budget(), Some(MemoryBudget::Blobs));
    assert_eq!(
        IoCause::from(io::Error::from(io::ErrorKind::StorageFull)).memory_budget(),
        None
    );
}

fn guidance_failures() -> Vec<(ObjectError, &'static str)> {
    use BackendFailure::*;
    let cases = [
        (
            CatalogClosed,
            "Object storage is unavailable: catalog is closed; wait for the server to restart before retrying",
        ),
        (
            UploadWaitTimeout,
            "Object storage is unavailable: upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 300000; 0 disables it); a completed upload may still commit",
        ),
        (
            RetainedSessionLimit,
            "Object storage is unavailable: retained upload session limit reached; raise max_resumable_sessions (0 is unlimited), wait for expired sessions to be reclaimed or use single-request media uploads",
        ),
        (
            RewriteSessionLimit,
            "Object storage is unavailable: rewrite session limit reached; raise max_rewrite_sessions (0 is unlimited) or wait for unfinished rewrites to expire",
        ),
        (
            FinalizationTimeout,
            "Object storage is unavailable: upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
        ),
        (
            CopyDeadline,
            "Object storage is unavailable: copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
        ),
        (
            SessionBusy,
            "Object storage is unavailable: upload session is busy; wait for the current request to finish before sending another request for this session, or increase catalog_wait_timeout_ms (for example, 60000)",
        ),
        (
            MissingCompletion,
            "Object storage is unavailable: internal storage failure (private backend details withheld)",
        ),
    ];
    let mut failures: Vec<_> = cases
        .into_iter()
        .flat_map(|(cause, message)| {
            [
                (object_unavailable(cause), message),
                (ObjectError::from(unavailable(cause)), message),
            ]
        })
        .collect();
    for (failure, guidance) in crate::storage::catalog::test_fixtures::guidance_source_failures() {
        failures.push((
            failure,
            cases.iter().find(|(kind, _)| *kind == guidance).unwrap().1,
        ));
    }
    for error in [
        crate::server::copy::CheckpointError::ObserverDropped,
        crate::server::copy::CheckpointError::Abandoned,
    ] {
        let message = "Object storage is unavailable: internal storage failure (private backend details withheld)";
        failures.push((ObjectError::backend(error), message));
        failures.push((
            ObjectError::from(unavailable(ObjectError::backend(error))),
            message,
        ));
    }
    failures
}

#[tokio::test]
async fn backend_guidance_wire_responses_are_stable_across_protocols() {
    use http_body_util::BodyExt;
    use hyper::{HeaderMap, StatusCode};

    for (((grpc, message), (xml, xml_message)), (json, json_message)) in guidance_failures()
        .into_iter()
        .zip(guidance_failures())
        .zip(guidance_failures())
    {
        assert_eq!((xml_message, json_message), (message, message));
        let grpc: hyper::Response<tonic::body::Body> = tonic::Status::from(grpc).into_http();
        assert_eq!(grpc.status(), StatusCode::OK);
        let expected: HeaderMap = [
            ("content-type", "application/grpc".to_owned()),
            ("grpc-status", "14".to_owned()),
            ("grpc-message", message.replace(' ', "%20")),
        ]
        .into_iter()
        .map(|(key, value)| {
            (
                key.parse::<hyper::header::HeaderName>().unwrap(),
                value.parse().unwrap(),
            )
        })
        .collect();
        assert_eq!(grpc.headers(), &expected);
        assert!(
            grpc.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );

        let xml = crate::xml::Error::from(xml).response(crate::xml::ErrorBody::Include);
        assert_eq!(xml.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = format!(
            "<?xml version='1.0' encoding='UTF-8'?><Error><Code>ServiceUnavailable</Code><Message>{message}</Message></Error>"
        );
        let expected: HeaderMap = [
            ("content-type", "application/xml; charset=UTF-8".to_owned()),
            ("cache-control", "private, max-age=0".to_owned()),
            ("content-length", body.len().to_string()),
        ]
        .into_iter()
        .map(|(key, value)| {
            (
                key.parse::<hyper::header::HeaderName>().unwrap(),
                value.parse().unwrap(),
            )
        })
        .collect();
        assert_eq!(xml.headers(), &expected);
        assert_eq!(xml.into_body().collect().await.unwrap().to_bytes(), body);

        let json = crate::ApiError::from(json).into_response();
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

fn sqlite_failure(code: std::ffi::c_int) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
}

#[test]
fn backend_guidance_survives_changed_and_nested_contexts() {
    for cause in [
        BackendCause::with_source("renamed backend diagnostic", BackendFailure::CatalogClosed),
        BackendCause::from_error(CatalogError::Unavailable(BackendCause::with_source(
            "renamed catalog diagnostic",
            BackendFailure::CatalogClosed,
        ))),
        // A context inside another context, as wrapped backend diagnostics travel.
        BackendCause::from_error(BackendCause::with_source(
            "renamed nested diagnostic",
            BackendFailure::CatalogClosed,
        )),
        // A closed catalog that an administrative operation reported as a backend failure.
        BackendCause::from_error(crate::storage::admin::AdminError::from(
            CatalogError::Unavailable(BackendFailure::CatalogClosed.into()),
        )),
    ] {
        assert_eq!(cause.failure(), Some(BackendFailure::CatalogClosed));
        assert!(cause.source().is_some());
        assert_eq!(
            crate::ApiError::from(ObjectError::Unavailable(cause)).message(),
            guidance_failures()[0].1
        );
    }
}

#[test]
fn backend_failure_events_carry_the_sqlite_extended_code_and_nothing_else_new() {
    // SQLite's own message names only the primary code, whatever the extended one.
    let temporary_path = || {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR_GETTEMPPATH),
            Some("disk I/O error".into()),
        )
    };
    for cause in [
        BackendCause::from_error(temporary_path()),
        // A catalog failure inside an object failure, as a shared batch failure carries it.
        BackendCause::from_error(unavailable(temporary_path())),
        BackendCause::with_source("renamed diagnostic", temporary_path()),
    ] {
        let event = serde_json::to_value(wording::BackendFailureEvent::from(&cause)).unwrap();
        let cause = if cause.message() == "renamed diagnostic" {
            "unrecognized backend failure (private details withheld)"
        } else {
            "disk I/O error"
        };
        assert_eq!(
            event,
            serde_json::json!({
                "cause": cause,
                "sqlite_extended_code": rusqlite::ffi::SQLITE_IOERR_GETTEMPPATH,
            })
        );
    }
    // Without an SQLite failure in its chain, the event keeps only the allowlisted cause.
    for cause in [
        BackendCause::new("database is locked: /secret token=private"),
        BackendCause::from_error(io::Error::other("disk I/O error")),
    ] {
        let event = serde_json::to_value(wording::BackendFailureEvent::from(&cause)).unwrap();
        assert_eq!(event.as_object().unwrap().len(), 1, "{event}");
        assert!(!event.to_string().contains("secret"), "{event}");
    }
}

#[test]
fn sqlite_corruption_and_decoding_failures_are_corrupt_with_their_text() {
    let corrupt = sqlite_failure(rusqlite::ffi::SQLITE_CORRUPT);
    let text = corrupt.to_string();
    let error = unavailable(corrupt);
    assert!(matches!(&error, CatalogError::Corrupt(cause) if cause.message() == text));
    assert_eq!(error.to_string(), format!("catalog corrupt: {text}"));
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string() == text)
    );

    let decoding = rusqlite::Error::InvalidColumnType(0, "x".into(), rusqlite::types::Type::Null);
    assert!(matches!(
        object_unavailable(decoding),
        ObjectError::Corrupt(_)
    ));
}

#[test]
fn full_sqlite_disk_and_filesystem_errors_are_io_with_their_kind() {
    let error = unavailable(sqlite_failure(rusqlite::ffi::SQLITE_FULL));
    assert!(
        matches!(&error, CatalogError::Io(cause) if cause.kind() == io::ErrorKind::StorageFull)
    );
    assert_eq!(error.to_string(), "catalog I/O: no storage space");
    assert!(error.source().is_some());

    let error = object_unavailable(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
    assert_eq!(
        error,
        ObjectError::Io(io::ErrorKind::PermissionDenied.into())
    );
    assert_eq!(error.to_string(), "object I/O: permission denied");
    assert_eq!(format!("{error:?}"), "Io(PermissionDenied)");
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string() == "denied")
    );
}

#[test]
fn other_failures_are_unavailable_with_the_stringified_message() {
    let busy = sqlite_failure(rusqlite::ffi::SQLITE_BUSY);
    let text = busy.to_string();
    let error = unavailable(busy);
    assert_eq!(error.to_string(), format!("catalog unavailable: {text}"));
    assert_eq!(format!("{error:?}"), format!("Unavailable({text:?})"));
    assert!(error.source().is_some());

    let error = object_unavailable("catalog is closed");
    assert!(matches!(&error, ObjectError::Unavailable(cause) if *cause == "catalog is closed"));
    assert!(error.source().is_none());
}

#[test]
fn classification_passes_through_already_classified_errors() {
    let nested = unavailable(ObjectError::Corrupt("bad row".into()));
    assert_eq!(nested, CatalogError::Corrupt("bad row".into()));
    let nested = object_unavailable(CatalogError::Io(io::ErrorKind::NotFound.into()));
    assert_eq!(nested, ObjectError::Io(io::ErrorKind::NotFound.into()));
    // Unclassified errors keep their full display text, as before sources were kept.
    let nested = unavailable(CatalogError::NotFound);
    assert_eq!(nested.to_string(), "catalog unavailable: bucket not found");
}

/// A failure already classified as unavailable keeps its own cause through every mapping, so
/// its text names the unavailable catalog once and log allowlists can still recognize it.
#[test]
fn unavailable_failures_pass_through_with_one_prefix_and_their_cause() {
    let cause = || BackendCause::with_source("offline", BackendFailure::CatalogClosed);
    let catalog = || CatalogError::Unavailable(cause());
    let object = || ObjectError::Unavailable(cause());
    for error in [unavailable(catalog()), unavailable(object())] {
        assert_eq!(error, CatalogError::Unavailable("offline".into()));
        assert_eq!(error.to_string(), "catalog unavailable: offline");
        assert_eq!(
            error.source().map(ToString::to_string),
            Some(BackendFailure::CatalogClosed.to_string())
        );
    }
    for error in [
        object_unavailable(object()),
        object_unavailable(catalog()),
        ObjectError::from(catalog()),
    ] {
        assert_eq!(error, ObjectError::Unavailable("offline".into()));
        assert_eq!(error.to_string(), "catalog unavailable: offline");
        assert_eq!(
            error.source().map(ToString::to_string),
            Some(BackendFailure::CatalogClosed.to_string())
        );
    }
}

#[test]
fn catalog_from_conversion_preserves_backend_classification_and_cause_chain() {
    let cases: [fn() -> CatalogError; 5] = [
        || CatalogError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied").into()),
        || {
            CatalogError::Corrupt(BackendCause::from_error(sqlite_failure(
                rusqlite::ffi::SQLITE_CORRUPT,
            )))
        },
        || CatalogError::Unavailable("offline".into()),
        || CatalogError::NotFound,
        || CatalogError::InvalidMetadata("invalid field".into()),
    ];
    for make_error in cases {
        let mapped = object_unavailable(make_error());
        let converted = ObjectError::from(make_error());
        assert_eq!(converted, mapped);
        assert_eq!(converted.to_string(), mapped.to_string());
        assert_eq!(
            converted.source().map(ToString::to_string),
            mapped.source().map(ToString::to_string)
        );
    }
}

/// A typed `Other` I/O cause reads exactly like the `io::Error::other` wrapper it replaces,
/// while the failure it holds keeps its own type.
#[test]
fn an_other_io_cause_reports_like_an_io_error_wrapper() {
    use crate::storage::{admin::AdminError, import_journal::ResumeError};
    let failure = || {
        ResumeError::Catalog(CatalogError::Unavailable(BackendCause::from_error(
            AdminError::busy(),
        )))
    };
    let wrapped = unavailable(io::Error::other(failure()));
    let typed = CatalogError::Io(IoCause::other(failure()));
    assert_eq!(typed, wrapped);
    assert_eq!(typed.to_string(), wrapped.to_string());
    assert_eq!(format!("{typed:?}"), format!("{wrapped:?}"));
    assert_eq!(
        crate::error::Report(&typed).to_string(),
        crate::error::Report(&wrapped).to_string()
    );
    assert!(typed.source().unwrap().is::<ResumeError>());
}
