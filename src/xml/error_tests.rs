use super::*;
use crate::{auth::AuthFailure, bucket::CatalogError, csek::CsekError, object::ObjectError};

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
        assert_eq!(error.status, status);
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
