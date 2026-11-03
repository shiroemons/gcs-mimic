use super::*;
use crate::{auth::AuthFailure, bucket::CatalogError, csek::CsekError, object::ObjectError};

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
