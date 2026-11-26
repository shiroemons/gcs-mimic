//! What a request header is to the XML adapter, decided from its name alone.

/// What a request header is to the XML adapter. Each operation matches on it to accept or refuse
/// a header, so a new kind makes every operation decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeaderKind {
    /// `x-goog-date`, `x-amz-date`, `x-goog-content-sha256`, `x-amz-content-sha256`.
    Signing,
    /// `x-amz-decoded-content-length`, the size of an aws-chunked body.
    DecodedLength,
    /// `x-goog-meta-*`, including a bare `x-goog-meta-` that names the empty key.
    CustomMetadata,
    /// Customer-supplied key headers, the billing project, the generation preconditions, the
    /// API client and the idempotency token: read alike by every object operation.
    CommonObject,
    /// Any other `x-goog-*`.
    Goog,
    /// Any other `x-amz-*`.
    Amz,
    /// `If-Match`, `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since`, `If-Range`.
    HttpPrecondition,
    Other,
}

// Ignoring these HTTP conditions would serve an object without checking its preconditions.
const HTTP_PRECONDITION_HEADERS: &[&str] = &[
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
    "if-range",
];

impl HeaderKind {
    /// Signing inputs are classified first because two of them start with `x-goog-`; the
    /// decoded length is apart from other `x-amz-*` because only a signed PUT accepts it.
    pub(super) fn of(name: &str) -> Self {
        if crate::auth::signature::authentication_header(name) {
            Self::Signing
        } else if name == "x-amz-decoded-content-length" {
            Self::DecodedLength
        } else if name.starts_with("x-goog-meta-") {
            Self::CustomMetadata
        } else if is_common_object_header(name) {
            Self::CommonObject
        } else if name.starts_with("x-goog-") {
            Self::Goog
        } else if name.starts_with("x-amz-") {
            Self::Amz
        } else if HTTP_PRECONDITION_HEADERS.contains(&name) {
            Self::HttpPrecondition
        } else {
            Self::Other
        }
    }
}

// These headers have the same meaning on XML reads, PUT and resumable initiation.
fn is_common_object_header(name: &str) -> bool {
    crate::csek::is_key_header(name, crate::csek::DESTINATION_PREFIX)
        || name == super::BILLING_HEADER
        || matches!(
            name,
            "x-goog-if-generation-match"
                | "x-goog-if-metageneration-match"
                | "x-goog-api-client"
                | crate::object::IdempotencyToken::HEADER
        )
}
