use super::*;

#[test]
fn object_error_keeps_authorization_from_catalog() {
    for (input, expected) in [
        (CatalogError::Forbidden, ObjectError::Forbidden),
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

fn guidance_failures() -> Vec<(ObjectError, &'static str)> {
    use BackendFailure::*;
    let cases = [
        (
            CatalogClosed,
            "Object storage is unavailable: catalog is closed; wait for the server to restart before retrying",
        ),
        (
            UploadWaitTimeout,
            "Object storage is unavailable: upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 60000); a completed upload may still commit",
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
            "Object storage is unavailable: upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000)",
        ),
        (
            CopyDeadline,
            "Object storage is unavailable: copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000)",
        ),
        (
            SessionBusy,
            "Object storage is unavailable: upload session is busy; wait for the current request to finish before sending another request for this session",
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

    for (failure, message) in guidance_failures() {
        let error = crate::json::JsonError::from(failure);
        let grpc: hyper::Response<tonic::body::Body> =
            tonic::Status::from(error.clone()).into_http();
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

        let xml = crate::xml::Error::from(error.clone()).response(crate::xml::ErrorBody::Include);
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

        let json = error.into_response();
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
fn backend_guidance_survives_changed_context_and_io_wrappers() {
    for cause in [
        BackendCause::with_source("renamed backend diagnostic", BackendFailure::CatalogClosed),
        BackendCause::from_error(CatalogError::Unavailable(BackendCause::with_source(
            "renamed catalog diagnostic",
            BackendFailure::CatalogClosed,
        ))),
        BackendCause::from_error(io::Error::other(BackendCause::with_source(
            "renamed I/O diagnostic",
            BackendFailure::CatalogClosed,
        ))),
    ] {
        assert_eq!(cause.failure(), Some(BackendFailure::CatalogClosed));
        assert!(cause.source().is_some());
        assert_eq!(
            crate::json::JsonError::from(ObjectError::Unavailable(cause)).message,
            guidance_failures()[0].1
        );
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
