//! Pins the code, message and details of typed Storage failures, including caller-specific replies.

use super::*;
use crate::{
    auth::Caller,
    error::golden_tests::Transcript,
    grpc::v2::object_io::{named_object_status, naming_object, object_status},
    storage::failure::test_support::{self as domain, Case, copies},
};
use std::fmt::Write as _;

const GOLDEN: &str = "src/grpc/v2/status/golden_tests/statuses.txt";

const ANONYMOUS: Option<Caller<'static>> = Some(Caller::Anonymous);

const BAD_REQUEST_TYPE: &str = "type.googleapis.com/google.rpc.BadRequest";

/// A status as the golden file shows it: code, message, any metadata, and the details' bytes
/// with what they decode to.
fn rendering(status: &Status) -> String {
    let mut text = format!(
        "code: {:?} ({})\nmessage: {:?}\n",
        status.code(),
        status.code() as i32,
        status.message()
    );
    let mut metadata: Vec<String> = status
        .metadata()
        .iter()
        .map(|entry| format!("{entry:?}"))
        .collect();
    metadata.sort();
    for entry in metadata {
        let _ = writeln!(text, "metadata: {entry}");
    }
    let details = status.details();
    if details.is_empty() {
        text.push_str("details: none\n");
        return text;
    }
    let rpc = RpcStatus::decode(details).unwrap();
    let hex: String = details.iter().map(|byte| format!("{byte:02x}")).collect();
    let _ = writeln!(text, "details: {} bytes\n  hex: {hex}", details.len());
    let _ = writeln!(
        text,
        "  google.rpc.Status: code {}, message {:?}",
        rpc.code, rpc.message
    );
    for detail in &rpc.details {
        let _ = writeln!(text, "  {}", detail.type_url);
        let value = detail.value.as_slice();
        match detail.type_url.as_str() {
            BAD_REQUEST_TYPE => {
                for violation in BadRequest::decode(value).unwrap().field_violations {
                    let _ = writeln!(
                        text,
                        "    field violation: field {:?}, description {:?}, reason {:?}",
                        violation.field, violation.description, violation.reason
                    );
                }
            }
            ERROR_INFO_TYPE => {
                let info = ErrorInfo::decode(value).unwrap();
                let _ = writeln!(
                    text,
                    "    reason {:?}, domain {:?}",
                    info.reason, info.domain
                );
                for (key, value) in &info.metadata {
                    let _ = writeln!(text, "    metadata {key}: {value:?}");
                }
            }
            other => panic!("unexpected detail type {other}"),
        }
    }
    text
}

struct Golden {
    transcript: Transcript,
}

impl Golden {
    /// Object failures, which handlers also map through the helpers that name the object.
    fn objects(&mut self) {
        let named = naming_object("bucket", "object");
        let named_status = named_object_status("bucket", "object");
        for (label, [direct, by_name, plain, by_name_status, refused]) in
            copies::<ObjectError, 5>(domain::object_cases)
        {
            self.transcript.case("ObjectError", label);
            let transcript = &mut self.transcript;
            transcript.render("direct", rendering(&Status::from(direct)));
            transcript.render("named object", rendering(&named(by_name)));
            transcript.render("object status", rendering(&object_status(plain)));
            transcript.render(
                "named object status",
                rendering(&named_status(by_name_status)),
            );
            transcript.render(
                "anonymous caller",
                rendering(&anonymous(Status::from(refused), ANONYMOUS)),
            );
        }
    }

    fn direct(&mut self, section: &str, label: &str, plain: Status, refused: Status) {
        self.transcript.case(section, label);
        self.transcript.render("direct", rendering(&plain));
        self.transcript.render(
            "anonymous caller",
            rendering(&anonymous(refused, ANONYMOUS)),
        );
    }

    fn mapped<E>(&mut self, section: &str, cases: fn() -> Vec<Case<E>>, direct: fn(E) -> Status) {
        for (label, [plain, refused]) in copies(cases) {
            self.direct(section, label, direct(plain), direct(refused));
        }
    }

    fn catalog(&mut self) {
        for (label, [plain, refused, legacy, legacy_anonymous]) in copies(domain::catalog_cases) {
            self.direct("CatalogError", label, plain.into(), refused.into());
            let legacy_status = crate::grpc::v1::tests::bucket_golden_status;
            if let (Some(plain), Some(refused)) = (
                legacy_status(legacy, None),
                legacy_status(legacy_anonymous, ANONYMOUS),
            ) {
                self.replies(["legacy v1", "anonymous caller, legacy v1"], plain, refused);
            }
        }
    }

    fn replies(&mut self, labels: [&'static str; 2], plain: Status, refused: Status) {
        self.transcript.render(labels[0], rendering(&plain));
        self.transcript.render(labels[1], rendering(&refused));
    }

    fn service<E: Into<ServiceError>>(&mut self, section: &str, cases: Vec<Case<E>>) {
        for case in cases {
            self.transcript.case(section, case.label);
            let plain = Status::from(case.error.into());
            self.replies(
                ["via ServiceError", "anonymous caller, via ServiceError"],
                plain.clone(),
                anonymous(plain, ANONYMOUS),
            );
        }
    }
}

#[tokio::test]
async fn every_error_keeps_its_storage_v2_status() {
    let mut golden = Golden {
        transcript: Transcript::new(
            module_path!(),
            "every_error_keeps_its_storage_v2_status",
            "The Storage v2 status of each error: code, message and details. The details are the bytes the server sends; ErrorInfo metadata is a sorted map, so it is encoded and shown in key order.",
        ),
    };
    golden.objects();
    golden.catalog();
    golden.mapped("AuthFailure", domain::auth_failure_cases, Status::from);
    golden.mapped(
        "AuthError",
        domain::auth_error_cases,
        crate::grpc::v2::auth_error,
    );
    golden.mapped("CsekError", domain::csek_cases, Status::from);
    golden.mapped("PageTokenError", domain::page_token_cases, Status::from);
    golden.mapped("InvalidProjectId", domain::project_cases, Status::from);
    golden.mapped("ProfileNameError", domain::bucket_name_cases, Status::from);
    golden.mapped("MalformedEncoding", domain::encoding_cases, Status::from);
    golden.mapped(
        "UntestablePermission",
        domain::untestable_permission_cases,
        Status::from,
    );
    golden.mapped("PolicyInputError", domain::policy_input_cases, Status::from);
    golden.mapped(
        "ListQueryError",
        domain::list_query_cases,
        list_query_status,
    );
    golden.mapped("GlobError", domain::glob_cases, |error| glob_status(&error));
    golden.service(
        "ResumableError",
        domain::resumable_cases()
            .into_iter()
            .filter(|case| {
                !matches!(
                    case.error,
                    crate::storage::ResumableError::NotFound
                        | crate::storage::ResumableError::Cancelled
                        | crate::storage::ResumableError::Gone
                )
            })
            .collect(),
    );
    for (label, write, query) in crate::grpc::v2::object_io::tests::ended_session_replies() {
        golden.transcript.case("ResumableError", label);
        golden.transcript.render("WriteObject", rendering(&write));
        golden
            .transcript
            .render("QueryWriteStatus", rendering(&query));
    }
    golden.service("BodyError", domain::body_cases());
    golden.service("TimeOutOfRange", domain::time_cases());
    for (label, error) in crate::error::golden_tests::bucket_service_errors() {
        golden.transcript.case("bucket", label);
        golden
            .transcript
            .render("response", rendering(&Status::from(error)));
    }
    for (label, error) in crate::error::golden_tests::upload::cases()
        .await
        .into_iter()
        .chain(crate::error::golden_tests::upload::hash_cases())
    {
        golden.transcript.case("upload", label);
        golden
            .transcript
            .render("response", rendering(&Status::from(error)));
    }
    golden.transcript.assert_golden(GOLDEN);
}
