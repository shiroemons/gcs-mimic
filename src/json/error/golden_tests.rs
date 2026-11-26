//! Pins the JSON API's answer to every error the shared error is built from: the status, each
//! header in the order it is sent and the exact body. An error is also rendered as each route
//! that rewords it sends it: an object route names its object and a media download answers some
//! errors with an HTML page, an anonymous caller gets 401 for a refusal, and the fake-gcs-server
//! bucket routes change reasons, messages and the body's framing.
//!
//! The domain cases and the constructors are shared with the XML and Storage v2 goldens; the
//! conversions that live in the JSON adapter add their cases here. See
//! [`crate::api_error::test_support`] for regenerating the golden file.

use super::{JsonErrorResponse, parse_error};
use crate::{
    api_error::{
        ApiError,
        test_support::{KindCoverage, Transcript, constructor_cases, http_response},
    },
    auth::Caller,
    config::CompatProfile,
    json::{ResponseFormat, with_format},
    server::{anonymous_error, bucket_answer},
    storage::failure::test_support::{self as domain, Case, case, every_variant},
};

const GOLDEN: &str = "src/json/error/golden_tests/responses.txt";

/// Which routes reword an error, and so which renderings it gets besides the plain response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Routes {
    /// Only the plain response.
    Plain,
    /// An object route, which names its object, and a media download of it.
    Object,
    /// A bucket route, which an anonymous caller and the fake-gcs-server profile reach.
    Bucket,
    /// Any route an anonymous caller reaches.
    Anonymous,
    /// Every rewording, for the constructors.
    All,
}

struct Golden {
    transcript: Transcript,
    kinds: KindCoverage,
}

impl Golden {
    async fn section<E>(&mut self, section: &str, cases: Vec<Case<E>>, routes: Routes)
    where
        ApiError: From<E>,
    {
        for case in cases {
            self.transcript.case(section, case.label);
            let error = ApiError::from(case.error);
            self.kinds.record(&error);
            self.render(error, routes).await;
        }
    }

    async fn render(&mut self, error: ApiError, routes: Routes) {
        let transcript = &mut self.transcript;
        transcript.render(
            "response",
            http_response(error.clone().into_response()).await,
        );
        let anonymous = || anonymous_error(error.clone(), Some(Caller::Anonymous));
        if routes == Routes::Object {
            let named = || error.clone().naming_object("bucket", "object");
            transcript.render("named object", http_response(named().into_response()).await);
            transcript.render(
                "media download",
                http_response(named().into_media_response()).await,
            );
            transcript.render(
                "anonymous caller",
                http_response(
                    anonymous()
                        .naming_object("bucket", "object")
                        .into_response(),
                )
                .await,
            );
        }
        if routes == Routes::All {
            transcript.render(
                "media download",
                http_response(error.clone().into_media_response()).await,
            );
        }
        if matches!(routes, Routes::Bucket | Routes::Anonymous | Routes::All) {
            transcript.render(
                "anonymous caller",
                http_response(anonymous().into_response()).await,
            );
        }
        if matches!(routes, Routes::Bucket | Routes::All) {
            let profile = CompatProfile::FakeGcsServer;
            let response = with_format(
                ResponseFormat::for_request(None, profile),
                bucket_answer(Err(error), profile),
            )
            .await;
            transcript.render(
                "fake-gcs-server bucket route",
                http_response(response).await,
            );
        }
    }
}

fn acl_cases() -> Vec<Case<crate::acl::AclError>> {
    use crate::acl::{AclError, AclKind};
    every_variant(
        vec![
            case("TooManyEntries", AclError::TooManyEntries),
            case(
                "DuplicateEntity",
                AclError::DuplicateEntity("user-viewer@example.invalid".into()),
            ),
            case("InvalidEntity", AclError::InvalidEntity("everyone".into())),
            case("EntityTooLong", AclError::EntityTooLong),
            case(
                "InvalidRole",
                AclError::InvalidRole {
                    kind: AclKind::Object,
                    role: "WRITER".into(),
                },
            ),
        ],
        |error| match error {
            AclError::TooManyEntries => "TooManyEntries",
            AclError::DuplicateEntity(_) => "DuplicateEntity",
            AclError::InvalidEntity(_) => "InvalidEntity",
            AclError::EntityTooLong => "EntityTooLong",
            AclError::InvalidRole { .. } => "InvalidRole",
        },
        &[
            "TooManyEntries",
            "DuplicateEntity",
            "InvalidEntity",
            "EntityTooLong",
            "InvalidRole",
        ],
    )
}

fn hmac_key_cases() -> Vec<Case<crate::storage::hmac_keys::HmacKeyError>> {
    use crate::storage::hmac_keys::HmacKeyError;
    every_variant(
        vec![
            case("NotFound", HmacKeyError::NotFound),
            case("PreconditionFailed", HmacKeyError::PreconditionFailed),
            case("DeleteActive", HmacKeyError::DeleteActive),
            case("Deleted", HmacKeyError::Deleted),
            case("LimitReached", HmacKeyError::LimitReached),
            case(
                "Catalog(NotFound)",
                HmacKeyError::Catalog(crate::bucket::CatalogError::NotFound),
            ),
        ],
        |error| match error {
            HmacKeyError::NotFound => "NotFound",
            HmacKeyError::PreconditionFailed => "PreconditionFailed",
            HmacKeyError::DeleteActive => "DeleteActive",
            HmacKeyError::Deleted => "Deleted",
            HmacKeyError::LimitReached => "LimitReached",
            HmacKeyError::Catalog(_) => "Catalog",
        },
        &[
            "NotFound",
            "PreconditionFailed",
            "DeleteActive",
            "Deleted",
            "LimitReached",
            "Catalog",
        ],
    )
}

fn notification_cases() -> Vec<Case<crate::notification::NotificationError>> {
    use crate::notification::{Event, NotificationError};
    every_variant(
        vec![
            case(
                "InvalidTopic",
                NotificationError::InvalidTopic("projects/p/topics/t".into()),
            ),
            case(
                "InvalidPayloadFormat",
                NotificationError::InvalidPayloadFormat("XML".into()),
            ),
            case(
                "InvalidEventType",
                NotificationError::InvalidEventType("OBJECT_READ".into()),
            ),
            case(
                "DuplicateEventType",
                NotificationError::DuplicateEventType(Event::Finalize),
            ),
            case(
                "TooManyCustomAttributes",
                NotificationError::TooManyCustomAttributes(11),
            ),
            case(
                "InvalidCustomAttributeKey",
                NotificationError::InvalidCustomAttributeKey("goog-key".into()),
            ),
            case(
                "CustomAttributeTooLong",
                NotificationError::CustomAttributeTooLong("long".into()),
            ),
            case(
                "ObjectNamePrefixTooLong",
                NotificationError::ObjectNamePrefixTooLong,
            ),
            case("BucketLimit", NotificationError::BucketLimit),
            case(
                "EventTypeLimit",
                NotificationError::EventTypeLimit(Event::Delete),
            ),
        ],
        |error| match error {
            NotificationError::InvalidTopic(_) => "InvalidTopic",
            NotificationError::InvalidPayloadFormat(_) => "InvalidPayloadFormat",
            NotificationError::InvalidEventType(_) => "InvalidEventType",
            NotificationError::DuplicateEventType(_) => "DuplicateEventType",
            NotificationError::TooManyCustomAttributes(_) => "TooManyCustomAttributes",
            NotificationError::InvalidCustomAttributeKey(_) => "InvalidCustomAttributeKey",
            NotificationError::CustomAttributeTooLong(_) => "CustomAttributeTooLong",
            NotificationError::ObjectNamePrefixTooLong => "ObjectNamePrefixTooLong",
            NotificationError::BucketLimit => "BucketLimit",
            NotificationError::EventTypeLimit(_) => "EventTypeLimit",
        },
        &[
            "InvalidTopic",
            "InvalidPayloadFormat",
            "InvalidEventType",
            "DuplicateEventType",
            "TooManyCustomAttributes",
            "InvalidCustomAttributeKey",
            "CustomAttributeTooLong",
            "ObjectNamePrefixTooLong",
            "BucketLimit",
            "EventTypeLimit",
        ],
    )
}

fn endpoint_cases() -> Vec<Case<crate::config::EndpointError>> {
    use crate::config::EndpointError;
    every_variant(
        vec![
            case(
                "Url(InvalidBaseUrl)",
                EndpointError::Url(crate::url::UrlError::InvalidBaseUrl),
            ),
            case("PublicHost", EndpointError::PublicHost),
            case("DomainLimit", EndpointError::DomainLimit),
            case("ApiVirtualHost", EndpointError::ApiVirtualHost),
            case("Poisoned", EndpointError::Poisoned),
        ],
        |error| match error {
            EndpointError::Url(_) => "Url",
            EndpointError::PublicHost => "PublicHost",
            EndpointError::DomainLimit => "DomainLimit",
            EndpointError::ApiVirtualHost => "ApiVirtualHost",
            EndpointError::Poisoned => "Poisoned",
        },
        &[
            "Url",
            "PublicHost",
            "DomainLimit",
            "ApiVirtualHost",
            "Poisoned",
        ],
    )
}

fn range_cases() -> Vec<Case<crate::json::objects::UnsatisfiableRange>> {
    vec![case(
        "UnsatisfiableRange",
        crate::json::objects::UnsatisfiableRange,
    )]
}

/// The bodies Cloud Storage's parser stopped at in each way `parse_error` words.
fn parse_error_cases() -> Vec<Case<ApiError>> {
    let parse = |input: &str| {
        let cause = serde_json::from_str::<serde_json::Value>(input).unwrap_err();
        parse_error(input.as_bytes(), &cause)
    };
    vec![
        case("missing colon", parse("{not json")),
        case("missing colon at the end", parse("{oops")),
        case("missing key", parse("{ ")),
        case("truncated value", parse("{\"name\":")),
        case("other syntax error", parse("[1,]")),
    ]
}

#[tokio::test]
async fn every_error_keeps_its_json_response() {
    let mut golden = Golden {
        transcript: Transcript::new(
            module_path!(),
            "every_error_keeps_its_json_response",
            "The JSON API's answer to each error: status, headers in the order they are sent, and the body's bytes.",
        ),
        kinds: KindCoverage::default(),
    };
    golden
        .section("ObjectError", domain::object_cases(), Routes::Object)
        .await;
    golden
        .section("CatalogError", domain::catalog_cases(), Routes::Bucket)
        .await;
    golden
        .section(
            "AuthFailure",
            domain::auth_failure_cases(),
            Routes::Anonymous,
        )
        .await;
    golden
        .section("AuthError", domain::auth_error_cases(), Routes::Anonymous)
        .await;
    golden
        .section("CsekError", domain::csek_cases(), Routes::Plain)
        .await;
    golden
        .section("ResumableError", domain::resumable_cases(), Routes::Plain)
        .await;
    golden
        .section("BodyError", domain::body_cases(), Routes::Plain)
        .await;
    golden
        .section("PageTokenError", domain::page_token_cases(), Routes::Plain)
        .await;
    golden
        .section(
            "ProfileNameError",
            domain::bucket_name_cases(),
            Routes::Bucket,
        )
        .await;
    golden
        .section("QueryError", domain::query_cases(), Routes::Plain)
        .await;
    golden
        .section("MalformedEncoding", domain::encoding_cases(), Routes::Plain)
        .await;
    golden
        .section("InvalidDigest", domain::digest_cases(), Routes::Plain)
        .await;
    golden
        .section("TimeOutOfRange", domain::time_cases(), Routes::Plain)
        .await;
    golden
        .section("InvalidProjectId", domain::project_cases(), Routes::Plain)
        .await;
    golden
        .section(
            "ConflictingAcl",
            domain::conflicting_acl_cases(),
            Routes::Plain,
        )
        .await;
    golden
        .section("GlobError", domain::glob_cases(), Routes::Plain)
        .await;
    golden
        .section("UrlError", domain::url_cases(), Routes::Plain)
        .await;
    golden
        .section("ListQueryError", domain::list_query_cases(), Routes::Plain)
        .await;
    golden
        .section(
            "UntestablePermission",
            domain::untestable_permission_cases(),
            Routes::Plain,
        )
        .await;
    golden
        .section(
            "PolicyInputError",
            domain::policy_input_cases(),
            Routes::Plain,
        )
        .await;
    golden.section("AclError", acl_cases(), Routes::Plain).await;
    golden
        .section("HmacKeyError", hmac_key_cases(), Routes::Plain)
        .await;
    golden
        .section("NotificationError", notification_cases(), Routes::Plain)
        .await;
    golden
        .section("EndpointError", endpoint_cases(), Routes::Plain)
        .await;
    golden
        .section("UnsatisfiableRange", range_cases(), Routes::Object)
        .await;
    golden
        .section("parse_error", parse_error_cases(), Routes::Plain)
        .await;
    golden
        .section("ApiError", constructor_cases(), Routes::All)
        .await;
    for (label, error) in crate::api_error::test_support::bucket_service_errors() {
        golden.transcript.case("bucket", label);
        golden.transcript.render(
            "response",
            http_response(ApiError::from(error).into_response()).await,
        );
    }
    golden.kinds.assert_complete();
    golden.transcript.assert_golden(GOLDEN);
}
