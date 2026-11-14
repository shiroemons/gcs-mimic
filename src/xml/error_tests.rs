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
fn signing_parameters_ignore_ascii_case_only_for_v4_names() {
    for name in [
        "X-Goog-Algorithm",
        "x-goog-algorithm",
        "X-GOOG-SIGNATURE",
        "x-goog-signedheaders",
        "X-Amz-Date",
        "x-amz-credential",
        "GoogleAccessId",
        "Expires",
        "Signature",
    ] {
        assert!(signing_parameter(name), "{name}");
    }
    for name in ["googleaccessid", "expires", "SIGNATURE", "x-goog-other"] {
        assert!(!signing_parameter(name), "{name}");
    }
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
    let denied = |id: Option<&str>| {
        let identity = id.map(|id| {
            crate::auth::Identity::from_upload_snapshot(
                &serde_json::json!({"id": id, "projects": {}}),
            )
            .unwrap()
        });
        crate::auth::Denied::new(
            identity.as_ref(),
            crate::auth::OBJECTS_GET,
            crate::auth::denied::Resource::in_bucket(crate::auth::OBJECTS_GET, "bucket"),
        )
    };
    for (mut json, status, code, message) in [
        (
            JsonError::from(CatalogError::Denied(denied(Some("viewer@example.invalid")))),
            StatusCode::FORBIDDEN,
            "AccessDenied",
            "viewer@example.invalid does not have storage.objects.get access to the Google Cloud Storage object. Permission 'storage.objects.get' denied on resource (or it may not exist).",
        ),
        (
            JsonError::from(ObjectError::Denied(denied(None))).for_anonymous_caller(),
            StatusCode::UNAUTHORIZED,
            "AuthenticationRequired",
            "Anonymous caller does not have storage.objects.get access to the Google Cloud Storage object. Permission 'storage.objects.get' denied on resource (or it may not exist).",
        ),
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
        "the public_keys of the identity document (identity_file or identities)",
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

/// A legacy header signature outside its time window answers as clock skew, while an expired
/// V4 header or V2 query signature answers as an expired token. Time is checked before the key
/// is looked up, so no signer needs to exist.
#[tokio::test]
async fn legacy_header_clock_skew_answers_request_time_too_skewed() {
    let dir = crate::test_support::TestDir::new();
    let catalog = crate::storage::Catalog::open(&dir.0).await.unwrap();
    let auth = crate::auth::Auth::strict(
        crate::auth::Registry::parse(br#"{"identities":[{"id":"user","tokens":["token"]}]}"#)
            .unwrap(),
    );
    let signed_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_175_024_202);
    let header = |pairs: &[(&'static str, &str)]| {
        let mut headers = HeaderMap::new();
        headers.insert("host", "localhost:4443".parse().unwrap());
        for (name, value) in pairs {
            headers.insert(*name, value.parse().unwrap());
        }
        headers
    };
    let date = ("date", "Tue, 27 Mar 2007 19:36:42 GMT");
    let v4 = [
        (
            "authorization",
            "GOOG4-HMAC-SHA256 Credential=GOOG1EXAMPLE/20070327/auto/storage/goog4_request, SignedHeaders=host;x-goog-date, Signature=0000000000000000000000000000000000000000000000000000000000000000",
        ),
        ("x-goog-date", "20070327T193642Z"),
    ];
    for (label, uri, headers, offset) in [
        (
            "goog1 late",
            "/bucket/object",
            header(&[
                date,
                ("authorization", "GOOG1 key:AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            ]),
            901,
        ),
        (
            "goog1 early",
            "/bucket/object",
            header(&[
                date,
                ("authorization", "GOOG1 key:AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            ]),
            -901,
        ),
        (
            "aws late",
            "/bucket/object",
            header(&[
                date,
                ("authorization", "AWS key:AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            ]),
            901,
        ),
        (
            "aws early",
            "/bucket/object",
            header(&[
                date,
                ("authorization", "AWS key:AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            ]),
            -901,
        ),
        ("v4 header", "/bucket/object", header(&v4), 86_400_i64),
        (
            "v2 query",
            "/bucket/object?GoogleAccessId=user&Expires=1175024202&Signature=AAAA",
            header(&[]),
            901,
        ),
    ] {
        let now = if offset < 0 {
            signed_at - std::time::Duration::from_secs(offset.unsigned_abs())
        } else {
            signed_at + std::time::Duration::from_secs(offset.unsigned_abs())
        };
        let uri: Uri = uri.parse().unwrap();
        let result = authenticate_target(Authentication {
            auth: &auth,
            catalog: &catalog,
            method: &Method::GET,
            uri: &uri,
            headers: &headers,
            clock: &crate::clock::ManualClock::new(now),
            resource: &crate::auth::signature::Resource::PathStyle,
            listener: crate::auth::ListenerRole::Public,
        })
        .await;
        let error = result.err().unwrap();
        let expected = if label.starts_with("v4") || label.starts_with("v2") {
            (
                StatusCode::BAD_REQUEST,
                "ExpiredToken",
                "The request signature has expired.",
            )
        } else {
            (
                StatusCode::FORBIDDEN,
                "RequestTimeTooSkewed",
                "The legacy signature timestamp is outside the allowed time window.",
            )
        };
        assert_eq!(
            (error.status(), error.code.as_str(), error.message.as_str()),
            expected,
            "{label}"
        );
    }
    catalog.close().await.unwrap();
}

/// Whether the Host or the path named the bucket decides the resource a V2 signature covers.
#[test]
fn object_paths_name_their_bucket_source() {
    let virtual_hosts = Config {
        xml_virtual_host_domains: vec!["storage.example.test".into()],
        ..Config::default()
    };
    let legacy_public = Config {
        compat_profile: crate::config::CompatProfile::FakeGcsServer,
        public_base_url: Some(
            crate::url::PublicOrigin::parse("http://storage.local:4443").unwrap(),
        ),
        ..Config::default()
    };
    // The public Host's root names the empty bucket as if the Host had chosen it.
    for (config, host, path, expected) in [
        (
            &virtual_hosts,
            "bucket.storage.example.test",
            "/object",
            "virtual \"bucket\"",
        ),
        (
            &Config::default(),
            "localhost:4443",
            "/bucket/object",
            "path style",
        ),
        (
            &legacy_public,
            "mybucket",
            "/object",
            "virtual \"mybucket\"",
        ),
        (&legacy_public, "storage.local:4443", "/", "virtual \"\""),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("host", host.parse().unwrap());
        let uri: Uri = path.parse().unwrap();
        let resource = match select_object(&uri, &headers, config) {
            Ok(Some(path)) => match path.signature_resource() {
                crate::auth::signature::Resource::PathStyle => "path style".to_owned(),
                crate::auth::signature::Resource::VirtualHosted(bucket) => {
                    format!("virtual {bucket:?}")
                }
            },
            Ok(None) => "none".to_owned(),
            Err(error) => format!("error {}", error.message),
        };
        assert_eq!(resource, expected, "{host} {path}");
    }
}
