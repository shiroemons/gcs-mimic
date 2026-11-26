//! Every constructor and modifier of the shared error, and the golden transcripts in which the
//! adapters' tests pin their answers to it and to the domain errors.
//!
//! A golden file changes only on request: a test run with `GCSM_UPDATE_GOLDENS=1` rewrites each
//! file that differs and then fails, so a regenerated file is never taken for a passing check.
//! Review the diff, then run the tests again without the variable.

use super::*;
use crate::storage::failure::{
    BackendFailure,
    test_support::{Case, VIEWER, case, class_location, denied},
};
use http_body_util::{BodyExt, Full};
use hyper::{Response, body::Bytes};
use std::{collections::BTreeSet, fmt::Write as _, path::Path};

/// The variable that asks the golden tests to rewrite the files they compare.
pub(crate) const UPDATE_VARIABLE: &str = "GCSM_UPDATE_GOLDENS";

/// Every constructor and modifier, each with the input it takes in production where one call
/// site gives it a fixed text, and a status for each arm the adapters map without a kind.
pub(crate) fn constructor_cases() -> Vec<Case<ApiError>> {
    let status = |code| StatusCode::from_u16(code).unwrap();
    vec![
        case(
            "new(401 unauthorized)",
            ApiError::new(status(401), "unauthorized", "Credentials rejected"),
        ),
        case(
            "new(404 notFound)",
            ApiError::new(status(404), "notFound", "Notification not found"),
        ),
        case(
            "new(408 requestTimeout)",
            ApiError::new(status(408), "requestTimeout", "The upload stalled"),
        ),
        case(
            "new(410 gone)",
            ApiError::new(status(410), "gone", "Upload session is no longer active"),
        ),
        case(
            "new(413 uploadTooLarge)",
            ApiError::new(status(413), "uploadTooLarge", "Upload exceeds 1 MiB"),
        ),
        case(
            "new(416 requestedRangeNotSatisfiable)",
            ApiError::new(
                status(416),
                "requestedRangeNotSatisfiable",
                "The requested Range cannot be satisfied",
            ),
        ),
        case(
            "new(429 rateLimitExceeded)",
            ApiError::new(status(429), "rateLimitExceeded", "Too many requests"),
        ),
        case(
            "new(500 internalError)",
            ApiError::new(status(500), "internalError", "Internal error"),
        ),
        case(
            "new(501 notImplemented)",
            ApiError::new(status(501), "notImplemented", "Unsupported object option"),
        ),
        case(
            "new(504 deadlineExceeded)",
            ApiError::new(status(504), "deadlineExceeded", "Deadline exceeded"),
        ),
        case(
            "backend(CatalogClosed)",
            ApiError::backend(&BackendFailure::CatalogClosed.into()),
        ),
        case(
            "catalog_backend(CatalogClosed)",
            ApiError::catalog_backend(&BackendFailure::CatalogClosed.into()),
        ),
        case(
            "metadata(ClassLocation)",
            ApiError::metadata(&class_location(), ApiError::unsupported),
        ),
        case(
            "metadata(StorageClassRequiresZone)",
            ApiError::metadata(
                &crate::metadata_error::MetadataError::StorageClassRequiresZone,
                ApiError::unsupported_object,
            ),
        ),
        case(
            "metadata(LongLanguage)",
            ApiError::metadata(
                &crate::metadata_error::MetadataError::LongLanguage,
                ApiError::unsupported_object,
            ),
        ),
        case("missing_bucket()", ApiError::missing_bucket()),
        case("missing_object()", ApiError::missing_object()),
        case(
            "named_missing_object()",
            ApiError::named_missing_object("bucket", "object"),
        ),
        case("precondition_not_held()", ApiError::precondition_not_held()),
        case("not_modified()", ApiError::not_modified()),
        case(
            "conflicting_preconditions()",
            ApiError::conflicting_preconditions(),
        ),
        case("write_not_modified()", ApiError::write_not_modified()),
        case(
            "malformed_json()",
            ApiError::malformed_json(
                "Parse Error: Expected : between key:value pair.\n{not json\n     ^",
            ),
        ),
        case(
            "legacy_parse_error()",
            ApiError::legacy_parse_error("invalid character 'x' looking for beginning of value"),
        ),
        case(
            "unsupported_message()",
            ApiError::unsupported_message("gcs-mimic does not support this setting; omit it."),
        ),
        case(
            "field_mask_syntax()",
            ApiError::field_mask_syntax("Invalid field selection items("),
        ),
        case(
            "invalid_parameter()",
            ApiError::invalid_parameter("Invalid argument"),
        ),
        case(
            "invalid_parameter().at_parameter(listed)",
            ApiError::invalid_long("x").at_parameter("generation"),
        ),
        case(
            "invalid_parameter().at_parameter(unlisted)",
            ApiError::invalid_long("x").at_parameter("notAParameter"),
        ),
        case(
            "invalid().at(POLICY_ETAG_FIELD)",
            ApiError::invalid("Invalid value for ByteString: CAF").at(&POLICY_ETAG_FIELD),
        ),
        case(
            "precondition_not_held().at_if_match()",
            ApiError::precondition_not_held().at_if_match(),
        ),
        case(
            "new(412).at_if_match()",
            ApiError::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "bucket must be empty prior to deletion",
            )
            .at_if_match(),
        ),
        case(
            "new(404).with_plain(NOT_FOUND_HTML)",
            ApiError::new(StatusCode::NOT_FOUND, "notFound", "Not Found")
                .with_plain(&NOT_FOUND_HTML),
        ),
        case(
            "new(404).with_plain(NOT_FOUND_TEXT)",
            ApiError::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Upload session not found",
            )
            .with_plain(&NOT_FOUND_TEXT),
        ),
        case(
            "invalid_parameter().with_plain(BAD_CONTENT_TYPE_TEXT)",
            ApiError::invalid_parameter("multipart/related requires a boundary")
                .with_plain(&BAD_CONTENT_TYPE_TEXT),
        ),
        case(
            "invalid_parameter().with_plain(CONTENT_RANGE_TEXT)",
            ApiError::invalid_parameter("Failed to parse Content-Range header.")
                .with_plain(&CONTENT_RANGE_TEXT),
        ),
        case(
            "invalid_parameter().with_plain(MISSING_END_BOUNDARY_TEXT)",
            ApiError::invalid_parameter("Missing end boundary in multipart body.")
                .with_plain(&MISSING_END_BOUNDARY_TEXT),
        ),
        case(
            "invalid_parameter().with_plain(BAD_REQUEST_TEXT)",
            ApiError::invalid_parameter("uploadType must be media, multipart or resumable")
                .with_plain(&BAD_REQUEST_TEXT),
        ),
        case("undecodable_body()", ApiError::undecodable_body()),
        case(
            "session_protocol()",
            ApiError::session_protocol("Upload offset 7 does not match the persisted offset 5"),
        ),
        case(
            "content_disagrees()",
            ApiError::content_disagrees("other", "bucket"),
        ),
        case("required()", ApiError::required("Required")),
        case(
            "method_not_allowed(with Allow)",
            ApiError::method_not_allowed(
                "HTTP method is not supported for this object endpoint",
                Some("GET, PATCH, PUT, DELETE"),
            ),
        ),
        case(
            "method_not_allowed(without Allow)",
            ApiError::method_not_allowed("Use POST to lock a retention policy", None),
        ),
        case(
            "service_unavailable()",
            ApiError::service_unavailable(
                "Runtime endpoint state is unavailable; restart the emulator",
            ),
        ),
        case("invalid_stored_header()", ApiError::invalid_stored_header()),
        case("invalid_long()", ApiError::invalid_long("abc")),
        case("invalid_unsigned()", ApiError::invalid_unsigned("-1")),
        // Markup in the message shows the escaping of an envelope labelled as HTML.
        case(
            "invalid().html_typed()",
            ApiError::invalid("Value <b>a&b</b> 'x' is \"invalid\"").html_typed(),
        ),
        case("missing_object_name()", ApiError::missing_object_name()),
        case(
            "for_upload(class location)",
            ApiError::metadata(&class_location(), ApiError::unsupported).for_upload(),
        ),
        case(
            "for_upload(other)",
            ApiError::invalid("Invalid storage class").for_upload(),
        ),
        case(
            "invalid()",
            ApiError::invalid("Invalid value for: x is not a valid value"),
        ),
        case(
            "invalid_argument()",
            ApiError::invalid_argument("sourceObjects must not be empty"),
        ),
        case("forbidden()", ApiError::forbidden()),
        case(
            "for_anonymous_caller(anonymous denial)",
            ApiError::from(denied(None)).for_anonymous_caller(),
        ),
        case(
            "for_anonymous_caller(principal denial)",
            ApiError::from(denied(Some(VIEWER))).for_anonymous_caller(),
        ),
        case(
            "for_anonymous_caller(forbidden)",
            ApiError::forbidden().for_anonymous_caller(),
        ),
        case(
            "for_anonymous_caller(not found)",
            ApiError::missing_object().for_anonymous_caller(),
        ),
        case("unsupported()", ApiError::unsupported("rpo")),
        case(
            "unsupported_object()",
            ApiError::unsupported_object("metadata"),
        ),
        case(
            "naming_object(missing object)",
            ApiError::missing_object().naming_object("bucket", "object"),
        ),
        // Markup in the name shows the escaping of the media download's HTML page.
        case(
            "naming_object(markup in the name)",
            ApiError::missing_object().naming_object("bucket", "<a href=\"x\">&'</a>"),
        ),
        case(
            "naming_object(already named)",
            ApiError::named_missing_object("bucket", "part").naming_object("bucket", "object"),
        ),
        case(
            "naming_object(other error)",
            ApiError::forbidden().naming_object("bucket", "object"),
        ),
        case("cancelled_upload()", ApiError::cancelled_upload()),
    ]
}

/// Names each kind. A new kind fails to compile here until it joins [`KINDS`]; the golden tests
/// then fail until a case reaches it.
fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Backend(_) => "Backend",
        ErrorKind::MissingBucket => "MissingBucket",
        ErrorKind::MissingObject => "MissingObject",
        ErrorKind::NamedMissingObject => "NamedMissingObject",
        ErrorKind::Required => "Required",
        ErrorKind::RetentionNotMet => "RetentionNotMet",
        ErrorKind::ActiveHold => "ActiveHold",
        ErrorKind::Csek(_) => "Csek",
        ErrorKind::Unsupported => "Unsupported",
        ErrorKind::CancelledUpload => "CancelledUpload",
        ErrorKind::IncompleteBody => "IncompleteBody",
        ErrorKind::InvalidArgument => "InvalidArgument",
        ErrorKind::UniformAccessAcl => "UniformAccessAcl",
        ErrorKind::ClassLocation => "ClassLocation",
        ErrorKind::LegacyParseError => "LegacyParseError",
        ErrorKind::MalformedJson => "MalformedJson",
        ErrorKind::FieldMaskSyntax => "FieldMaskSyntax",
        ErrorKind::PreconditionFailed => "PreconditionFailed",
        ErrorKind::NotModified => "NotModified",
        ErrorKind::WriteNotModified => "WriteNotModified",
        ErrorKind::Other => "Other",
    }
}

const KINDS: [&str; 21] = [
    "Backend",
    "MissingBucket",
    "MissingObject",
    "NamedMissingObject",
    "Required",
    "RetentionNotMet",
    "ActiveHold",
    "Csek",
    "Unsupported",
    "CancelledUpload",
    "IncompleteBody",
    "InvalidArgument",
    "UniformAccessAcl",
    "ClassLocation",
    "LegacyParseError",
    "MalformedJson",
    "FieldMaskSyntax",
    "PreconditionFailed",
    "NotModified",
    "WriteNotModified",
    "Other",
];

/// The kinds of the errors a golden test rendered, so the test can show it reached them all.
#[derive(Default)]
pub(crate) struct KindCoverage(BTreeSet<&'static str>);

impl KindCoverage {
    pub(crate) fn record(&mut self, error: &ApiError) {
        self.0.insert(kind_name(error.kind));
    }

    pub(crate) fn assert_complete(&self) {
        let missing: Vec<&str> = KINDS
            .iter()
            .copied()
            .filter(|kind| !self.0.contains(kind))
            .collect();
        assert!(missing.is_empty(), "no case reaches the kinds {missing:?}");
    }
}

/// A golden transcript: each case's label and its renderings, in the order the test adds them.
pub(crate) struct Transcript {
    text: String,
    labels: BTreeSet<String>,
    /// The renderings of the current case, so that a repeat names the earlier one.
    renderings: Vec<(&'static str, String)>,
}

impl Transcript {
    /// Starts a transcript whose header says what it pins and which test writes it. `module` is
    /// the test's `module_path!()`.
    pub(crate) fn new(module: &str, test: &str, pins: &str) -> Self {
        let module = module.split_once("::").map_or(module, |(_, path)| path);
        Self {
            text: format!(
                "# {pins}\n# Written by {module}::{test}. Regenerate with {UPDATE_VARIABLE}=1 cargo test --lib {module},\n# which rewrites this file and fails; review the diff and run the test again without it.\n"
            ),
            labels: BTreeSet::new(),
            renderings: Vec::new(),
        }
    }

    pub(crate) fn case(&mut self, section: &str, label: &str) {
        let label = format!("{section}::{label}");
        assert!(self.labels.insert(label.clone()), "{label} is listed twice");
        self.renderings.clear();
        let _ = write!(self.text, "\n== {label}\n");
    }

    /// Adds a rendering of the current case. One equal to an earlier rendering of the case names
    /// that rendering instead of repeating it, so a difference between two paths stands out.
    pub(crate) fn render(&mut self, name: &'static str, rendering: String) {
        assert!(rendering.ends_with('\n'), "{name}: {rendering:?}");
        let earlier = self
            .renderings
            .iter()
            .find(|(_, earlier)| *earlier == rendering);
        if let Some((earlier, _)) = earlier {
            let _ = writeln!(self.text, "-- {name}: same as {earlier}");
        } else {
            let _ = writeln!(self.text, "-- {name}");
            self.text.push_str(&rendering);
        }
        self.renderings.push((name, rendering));
    }

    /// Compares the transcript with the golden file at `path`, relative to the crate root.
    pub(crate) fn assert_golden(&self, path: &str) {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let golden = match std::fs::read_to_string(&file) {
            Ok(golden) => golden,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("{path}: {error}"),
        };
        if golden == self.text {
            return;
        }
        if std::env::var(UPDATE_VARIABLE).as_deref() == Ok("1") {
            std::fs::write(&file, &self.text).unwrap();
            panic!(
                "rewrote {path}; review its diff, then run the test again without {UPDATE_VARIABLE}"
            );
        }
        panic!("{}", first_difference(path, &golden, &self.text));
    }
}

/// The first line where the golden file and the transcript differ, with the case it belongs to.
fn first_difference(path: &str, golden: &str, actual: &str) -> String {
    let advice = format!(
        "If the new answers are intended, run the test with {UPDATE_VARIABLE}=1, which rewrites the file, and review its diff."
    );
    let mut case = "the header";
    let (mut golden_lines, mut actual_lines) = (golden.lines(), actual.lines());
    for number in 1.. {
        let (expected, found) = (golden_lines.next(), actual_lines.next());
        if expected != found {
            return format!(
                "{path} differs at line {number}, in {case}:\n  golden: {expected:?}\n  actual: {found:?}\n{advice}"
            );
        }
        let Some(found) = found else { break };
        if let Some(label) = found.strip_prefix("== ") {
            case = label;
        }
    }
    format!(
        "{path} differs only in its line endings; .gitattributes must check it out with LF. {advice}"
    )
}

/// An HTTP answer as the golden files show it: the status line, every header in the order it is
/// sent, and the body's exact text.
pub(crate) async fn http_response(response: Response<Full<Bytes>>) -> String {
    let (parts, body) = response.into_parts();
    let Ok(collected) = body.collect().await;
    let bytes = collected.to_bytes();
    let mut text = format!("{}\n", parts.status);
    for (name, value) in &parts.headers {
        let _ = writeln!(text, "{name}: {}", value.to_str().unwrap());
    }
    let body = std::str::from_utf8(&bytes).expect("error bodies are text");
    if body.is_empty() || body.ends_with('\n') {
        let _ = writeln!(text, "body: {} bytes", bytes.len());
        text.push_str(body);
    } else {
        let _ = writeln!(text, "body: {} bytes, no final newline", bytes.len());
        text.push_str(body);
        text.push('\n');
    }
    text
}
