//! How the JSON API sends the common error: the error envelope, the bare text pages of the
//! upload front end, the media download's plain answers and the wording of a body that is not
//! JSON. The error itself, its kinds and the conversions from domain errors live in
//! [`crate::api_error`], which the XML and gRPC adapters map to their own answers.

use crate::api_error::{ApiError, BAD_CONTENT_TYPE_TEXT, MISSING_END_BOUNDARY_TEXT, PlainCache};
use crate::bounded_body::BodyError;
use crate::input::{InputError, InputKind};
use crate::multipart::MultipartError;
use crate::object::media_headers::NO_STORE_CACHE_CONTROL;
use crate::object::upload_headers::UploadHeaderError;
use crate::service::ServiceError;
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes, header};
use serde_json::{Map, Value, json};

impl From<InputError> for ApiError {
    fn from(input: InputError) -> Self {
        let error = match input.kind {
            InputKind::Invalid => Self::invalid(input.message),
            InputKind::Required => Self::required(input.message),
            InputKind::Unsupported => Self::unsupported_message(input.message),
        };
        match input.field {
            Some(field) => error.at_parameter(field),
            None => error,
        }
    }
}

impl From<BodyError> for ApiError {
    fn from(error: BodyError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<UploadHeaderError> for ApiError {
    fn from(error: UploadHeaderError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<crate::object::media_headers::StoredHeaderError> for ApiError {
    fn from(error: crate::object::media_headers::StoredHeaderError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::Input(error) => error.into(),
            ServiceError::Project(error) => error.into(),
            ServiceError::Catalog(error) => error.into(),
            ServiceError::UploadHeader(error) => match error {
                UploadHeaderError::UnsupportedAlgorithm => {
                    Self::unsupported_message(error.to_string())
                }
                UploadHeaderError::InvalidContentType
                | UploadHeaderError::DuplicateContentType
                | UploadHeaderError::DuplicateAlgorithm
                | UploadHeaderError::MalformedHash(_)
                | UploadHeaderError::Digest(_)
                | UploadHeaderError::Conflicting(_) => Self::invalid_parameter(error.to_string()),
            },
            ServiceError::Multipart(error) => match error {
                MultipartError::Framing => Self::invalid_parameter(error.to_string()),
                MultipartError::BadContentType => {
                    Self::invalid_parameter(error.to_string()).with_plain(&BAD_CONTENT_TYPE_TEXT)
                }
                MultipartError::MissingEnd => Self::invalid_parameter(error.to_string())
                    .with_plain(&MISSING_END_BOUNDARY_TEXT),
                MultipartError::UnsupportedParameter(_) => {
                    Self::unsupported_message(error.to_string())
                }
            },
            ServiceError::StoredHeader(_) => Self::invalid_stored_header(),
            ServiceError::Body(error) => match error {
                BodyError::TooLarge => Self::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "uploadTooLarge",
                    error.to_string(),
                ),
                BodyError::OutOfMemory => Self::service_unavailable(error.to_string()),
                BodyError::Unreadable => {
                    Self::new(StatusCode::BAD_REQUEST, "badRequest", error.to_string())
                }
            },
            ServiceError::Time(error) => error.into(),
        }
    }
}

// PATCH and PUT read individual members of the same resource as an insert.
impl From<crate::bucket::resource::Error> for ApiError {
    fn from(error: crate::bucket::resource::Error) -> Self {
        ServiceError::from(error).into()
    }
}

/// The `Cache-Control` of a media download that Cloud Storage answers with a plain error; its
/// `Expires` is the response time.
pub(crate) const MEDIA_ERROR_CACHE_CONTROL: &str = "private, max-age=0";

/// How long a malformed body an error quotes. A resource body may be far longer, and the quote
/// appears twice in the response, so past this it would make the answer larger than the request.
const QUOTED_BODY_LIMIT: usize = 64 * 1024;

/// What a JSON parse error quotes: the parser's reason, the input it shows and the column of
/// the caret under it.
struct ParseContext {
    reason: String,
    shown: String,
    column: usize,
}

/// Where Cloud Storage's parser expected the `:` after the first key and what it said.
/// The first key may be bare, as in the recorded `{not json` and `oops` inputs.
fn missing_colon(text: &str) -> Option<ParseContext> {
    let chars: Vec<char> = text.chars().collect();
    let skip_space = |mut index: usize| {
        while chars.get(index).is_some_and(|c| c.is_whitespace()) {
            index += 1;
        }
        index
    };
    let mut index = skip_space(0);
    if chars.get(index) == Some(&'{') {
        index = skip_space(index + 1);
        // An object that ends right after its brace has no key yet (recorded for `{`).
        if index == chars.len() {
            return Some(ParseContext {
                reason: "Unexpected end of string. Expected an object key or }.".to_owned(),
                shown: String::new(),
                column: 0,
            });
        }
    }
    let key_start = index;
    if chars.get(index) == Some(&'"') {
        index += 1;
        while chars.get(index).is_some_and(|c| *c != '"') {
            index += 1;
        }
        index += 1;
    } else {
        while chars
            .get(index)
            .is_some_and(|c| !c.is_whitespace() && *c != ':')
        {
            index += 1;
        }
    }
    let index = skip_space(index);
    match chars.get(index) {
        Some(':') => None,
        Some(_) => Some(ParseContext {
            reason: "Expected : between key:value pair.".to_owned(),
            shown: text.to_owned(),
            column: index,
        }),
        // At the end of the input the quoted context starts at the key (recorded for `{oops`).
        None => Some(ParseContext {
            reason: "Unexpected end of string. Expected : between key:value pair.".to_owned(),
            shown: chars.iter().skip(key_start).collect(),
            column: chars.len() - key_start,
        }),
    }
}

/// Escapes text for an HTML document. The object name in a media 404 is client-controlled and
/// the response is `text/html`, so every markup-significant character must be neutralized.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// A request body that is not JSON. Cloud Storage quotes the offending input with a caret
/// under the position where its lenient parser stopped, which the recorded object-resource
/// failures show for a missing `:` after the first key.
pub(crate) fn parse_error(input: &[u8], cause: &serde_json::Error) -> ApiError {
    if input.len() > QUOTED_BODY_LIMIT {
        return ApiError::malformed_json(format!("Parse Error: {cause}"));
    }
    let text = String::from_utf8_lossy(input);
    let ParseContext {
        reason,
        shown,
        column,
    } = missing_colon(&text).unwrap_or_else(|| {
        // A body that ends inside a value is reported with an empty context line (recorded
        // 2026-10-31 for bucket bodies truncated after a key).
        if cause.is_eof() {
            return ParseContext {
                reason: "Unexpected end of string. Expected a value.".to_owned(),
                shown: String::new(),
                column: 0,
            };
        }
        ParseContext {
            reason: cause
                .to_string()
                .split(" at line ")
                .next()
                .unwrap_or_default()
                .to_owned(),
            shown: text.to_string(),
            column: cause.column().saturating_sub(1),
        }
    });
    ApiError::malformed_json(format!(
        "Parse Error: {reason}\n{shown}\n{}^",
        " ".repeat(column)
    ))
}

/// The JSON API's answers to the shared error. A trait in this module rather than methods on
/// the error, so that only code that names the JSON adapter can render one: the XML and gRPC
/// adapters map the error to their own answers and must not reach this rendering.
pub(crate) trait JsonErrorResponse {
    /// The error envelope: `{"error": {"code", "message", "errors": [..]}}`.
    fn value(&self) -> Value;

    /// A media download's error. Cloud Storage answers a missing object and an unsatisfiable
    /// range with an HTML-typed plain message and `private, max-age=0`; only these two were
    /// recorded, so other media errors keep the JSON form.
    fn into_media_response(self) -> Response<Full<Bytes>>;

    fn into_response(self) -> Response<Full<Bytes>>;
}

impl JsonErrorResponse for ApiError {
    fn value(&self) -> Value {
        let mut item = Map::from_iter([
            ("message".into(), self.message.as_str().into()),
            ("domain".into(), "global".into()),
            ("reason".into(), self.reason.into()),
        ]);
        if let Some(location) = self.location {
            item.insert("locationType".into(), location.kind.into());
            item.insert("location".into(), location.name.into());
        }
        json!({"error": {"code": self.status.as_u16(), "message": self.message, "errors": [item]}})
    }

    fn into_media_response(self) -> Response<Full<Bytes>> {
        if !self.is_missing_object() && self.status != StatusCode::RANGE_NOT_SATISFIABLE {
            return self.into_response();
        }
        // The range refusal has its own short wording, unlike the JSON form.
        let text = if self.status == StatusCode::RANGE_NOT_SATISFIABLE {
            "Request range not satisfiable".to_owned()
        } else {
            escape_html(&self.message)
        };
        let mut response = Response::new(Full::new(Bytes::from(text)));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/html; charset=UTF-8"),
        );
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static(MEDIA_ERROR_CACHE_CONTROL),
        );
        response
    }

    fn into_response(self) -> Response<Full<Bytes>> {
        if let Some(denied) = &self.denied {
            denied.log();
        }
        if self.status == StatusCode::NOT_MODIFIED {
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = self.status;
            return response;
        }
        if let Some(plain) = self.plain {
            let body = plain.body.map_or_else(
                || Bytes::from(self.message.clone()),
                |text| Bytes::from_static(text.as_bytes()),
            );
            let mut response = Response::new(Full::new(body));
            *response.status_mut() = self.status;
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(plain.content_type),
            );
            if plain.cache == PlainCache::NoStore {
                headers.insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static(NO_STORE_CACHE_CONTROL),
                );
                headers.insert(header::PRAGMA, header::HeaderValue::from_static("no-cache"));
            }
            return response;
        }
        let mut text = super::render_body(&self.value());
        // The upload front end ended its `text/html` envelopes with a newline (recorded).
        if self.html_typed {
            // The upload front end sends no nosniff header; JSON escapes keep request input out of HTML markup.
            text = text
                .replace('&', "\\u0026")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e");
            text.push('\n');
        }
        let mut response = Response::new(Full::new(Bytes::from(text)));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static(if self.html_typed {
                "text/html; charset=UTF-8"
            } else {
                "application/json; charset=UTF-8"
            }),
        );
        // Recorded Cloud Storage JSON errors forbid every cache, HTTP/1.0 ones included.
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static(NO_STORE_CACHE_CONTROL),
        );
        headers.insert(header::PRAGMA, header::HeaderValue::from_static("no-cache"));
        if let Some(allow) = self.allow {
            response
                .headers_mut()
                .insert(header::ALLOW, header::HeaderValue::from_static(allow));
        }
        response
    }
}

#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_error::ErrorKind;
    use crate::{
        auth::AuthFailure,
        bucket::CatalogError,
        object::ObjectError,
        storage::failure::{BackendFailure, test_support::denied},
    };

    #[test]
    fn a_long_malformed_body_is_not_quoted_back() {
        let parse = |input: &[u8]| {
            let cause = serde_json::from_slice::<serde_json::Value>(input).unwrap_err();
            parse_error(input, &cause)
        };
        assert!(parse(b"{\"a\":}").message.contains("{\"a\":}\n"));
        let mut long = vec![b' '; QUOTED_BODY_LIMIT];
        long.push(b'x');
        let error = parse(&long);
        assert!(error.message.len() < 1024, "{}", error.message.len());
        assert_eq!(error.kind, ErrorKind::MalformedJson);
        // The bare-key form walks the body by character before it can report anything.
        let mut bare = vec![b'x'; QUOTED_BODY_LIMIT];
        bare.push(b'x');
        assert!(parse(&bare).message.len() < 1024);
    }

    /// Every error enum renders a refused permission as the documented 403 `forbidden` reason
    /// with the message naming the principal and the permission.
    #[test]
    fn denials_keep_the_forbidden_reason_and_name_the_principal_and_permission() {
        let message = "viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
        let viewer = || denied(Some("viewer@example.invalid"));
        for error in [
            ApiError::from(AuthFailure::Denied(viewer())),
            ApiError::from(CatalogError::Denied(viewer())),
            ApiError::from(ObjectError::Denied(viewer())),
        ] {
            assert_eq!(
                error.value(),
                json!({"error": {"code": 403, "message": message,
                    "errors": [{"message": message, "domain": "global", "reason": "forbidden"}]}})
            );
            assert_eq!(error.denied, Some(viewer()));
        }
        // Refusals that no permission names keep the documented example message.
        for error in [
            ApiError::from(AuthFailure::Forbidden),
            ApiError::from(CatalogError::Forbidden),
            ApiError::from(ObjectError::Forbidden),
        ] {
            assert_eq!(
                (error.status, error.reason),
                (StatusCode::FORBIDDEN, "forbidden")
            );
            assert_eq!(error.message, "Forbidden");
            assert_eq!(error.denied, None);
        }
    }

    #[test]
    fn an_anonymous_caller_gets_401_required_naming_the_permission_it_lacks() {
        let anonymous = ApiError::from(CatalogError::Denied(denied(None))).for_anonymous_caller();
        let message = "Anonymous caller does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
        assert_eq!(
            anonymous.value(),
            json!({"error": {"code": 401, "message": message, "errors": [{"message": message,
                "domain": "global", "reason": "required", "locationType": "header",
                "location": "Authorization"}]}})
        );
        assert_eq!(anonymous.kind, ErrorKind::Required);
        // Another 403, or a refusal naming a stored principal, keeps the documented example.
        let login_required = ApiError::from(AuthFailure::LoginRequired);
        for error in [
            ApiError::from(ObjectError::LockedRetention),
            ApiError::from(ObjectError::Denied(denied(Some(
                "snapshot@example.invalid",
            )))),
        ] {
            let error = error.for_anonymous_caller();
            assert_eq!(error.value(), login_required.value());
            assert_eq!(error.kind, ErrorKind::Required);
        }
    }

    #[test]
    fn json_parse_errors_quote_the_input_with_a_caret_like_cloud_storage() {
        let parse = |input: &str| {
            let cause = serde_json::from_slice::<Value>(input.as_bytes()).unwrap_err();
            parse_error(input.as_bytes(), &cause)
        };
        let error = parse("{not json");
        assert_eq!(error.kind, ErrorKind::MalformedJson);
        assert_eq!((error.status.as_u16(), error.reason), (400, "invalid"));
        assert_eq!(
            error.message,
            "Parse Error: Expected : between key:value pair.\n{not json\n     ^"
        );
        assert_eq!(
            parse("{oops").message,
            "Parse Error: Unexpected end of string. Expected : between key:value pair.\noops\n    ^"
        );
        assert_eq!(
            parse("{ ").message,
            "Parse Error: Unexpected end of string. Expected an object key or }.\n\n^"
        );
    }

    #[tokio::test]
    async fn catalog_backend_errors_keep_bucket_guidance_and_safe_envelope() {
        use http_body_util::BodyExt;

        let generic = "Bucket catalog is unavailable";
        for (cause, message) in [
            (
                CatalogError::Unavailable("catalog is closed".into()),
                generic,
            ),
            (
                CatalogError::Unavailable(BackendFailure::CatalogClosed.into()),
                "Bucket catalog is unavailable: catalog is closed; wait for the server to restart before retrying",
            ),
            (
                CatalogError::Io(std::io::ErrorKind::PermissionDenied.into()),
                generic,
            ),
            (
                CatalogError::NewerSchema {
                    version: 99,
                    supported: 50,
                },
                generic,
            ),
            (CatalogError::AuthenticationAlreadyConfigured, generic),
            (
                CatalogError::Corrupt("/secret token=private".into()),
                generic,
            ),
        ] {
            let error = ApiError::from(cause);
            assert!(!error.message.contains("Object storage"));
            assert_eq!(error.message, message);
            let response = error.into_response();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/json; charset=UTF-8"
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                NO_STORE_CACHE_CONTROL
            );
            assert_eq!(response.headers()[header::PRAGMA], "no-cache");
            assert_eq!(response.headers().len(), 3);
            let expected = json!({"error": {
                "code": 503,
                "message": message,
                "errors": [{"message": message, "domain": "global", "reason": "backendError"}]
            }});
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                serde_json::to_string_pretty(&expected).unwrap()
            );
        }
    }
}
