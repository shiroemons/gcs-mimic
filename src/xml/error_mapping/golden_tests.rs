//! Pins XML status, headers and document bytes on the domain and shared request paths.

use super::super::{Error, ErrorBody};
use crate::{
    auth::Caller,
    error::golden_tests::{Transcript, http_response},
    object::name::InvalidObjectName,
    service::ServiceError,
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
}

impl Golden {
    async fn mapped<E>(
        &mut self,
        section: &str,
        cases: fn() -> Vec<Case<E>>,
        naming: Naming,
        service: Option<fn(E) -> ServiceError>,
    ) where
        Error: From<E>,
    {
        for (label, [direct, refused, named, shared, anon, by_name]) in copies(cases) {
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
            if let Some(service) = service {
                transcript.render(
                    "via ServiceError",
                    document(<Error as From<ServiceError>>::from(service(shared))).await,
                );
                transcript.render(
                    "anonymous caller, via ServiceError",
                    document(
                        <Error as From<ServiceError>>::from(service(anon)).for_caller(ANONYMOUS),
                    )
                    .await,
                );
                if naming == Naming::Object {
                    transcript.render(
                        "named object, via ServiceError",
                        document(
                            <Error as From<ServiceError>>::from(service(by_name))
                                .naming_object("bucket", "object"),
                        )
                        .await,
                    );
                }
            }
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
    };
    golden
        .mapped(
            "ObjectError",
            domain::object_cases,
            Naming::Object,
            Some(ServiceError::from),
        )
        .await;
    golden
        .mapped(
            "CatalogError",
            domain::catalog_cases,
            Naming::None,
            Some(ServiceError::from),
        )
        .await;
    golden
        .mapped(
            "AuthFailure",
            domain::auth_failure_cases,
            Naming::None,
            None,
        )
        .await;
    golden
        .mapped("AuthError", domain::auth_error_cases, Naming::None, None)
        .await;
    golden
        .mapped(
            "CsekError",
            domain::csek_cases,
            Naming::None,
            Some(ServiceError::from),
        )
        .await;
    golden
        .mapped(
            "BodyError",
            domain::body_cases,
            Naming::None,
            Some(ServiceError::from),
        )
        .await;
    golden
        .mapped(
            "TimeOutOfRange",
            domain::time_cases,
            Naming::None,
            Some(ServiceError::from),
        )
        .await;
    golden
        .mapped("InvalidDigest", domain::digest_cases, Naming::None, None)
        .await;
    golden
        .mapped(
            "InvalidProjectId",
            domain::project_cases,
            Naming::None,
            Some(ServiceError::from),
        )
        .await;
    golden
        .mapped(
            "ProfileNameError",
            domain::bucket_name_cases,
            Naming::None,
            None,
        )
        .await;
    golden
        .mapped(
            "MalformedEncoding",
            domain::encoding_cases,
            Naming::None,
            None,
        )
        .await;
    golden
        .mapped("QueryError", domain::query_cases, Naming::None, None)
        .await;
    golden
        .mapped(
            "ResumableError",
            domain::resumable_cases,
            Naming::Object,
            None,
        )
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
    for (label, error) in crate::error::golden_tests::bucket_service_errors() {
        golden.transcript.case("bucket", label);
        golden
            .transcript
            .render("response", document(Error::from(error)).await);
    }
    for (label, error) in crate::error::golden_tests::upload::cases().await {
        golden.transcript.case("upload", label);
        golden
            .transcript
            .render("response", document(Error::from(error)).await);
    }
    for &(label, value) in crate::error::golden_tests::upload::HASH_CASES {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            "x-goog-hash",
            hyper::header::HeaderValue::from_static(value),
        );
        golden.transcript.case("XML PUT", label);
        let error = crate::xml::put::content_type_and_hashes(&headers).unwrap_err();
        golden.transcript.render("response", document(error).await);
    }
    golden.transcript.assert_golden(GOLDEN);
}
