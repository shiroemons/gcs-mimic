use super::*;
use crate::{auth::AuthFailure, bucket::CatalogError, csek::CsekError, object::ObjectError};

#[test]
fn storage_capacity_failures_keep_the_xml_service_unavailable_contract() {
    let error = Error::from(JsonError::from(ObjectError::Io(
        std::io::ErrorKind::StorageFull.into(),
    )));
    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.code.as_str(), "ServiceUnavailable");
    assert!(error.message.starts_with("Storage capacity exhausted;"));
}

#[test]
fn legacy_trailing_dot_hosts_do_not_fall_through_to_a_shorter_domain() {
    let endpoints = EndpointSnapshot::explicit(
        "http://localhost",
        None,
        vec!["storage.example.com".into(), "example.com".into()],
    );
    let public = "storage.example.com".parse().unwrap();
    let host = "bucket.storage.example.com.".parse().unwrap();
    assert_eq!(
        legacy_virtual_bucket(&host, &public, &endpoints),
        (None, true)
    );
}

#[test]
fn xml_codes_preserve_exceptional_statuses_and_spelling() {
    for (code, status, spelling) in [
        (
            XmlCode::MalformedXml,
            StatusCode::BAD_REQUEST,
            "MalformedXML",
        ),
        (
            XmlCode::UploadBrokenConnection,
            StatusCode::REQUEST_TIMEOUT,
            "uploadBrokenConnection",
        ),
        (
            XmlCode::RequestTimeout,
            StatusCode::BAD_REQUEST,
            "RequestTimeout",
        ),
        (
            XmlCode::MissingContentLength,
            StatusCode::LENGTH_REQUIRED,
            "MissingContentLength",
        ),
        (
            XmlCode::InvalidRange,
            StatusCode::RANGE_NOT_SATISFIABLE,
            "InvalidRange",
        ),
        (
            XmlCode::BucketNameUnavailable,
            StatusCode::CONFLICT,
            "BucketNameUnavailable",
        ),
        (
            XmlCode::InvalidPartOrder,
            StatusCode::BAD_REQUEST,
            "InvalidPartOrder",
        ),
        (
            XmlCode::ExpiredToken,
            StatusCode::BAD_REQUEST,
            "ExpiredToken",
        ),
    ] {
        assert_eq!(code.status(), status);
        assert_eq!(code.as_str(), spelling);
    }
    for status in [
        StatusCode::METHOD_NOT_ALLOWED,
        StatusCode::PAYLOAD_TOO_LARGE,
        StatusCode::NOT_IMPLEMENTED,
        StatusCode::GATEWAY_TIMEOUT,
    ] {
        let error = Error::from(JsonError::new(status, "other", "message"));
        assert_eq!(error.status(), status);
        assert_eq!(error.code.as_str(), "InvalidArgument");
    }
}

#[tokio::test]
async fn typed_json_errors_keep_the_xml_status_code_and_document() {
    for (mut json, status, code, message) in [
        (
            JsonError::from(AuthFailure::LoginRequired),
            StatusCode::UNAUTHORIZED,
            "AuthenticationRequired",
            "Login Required",
        ),
        (
            JsonError::from(AuthFailure::InvalidCredentials),
            StatusCode::BAD_REQUEST,
            "InvalidAuthentication",
            "Invalid Credentials",
        ),
        (
            JsonError::from(CatalogError::NotFound),
            StatusCode::NOT_FOUND,
            "NoSuchBucket",
            "The specified bucket does not exist.",
        ),
        (
            JsonError::from(ObjectError::ObjectNotFound).naming_object("bucket", "object"),
            StatusCode::NOT_FOUND,
            "NoSuchKey",
            NO_SUCH_KEY,
        ),
        (
            JsonError::from(CsekError::KeyIncorrect),
            StatusCode::BAD_REQUEST,
            "CustomerEncryptionKeyIsIncorrect",
            "The provided encryption key is incorrect.",
        ),
    ] {
        json.reason = "unrelatedReason";
        let error = Error::from(json);
        let response = error.response(ErrorBody::Include);
        assert_eq!(response.status(), status);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            bytes.as_ref(),
            format!("{DECLARATION}<Error><Code>{code}</Code><Message>{message}</Message></Error>")
                .as_bytes()
        );
    }
}

/// An unknown signer's answer says where this profile looks for keys, and a zero expiry names
/// its likely cause, so a client can tell what to change.
#[test]
fn signature_errors_explain_unknown_signers_and_a_zero_expiry() {
    use crate::auth::signature::{
        ExpiresParameter, Failure, InvalidExpires, KeyLookup, VerificationError,
    };
    let message = |failure| signature_error(VerificationError::Signature(failure)).message;
    let unknown = |lookup| {
        let error = signature_error(VerificationError::Signature(Failure::UnknownSigner(lookup)));
        assert_eq!(error.code, XmlCode::InvalidAccessKeyId);
        error.message
    };
    assert_eq!(
        unknown(KeyLookup::Registry),
        "The signing key or its registered identity is unknown or inactive."
    );
    let permissive = unknown(KeyLookup::PermissiveKeys);
    assert!(!permissive.contains("registered identity"), "{permissive}");
    for source in [
        "identity file public_keys",
        "HMAC keys created through the JSON API",
        "the built-in test signer",
    ] {
        assert!(permissive.contains(source), "{permissive}");
    }
    assert_eq!(
        unknown(KeyLookup::DisabledTestSigner),
        "test-signer@gcs-mimic.invalid is the built-in test signer, which is disabled; set test_signer=enabled."
    );
    assert!(unknown(KeyLookup::Ignored).contains("set signed_urls=verify"));
    for (parameter, name) in [
        (ExpiresParameter::Google, "X-Goog-Expires"),
        (ExpiresParameter::Aws, "X-Amz-Expires"),
    ] {
        assert_eq!(
            message(Failure::ExpiresOutOfRange(parameter, InvalidExpires::Zero)),
            format!(
                "{name} must be a whole number of seconds from 1 to 604800; 0 usually means the signer's expiry was less than one second away."
            )
        );
        assert_eq!(
            message(Failure::ExpiresOutOfRange(parameter, InvalidExpires::Other)),
            format!("{name} must be a whole number of seconds from 1 to 604800.")
        );
    }
}
