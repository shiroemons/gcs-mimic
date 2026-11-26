//! Pins the XML API's answer to every error the shared error is built from: the status, each
//! header in the order it is sent and the exact document. A domain error the XML adapter maps
//! itself is rendered both directly and through the shared error, the old mapping the direct
//! ones replaced, and each path again as an anonymous caller and, for an object failure, with
//! the requested object named. See [`crate::api_error::test_support`] for regenerating the
//! golden file.

use super::super::{Error, ErrorBody};
use crate::{
    api_error::{
        ApiError,
        test_support::{KindCoverage, Transcript, constructor_cases, http_response},
    },
    auth::Caller,
    object::name::InvalidObjectName,
    server::anonymous_error,
    storage::failure::test_support::{self as domain, Case, case, copies, every_variant},
};

const GOLDEN: &str = "src/xml/error_mapping/golden_tests/responses.txt";

const ANONYMOUS: Option<Caller<'static>> = Some(Caller::Anonymous);

async fn document(error: Error) -> String {
    http_response(error.response(ErrorBody::Include)).await
}

/// Whether the routes that answer an error name the object they act on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Naming {
    Object,
    None,
}

struct Golden {
    transcript: Transcript,
    kinds: KindCoverage,
}

impl Golden {
    /// Errors the XML adapter maps itself and through the shared error.
    async fn mapped<E>(&mut self, section: &str, cases: fn() -> Vec<Case<E>>, naming: Naming)
    where
        Error: From<E>,
        ApiError: From<E>,
    {
        for (label, [direct, refused, named, shared]) in copies::<E, 4>(cases) {
            self.transcript.case(section, label);
            let transcript = &mut self.transcript;
            transcript.render("direct", document(Error::from(direct)).await);
            transcript.render(
                "anonymous caller",
                document(Error::from(refused).for_caller(ANONYMOUS)).await,
            );
            if naming == Naming::Object {
                transcript.render(
                    "named object",
                    document(Error::from(named).naming_object("bucket", "object")).await,
                );
            }
            self.shared_documents(ApiError::from(shared), naming).await;
        }
    }

    /// Errors that reach the XML adapter only as the shared error.
    async fn shared<E>(&mut self, section: &str, cases: Vec<Case<E>>, naming: Naming)
    where
        ApiError: From<E>,
    {
        for case in cases {
            self.transcript.case(section, case.label);
            self.shared_documents(ApiError::from(case.error), naming)
                .await;
        }
    }

    /// The shared error's documents, as the XML handlers that convert it send them.
    async fn shared_documents(&mut self, shared: ApiError, naming: Naming) {
        self.kinds.record(&shared);
        let transcript = &mut self.transcript;
        transcript.render("via ApiError", document(Error::from(shared.clone())).await);
        transcript.render(
            "anonymous caller, via ApiError",
            document(Error::from(anonymous_error(shared.clone(), ANONYMOUS))).await,
        );
        if naming == Naming::Object {
            transcript.render(
                "named object, via ApiError",
                document(Error::from(shared).naming_object("bucket", "object")).await,
            );
        }
    }

    /// Errors only the XML adapter maps.
    async fn direct<E>(&mut self, section: &str, cases: Vec<Case<E>>)
    where
        Error: From<E>,
    {
        for case in cases {
            self.transcript.case(section, case.label);
            self.transcript
                .render("direct", document(Error::from(case.error)).await);
        }
    }

    /// A HEAD request's answer, which keeps the length of the document it leaves out.
    async fn head(&mut self, label: &str, error: Error) {
        self.transcript.case("HEAD", label);
        self.transcript.render(
            "response",
            http_response(error.response(ErrorBody::Omit)).await,
        );
    }
}

fn object_name_cases() -> Vec<Case<InvalidObjectName>> {
    every_variant(
        vec![
            case("Empty", InvalidObjectName::Empty),
            case("TooLong", InvalidObjectName::TooLong),
            case("LineBreak", InvalidObjectName::LineBreak),
            case("DotSegment", InvalidObjectName::DotSegment),
            case("AcmeChallenge", InvalidObjectName::AcmeChallenge),
        ],
        |error| match error {
            InvalidObjectName::Empty => "Empty",
            InvalidObjectName::TooLong => "TooLong",
            InvalidObjectName::LineBreak => "LineBreak",
            InvalidObjectName::DotSegment => "DotSegment",
            InvalidObjectName::AcmeChallenge => "AcmeChallenge",
        },
        &[
            "Empty",
            "TooLong",
            "LineBreak",
            "DotSegment",
            "AcmeChallenge",
        ],
    )
}

#[tokio::test]
async fn every_error_keeps_its_xml_response() {
    let mut golden = Golden {
        transcript: Transcript::new(
            module_path!(),
            "every_error_keeps_its_xml_response",
            "The XML API's answer to each error: status, headers in the order they are sent, and the document's bytes.",
        ),
        kinds: KindCoverage::default(),
    };
    golden
        .mapped("ObjectError", domain::object_cases, Naming::Object)
        .await;
    golden
        .mapped("CatalogError", domain::catalog_cases, Naming::None)
        .await;
    golden
        .mapped("AuthFailure", domain::auth_failure_cases, Naming::None)
        .await;
    golden
        .mapped("AuthError", domain::auth_error_cases, Naming::None)
        .await;
    golden
        .mapped("CsekError", domain::csek_cases, Naming::None)
        .await;
    golden
        .mapped("BodyError", domain::body_cases, Naming::None)
        .await;
    golden
        .mapped("TimeOutOfRange", domain::time_cases, Naming::None)
        .await;
    golden
        .mapped("InvalidDigest", domain::digest_cases, Naming::None)
        .await;
    golden
        .mapped("InvalidProjectId", domain::project_cases, Naming::None)
        .await;
    golden
        .mapped("ProfileNameError", domain::bucket_name_cases, Naming::None)
        .await;
    golden
        .mapped("MalformedEncoding", domain::encoding_cases, Naming::None)
        .await;
    golden
        .mapped("QueryError", domain::query_cases, Naming::None)
        .await;
    golden
        .shared("ResumableError", domain::resumable_cases(), Naming::Object)
        .await;
    golden
        .shared("PageTokenError", domain::page_token_cases(), Naming::None)
        .await;
    golden
        .shared(
            "ConflictingAcl",
            domain::conflicting_acl_cases(),
            Naming::None,
        )
        .await;
    golden
        .shared("GlobError", domain::glob_cases(), Naming::None)
        .await;
    golden
        .shared("UrlError", domain::url_cases(), Naming::None)
        .await;
    golden
        .shared("ListQueryError", domain::list_query_cases(), Naming::None)
        .await;
    golden
        .shared(
            "UntestablePermission",
            domain::untestable_permission_cases(),
            Naming::None,
        )
        .await;
    golden
        .shared(
            "PolicyInputError",
            domain::policy_input_cases(),
            Naming::None,
        )
        .await;
    golden
        .shared("ApiError", constructor_cases(), Naming::Object)
        .await;
    golden
        .direct("InvalidObjectName", object_name_cases())
        .await;
    golden
        .head(
            "named missing object",
            Error::from(crate::object::ObjectError::ObjectNotFound)
                .naming_object("bucket", "object"),
        )
        .await;
    golden
        .head(
            "precondition failed",
            Error::from(crate::object::ObjectError::PreconditionFailed),
        )
        .await;
    golden
        .head(
            "access denied",
            Error::from(crate::object::ObjectError::Denied(domain::denied(Some(
                domain::VIEWER,
            )))),
        )
        .await;
    for (label, error) in crate::api_error::test_support::bucket_service_errors() {
        golden.transcript.case("bucket", label);
        golden
            .transcript
            .render("response", document(Error::from(error)).await);
    }
    golden.kinds.assert_complete();
    golden.transcript.assert_golden(GOLDEN);
}
