//! XML POST Object HTML form uploads, optionally authorized by a V2 or V4 policy document.
//!
//! Form fields before `file` are read into a bounded map; the file part then streams
//! through the shared object write path, so generations, checksums and catalog
//! admission are the same as for an XML PUT.

use super::{Error, Namespace, ObjectPath, XmlCode, XmlWriter, put::storage_error, writer::Quotes};
use crate::object::ObjectKey;
use crate::service::body::RequestBody as Incoming;
use crate::service::upload_drive::{
    CatalogWait, DriveError, UploadEnd, UploadSender, drive_upload, upload_channel,
};
use crate::{
    auth::{
        self, Caller,
        signature::{self, Failure, MAX_EXPIRES},
    },
    bucket::name::BucketKey,
    multipart::{Multipart, form_data_boundary},
    object::{
        ExpectedHashes, MapPatch, ObjectConditions, ObjectError, ObjectMetadataUpdate,
        ObjectRecord, Patch, UploadChunk, content_type, media_headers::CUSTOM_METADATA_PREFIX,
    },
    service::{
        body::{ResponseBody, boxed},
        context::{ObjectCtx, RequestCtx},
    },
    storage::ObjectUpload,
};
use http_body_util::Full;
use hyper::{
    HeaderMap, Request, Response, StatusCode, Uri,
    body::{Body, Bytes},
    header::HeaderValue,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod policy;
use super::put::length_range::LengthRange;
use policy::{Credentials, PolicyForm};
#[cfg(test)]
mod failure_tests;

/// All non-file fields together, which bounds the policy document as well. They stay in memory
/// until the file part starts, so a field's name and map entry count too: nothing else would stop
/// a form of empty fields from growing, now that the number of fields has no limit of its own.
const FORM_CAP: usize = crate::bounded_body::RESOURCE_BODY_LIMIT;
/// What one field's map entry takes beside its name and value.
const FIELD_ENTRY_CHARGE: usize = 64;

/// Cloud Storage reports unmet conditions as InvalidPolicyDocument with this wording.
fn policy_error(details: impl std::fmt::Display) -> Error {
    Error::new(
        XmlCode::InvalidPolicyDocument,
        format!(
            "The content of the form does not meet the conditions specified in the policy document. {details}"
        ),
    )
}

fn signature_error(failure: signature::VerificationError) -> Error {
    let failure = match failure {
        signature::VerificationError::Signature(failure) => failure,
        signature::VerificationError::Catalog(error) => return error.into(),
        signature::VerificationError::Worker(_) => {
            return Error::new(
                XmlCode::ServiceUnavailable,
                "The policy verifier is unavailable.",
            );
        }
    };
    match failure {
        Failure::UnknownSigner(lookup) => super::unknown_signer(lookup, "policy signing key"),
        // A policy does not sign Host, so a port never explains its mismatch.
        Failure::Mismatch | Failure::MismatchWithHostPort => Error::new(
            XmlCode::SignatureDoesNotMatch,
            "The policy signature does not match.",
        ),
        Failure::Malformed
        | Failure::RepeatedParameter(_)
        | Failure::BothExtensionSpellings
        | Failure::ExpiresOutOfRange(..)
        | Failure::Expired
        | Failure::NotYetValid => Error::invalid(
            "Invalid policy credentials; use exactly one supported signing version with its required fields and signature encoding.",
        ),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Form {
    fields: BTreeMap<String, String>,
    filename: Option<String>,
}

/// A form part's field name, lowercased, and the file name it sent, if any.
#[derive(Debug, PartialEq, Eq)]
struct PartName {
    name: String,
    filename: Option<String>,
}

/// Parse `form-data; name="..."; filename="..."`. Field names compare case-insensitively.
fn disposition(headers: &HeaderMap) -> Result<PartName, Error> {
    let malformed = || {
        Error::invalid("Each form part requires Content-Disposition: form-data; name=\"FIELD\".")
    };
    let raw = headers
        .get("content-disposition")
        .ok_or_else(malformed)?
        .as_bytes();
    let raw = std::str::from_utf8(raw).map_err(|_| malformed())?.trim();
    let (kind, mut rest) = raw.split_once(';').unwrap_or((raw, ""));
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return Err(malformed());
    }
    let (mut name, mut filename) = (None, None);
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let (key, tail) = rest.split_once('=').ok_or_else(malformed)?;
        let key = key.trim().to_ascii_lowercase();
        let tail = tail.trim_start();
        let (value, tail) = if let Some(tail) = tail.strip_prefix('"') {
            let mut value = String::new();
            let mut escaped = false;
            let mut end = None;
            for (index, c) in tail.char_indices() {
                if escaped {
                    value.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    end = Some(index);
                    break;
                } else {
                    value.push(c);
                }
            }
            let end = end.ok_or_else(malformed)?;
            (value, tail.get(end + 1..).ok_or_else(malformed)?)
        } else {
            let (value, rest) = tail
                .split_at_checked(tail.find(';').unwrap_or(tail.len()))
                .ok_or_else(malformed)?;
            (value.trim().to_owned(), rest)
        };
        let target = match key.as_str() {
            "name" => &mut name,
            "filename" => &mut filename,
            "filename*" => {
                return Err(Error::unsupported(
                    "RFC 5987 filename* form parameter; send a plain filename parameter",
                ));
            }
            _ => return Err(malformed()),
        };
        if target.replace(value).is_some() {
            return Err(malformed());
        }
        let tail = tail.trim_start();
        rest = match tail.strip_prefix(';') {
            Some(tail) => tail,
            None if tail.is_empty() => tail,
            None => return Err(malformed()),
        };
    }
    let name = name.filter(|name| !name.is_empty()).ok_or_else(malformed)?;
    Ok(PartName {
        name: name.to_ascii_lowercase(),
        filename,
    })
}

async fn read_form<B: Body<Data = Bytes> + Unpin>(
    parser: &mut Multipart<B>,
) -> Result<Form, Error> {
    parser.start().await?;
    let too_large = || {
        Error::invalid(format!(
            "Form fields before the file field are limited to {} in total.",
            crate::bounded_body::RESOURCE_BODY_LIMIT_TEXT
        ))
    };
    let mut fields = BTreeMap::new();
    let mut total = 0;
    loop {
        if parser.closed() {
            return Err(Error::invalid(
                "The form requires a file field; send it as the last form field.",
            ));
        }
        let PartName { name, filename } = disposition(&parser.part_headers().await?)?;
        if name == "file" {
            return Ok(Form { fields, filename });
        }
        total += name.len() + FIELD_ENTRY_CHARGE;
        if total > FORM_CAP {
            return Err(too_large());
        }
        let mut value = Vec::new();
        while let Some(chunk) = parser.next_chunk().await? {
            if chunk.len() > FORM_CAP - total {
                return Err(too_large());
            }
            total += chunk.len();
            value.extend_from_slice(&chunk);
        }
        let value = String::from_utf8(value)
            .map_err(|_| Error::invalid(format!("Form field {name} must be UTF-8.")))?;
        if fields.insert(name.clone(), value).is_some() {
            return Err(Error::invalid(format!("Duplicate form field {name}.")));
        }
    }
}

fn decode_base64(raw: &str) -> Option<Vec<u8>> {
    if raw.is_empty() {
        return None;
    }
    crate::base64::decode(
        raw,
        crate::base64::Alphabet::Standard,
        crate::base64::Padding::RequiredUncheckedBits,
    )
}

/// The documented format is ISO 8601 basic; the official SDKs send RFC 3339 instead, and the
/// C++ SDK writes nanoseconds. Flooring them to the microsecond never extends a policy.
fn expiration(raw: &str) -> Option<SystemTime> {
    if let Ok(time) = auth::signature::parse_timestamp(raw) {
        return Some(time);
    }
    let micros = crate::time_format::parse_deadline_timestamp(raw).ok()?;
    if micros >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_micros(micros.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_micros(micros.unsigned_abs()))
    }
}

/// Check that the policy has not expired and, for a verified signature, that it is already
/// valid and expires within the V4 signing limit.
fn check_policy_window(
    document: &serde_json::Map<String, serde_json::Value>,
    now: SystemTime,
    signed_at: Option<SystemTime>,
) -> Result<(), Error> {
    let expires = document
        .get("expiration")
        .and_then(serde_json::Value::as_str)
        .and_then(expiration)
        .ok_or_else(|| {
            policy_error("The policy expiration must be an ISO 8601 or RFC 3339 UTC timestamp.")
        })?;
    if now >= expires {
        return Err(policy_error("Invalid according to Policy: Policy expired."));
    }
    // Without these bounds a leaked signature could authorize uploads indefinitely, so a
    // verified policy gets the same validity window as a V4 signed URL.
    if let Some(active) = signed_at {
        if now
            .checked_add(crate::auth::signature::CLOCK_SKEW)
            .is_none_or(|now| now < active)
        {
            return Err(policy_error(
                "Invalid according to Policy: Policy not yet valid.",
            ));
        }
        if active
            .checked_add(Duration::from_secs(MAX_EXPIRES))
            .is_none_or(|limit| expires > limit)
        {
            return Err(policy_error(
                "The policy expiration must be no more than 7 days after x-goog-date.",
            ));
        }
    }
    Ok(())
}

/// The decoded policy document: a JSON object with only `expiration` and `conditions`.
fn policy_document(encoded: &str) -> Result<serde_json::Map<String, serde_json::Value>, Error> {
    let document = decode_base64(encoded)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .ok_or_else(|| policy_error("The policy field must be base64-encoded UTF-8 JSON."))?;
    let serde_json::Value::Object(document) = document else {
        return Err(policy_error("The policy document must be a JSON object."));
    };
    if let Some(key) = document
        .keys()
        .find(|key| !matches!(key.as_str(), "expiration" | "conditions"))
    {
        return Err(policy_error(format!(
            "Unsupported policy document field {key}."
        )));
    }
    Ok(document)
}

/// A policy condition's comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Eq,
    StartsWith,
}

impl Operator {
    fn parse(raw: &str) -> Option<Self> {
        if raw.eq_ignore_ascii_case("eq") {
            Some(Self::Eq)
        } else if raw.eq_ignore_ascii_case("starts-with") {
            Some(Self::StartsWith)
        } else {
            None
        }
    }

    /// The spelling a failed condition reports.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::StartsWith => "starts-with",
        }
    }

    fn matches(self, actual: &str, expected: &str) -> bool {
        match self {
            Self::Eq => actual == expected,
            Self::StartsWith => actual.starts_with(expected),
        }
    }
}

/// One parsed policy condition.
enum Condition<'p> {
    /// `{"field": "value"}`, `["eq", "$field", "value"]` or `["starts-with", "$field", "prefix"]`;
    /// the field name is lowercase, as form field names are.
    Compare {
        operator: Operator,
        field: String,
        expected: &'p str,
    },
    /// `["content-length-range", min, max]`, an inclusive file size.
    ContentLength(LengthRange),
}

impl<'p> Condition<'p> {
    fn parse(value: &'p serde_json::Value) -> Result<Self, Error> {
        let unsupported = || policy_error(format!("Unsupported policy condition {value}."));
        match value {
            serde_json::Value::Object(entry) if entry.len() == 1 => {
                let Some((field, expected)) = entry.iter().next() else {
                    return Err(unsupported());
                };
                Ok(Self::Compare {
                    operator: Operator::Eq,
                    field: field.to_ascii_lowercase(),
                    expected: expected.as_str().ok_or_else(unsupported)?,
                })
            }
            serde_json::Value::Array(items) => {
                let [operator, field, expected] = items.as_slice() else {
                    return Err(unsupported());
                };
                let operator = operator.as_str().ok_or_else(unsupported)?;
                if operator.eq_ignore_ascii_case("content-length-range") {
                    let range = field
                        .as_u64()
                        .zip(expected.as_u64())
                        .and_then(|(min, max)| LengthRange::new(min, max))
                        .ok_or_else(unsupported)?;
                    return Ok(Self::ContentLength(range));
                }
                let operator = Operator::parse(operator).ok_or_else(unsupported)?;
                let field = field
                    .as_str()
                    .and_then(|field| field.strip_prefix('$'))
                    .filter(|field| !field.is_empty())
                    .ok_or_else(unsupported)?;
                Ok(Self::Compare {
                    operator,
                    field: field.to_ascii_lowercase(),
                    expected: expected.as_str().ok_or_else(unsupported)?,
                })
            }
            // Only a one-entry object or an array states a condition.
            serde_json::Value::Object(_)
            | serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => Err(unsupported()),
        }
    }
}

/// The fields and size range the conditions checked so far cover.
#[derive(Default)]
struct Coverage {
    fields: BTreeSet<String>,
    range: Option<LengthRange>,
}

impl Coverage {
    fn apply(
        &mut self,
        condition: Condition<'_>,
        fields: &BTreeMap<String, String>,
        bucket: &str,
    ) -> Result<(), Error> {
        match condition {
            Condition::ContentLength(range) => {
                if self.range.replace(range).is_some() {
                    return Err(policy_error(
                        "The policy document has more than one content-length-range condition.",
                    ));
                }
                self.fields.insert("content-length".to_owned());
            }
            Condition::Compare {
                operator,
                field,
                expected,
            } => {
                // The bucket condition applies to the request target, whether or not a field
                // names it.
                let actual = if field == "bucket" {
                    bucket
                } else {
                    fields
                        .get(&field)
                        .ok_or_else(|| policy_error(format!("Missing key: {field}")))?
                };
                if !operator.matches(actual, expected) {
                    return Err(policy_error(format!(
                        "Policy condition failed: [\"{}\", \"${field}\", {}]",
                        operator.as_str(),
                        serde_json::Value::from(expected)
                    )));
                }
                self.fields.insert(field);
            }
        }
        Ok(())
    }

    /// The V4 bucket condition, then a condition for every field that needs one.
    fn finish(
        self,
        credentials: &Credentials<'_>,
        fields: &BTreeMap<String, String>,
    ) -> Result<Option<LengthRange>, Error> {
        if credentials.requires_bucket_condition() && !self.fields.contains("bucket") {
            return Err(policy_error(
                "The policy document requires a bucket condition.",
            ));
        }
        if let Some(field) = fields
            .keys()
            .find(|field| !credentials.unconditioned(field) && !self.fields.contains(*field))
        {
            return Err(policy_error(format!(
                "Form field {field} has no policy condition."
            )));
        }
        Ok(self.range)
    }
}

/// Check every condition against the submitted form and require a condition for every
/// field. Returns the inclusive content-length-range, if any.
///
/// `signed_at` is the x-goog-date of a verified signature. It is absent when policy
/// credentials are ignored (permissive mode with `signed_urls=ignore`).
fn check_policy(
    policy: &PolicyForm<'_>,
    fields: &BTreeMap<String, String>,
    bucket: &str,
    now: SystemTime,
    signed_at: Option<SystemTime>,
) -> Result<Option<LengthRange>, Error> {
    let document = policy_document(policy.document)?;
    check_policy_window(&document, now, signed_at)?;
    let conditions = document
        .get("conditions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| policy_error("The policy conditions must be an array."))?;
    let mut coverage = Coverage::default();
    for value in conditions {
        // Parse and check one condition at a time: an earlier failed condition is reported
        // before a later unsupported one.
        coverage.apply(Condition::parse(value)?, fields, bucket)?;
    }
    coverage.finish(&policy.credentials, fields)
}

/// What a successful form upload answers, from `success_action_redirect` or
/// `success_action_status`.
#[cfg_attr(test, derive(Debug))]
pub(super) enum Success {
    NoContent,
    Ok,
    /// 201 with the `PostResponse` document.
    Created,
    /// The validated `success_action_redirect`, already in its `Location` header form.
    Redirect(HeaderValue),
}

struct Plan<'f> {
    name: String,
    content_type: String,
    metadata: ObjectMetadataUpdate,
    declared_length: Option<u64>,
    success: Success,
    policy: Option<PolicyForm<'f>>,
}

fn header_value(field: &str, value: &str) -> Result<String, Error> {
    if value
        .bytes()
        .any(|byte| byte.is_ascii_control() && byte != b'\t')
    {
        return Err(Error::invalid(format!(
            "Form field {field} must not contain control characters."
        )));
    }
    Ok(value.to_owned())
}

/// Map form fields onto the shared upload request. Unknown or unsupported fields fail
/// instead of being stored or ignored.
fn plan<'f>(form: &'f Form, bucket: &str) -> Result<Plan<'f>, Error> {
    let fields = &form.fields;
    for name in fields.keys() {
        match name.as_str() {
            "key"
            | "bucket"
            | policy::POLICY
            | "success_action_status"
            | "success_action_redirect"
            | "content-type"
            | "cache-control"
            | "content-disposition"
            | "content-encoding"
            | "content-length"
            | super::CUSTOM_TIME => {}
            name if policy::V2_FIELDS.contains(&name) || policy::V4_FIELDS.contains(&name) => {}
            name if name.len() > CUSTOM_METADATA_PREFIX.len()
                && name.starts_with(CUSTOM_METADATA_PREFIX) => {}
            name => return Err(Error::unsupported(&format!("POST form field {name}"))),
        }
    }
    let policy = PolicyForm::select(fields)?;
    if fields.get("bucket").is_some_and(|value| value != bucket) {
        return Err(Error::invalid(
            "The bucket form field must match the bucket in the request URL.",
        ));
    }
    let key = fields
        .get("key")
        .filter(|key| !key.is_empty())
        .ok_or_else(|| Error::invalid("The form requires a non-empty key field."))?;
    let name = if key.contains("${filename}") {
        let filename = form
            .filename
            .as_deref()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                Error::invalid("The key uses ${filename}, but the file part has no filename.")
            })?;
        key.replace("${filename}", filename)
    } else {
        key.clone()
    };
    crate::object::name::validate(&name)?;
    let success = success_action(fields)?;
    let mut selected = HeaderMap::new();
    if let Some(value) = fields.get("content-type") {
        selected.insert(
            "content-type",
            value
                .parse()
                .map_err(|_| Error::invalid("Invalid Content-Type form field."))?,
        );
    }
    let content_type = crate::object::upload_headers::content_type(&selected)?;
    let content_type = content_type::or_default(content_type);
    let declared_length = fields
        .get("content-length")
        .map(|raw| {
            crate::decimal::Grammar::Digits
                .parse::<u64>(raw)
                .ok_or_else(|| Error::invalid("Invalid Content-Length form field."))
        })
        .transpose()?;
    Ok(Plan {
        name,
        content_type,
        metadata: form_metadata(fields)?,
        declared_length,
        success,
        policy,
    })
}

fn success_action(fields: &BTreeMap<String, String>) -> Result<Success, Error> {
    if let Some(url) = fields.get("success_action_redirect") {
        let location = url
            .parse()
            .ok()
            .filter(|uri: &Uri| {
                matches!(uri.scheme_str(), Some("http" | "https")) && uri.authority().is_some()
            })
            .and_then(|uri| HeaderValue::try_from(uri.to_string()).ok())
            .ok_or_else(|| {
                Error::invalid("success_action_redirect must be an absolute HTTP(S) URL.")
            })?;
        return Ok(Success::Redirect(location));
    }
    match fields.get("success_action_status").map(String::as_str) {
        None | Some("204") => Ok(Success::NoContent),
        Some("200") => Ok(Success::Ok),
        Some("201") => Ok(Success::Created),
        Some(_) => Err(Error::invalid(
            "success_action_status must be 200, 201 or 204.",
        )),
    }
}

fn form_metadata(fields: &BTreeMap<String, String>) -> Result<ObjectMetadataUpdate, Error> {
    let mut metadata = ObjectMetadataUpdate::default();
    for (field, target) in [
        ("cache-control", &mut metadata.cache_control),
        ("content-disposition", &mut metadata.content_disposition),
        ("content-encoding", &mut metadata.content_encoding),
    ] {
        if let Some(value) = fields.get(field) {
            *target = Patch::Set(header_value(field, value)?);
        }
    }
    if let Some(value) = fields.get(super::CUSTOM_TIME) {
        metadata.custom_time = Patch::Set(super::custom_time(value)?);
    }
    let mut custom = BTreeMap::new();
    for (field, value) in fields {
        if let Some(key) = field.strip_prefix(CUSTOM_METADATA_PREFIX) {
            custom.insert(key.to_owned(), Some(header_value(field, value)?));
        }
    }
    metadata.metadata = MapPatch::Merge(custom);
    Ok(metadata)
}

pub(crate) async fn respond(
    request: Request<Incoming>,
    object: &ObjectPath,
    ctx: RequestCtx<'_>,
    deadline: tokio::time::Instant,
) -> Result<Response<ResponseBody>, Error> {
    let RequestCtx { state, config, .. } = ctx;
    let bucket = BucketKey::for_profile(object.bucket.as_str(), config.compat_profile())?;
    // POST Object takes every option from the form; query options belong to other operations.
    if let Some(query) = request.uri().query() {
        let key = query.split('&').next().unwrap_or_default();
        return Err(Error::unsupported(&format!(
            "POST /BUCKET query parameter {}; send POST Object options as form fields",
            key.split('=').next().unwrap_or_default()
        )));
    }
    let boundary = form_data_boundary(request.headers()).map_err(|_| {
        Error::invalid(
            "POST Object requires Content-Type: multipart/form-data with a valid boundary.",
        )
    })?;
    let headers = request.headers().clone();
    let mut parser = Multipart::new(request.into_body(), &boundary);
    let form = read_form(&mut parser).await?;
    let Plan {
        name,
        content_type,
        metadata,
        declared_length,
        success,
        policy,
    } = plan(&form, bucket.as_str())?;
    let fields = &form.fields;
    let caller = authenticate(ctx, &headers, policy.as_ref()).await?;
    let range = policy
        .as_ref()
        .map(|policy| {
            // A verified signature leaves no caller in permissive mode, so ask the profile.
            let verifying = state.auth.signed_urls() == auth::SignedUrls::Verify;
            let signed_at = match &policy.credentials {
                Credentials::V4 { date, .. } => verifying
                    .then(|| signature::parse_timestamp(date).ok())
                    .flatten(),
                Credentials::V2 { .. } => None,
            };
            check_policy(
                policy,
                fields,
                bucket.as_str(),
                state.clock.now(),
                signed_at,
            )
        })
        .transpose()?
        .flatten();
    let ctx = ctx.object(caller, deadline);
    let access = ctx
        .authorize(auth::OBJECTS_CREATE, bucket.as_str())
        .await
        .map_err(|error| Error::from(error).for_caller(caller))?;
    let key = ObjectKey::from_bucket(bucket, &name);
    let permit = ctx
        .await_catalog(state.catalog.prepare_object_upload(
            key,
            ObjectConditions::default(),
            &access,
        ))
        .await
        .map_err(|error| storage_error(error, caller))?;
    let (sender, receiver) = upload_channel();
    let upload = state.catalog.put_object_prechecked(
        ObjectUpload {
            key,
            content_type,
            expected: ExpectedHashes::default(),
            conditions: ObjectConditions::default(),
            max_bytes: config.max_object_bytes(),
            metadata,
            access,
        },
        receiver,
        permit,
    );
    let limits = FileLimits {
        range,
        declared_length,
    };
    let record = stream_file(&mut parser, ctx, upload, sender, limits).await?;
    success_response(success, &record, ctx.endpoints.api_base_url())
}

/// Authenticates a form upload from its policy fields, or from its Authorization header when
/// the form has no policy.
async fn authenticate<'a>(
    ctx: RequestCtx<'a>,
    headers: &HeaderMap,
    policy: Option<&PolicyForm<'_>>,
) -> Result<Option<Caller<'a>>, Error> {
    let state = ctx.state;
    let Some(policy) = policy else {
        return state
            .auth
            .authenticate_request(headers, None, state.clock.as_ref(), ctx.listener)
            .await
            .map_err(Error::from);
    };
    if headers.contains_key("authorization") {
        return Err(Error::invalid(
            "Send either a policy form or an Authorization header, not both.",
        ));
    }
    match &policy.credentials {
        Credentials::V2 {
            access_id,
            signature,
        } => state
            .auth
            .authenticate_policy_v2(&state.catalog, access_id, signature, policy.document)
            .await
            .map_err(signature_error),
        Credentials::V4 {
            algorithm,
            credential,
            date,
            signature,
        } => state
            .auth
            .authenticate_policy(
                &state.catalog,
                algorithm,
                credential,
                date,
                signature,
                policy.document,
            )
            .await
            .map_err(signature_error),
    }
}

/// The file sizes a form allows: the policy's inclusive content-length-range and the
/// Content-Length form field.
#[derive(Clone, Copy)]
struct FileLimits {
    range: Option<LengthRange>,
    declared_length: Option<u64>,
}

/// Streams the file part into `upload` and checks its size against the form's limits.
async fn stream_file<B: Body<Data = Bytes> + Unpin>(
    parser: &mut Multipart<B>,
    ctx: ObjectCtx<'_>,
    upload: impl std::future::Future<Output = Result<ObjectRecord, ObjectError>>,
    sender: UploadSender,
    limits: FileLimits,
) -> Result<ObjectRecord, Error> {
    let FileLimits {
        range,
        declared_length,
    } = limits;
    let caller = ctx.caller;
    let max_bytes = ctx.config.max_object_bytes();
    let streaming = async {
        let mut size = 0_u64;
        while let Some(bytes) = parser.next_chunk().await? {
            size = size
                .checked_add(bytes.len() as u64)
                .filter(|size| *size <= max_bytes)
                .ok_or_else(|| storage_error(ObjectError::TooLarge, None))?;
            if range.is_some_and(|range| size > range.max()) {
                return Err(Error::new(
                    XmlCode::EntityTooLarge,
                    "The file exceeds the policy content-length-range maximum.",
                ));
            }
            if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
                // The upload driver preserves the catalog's original failure.
                return Ok(UploadEnd::Eof);
            }
        }
        if !parser.closed() {
            return Err(Error::invalid(
                "The file field must be the last field in the form.",
            ));
        }
        if range.is_some_and(|range| size < range.min()) {
            return Err(Error::new(
                XmlCode::EntityTooSmall,
                "The file is smaller than the policy content-length-range minimum.",
            ));
        }
        if declared_length.is_some_and(|length| length != size) {
            return Err(Error::invalid(
                "The Content-Length form field does not match the file size.",
            ));
        }
        parser.finish().await?;
        Ok(UploadEnd::Eof)
    };
    drive_upload(CatalogWait::Upload(ctx), upload, &sender, streaming)
        .await
        .map_err(|error| match error {
            DriveError::Upload(error) => storage_error(error, caller),
            DriveError::Producer(error) => error,
        })
}

fn status_response(status: StatusCode, body: String) -> Response<ResponseBody> {
    let mut response = boxed(Response::new(Full::new(Bytes::from(body))));
    *response.status_mut() = status;
    response
}

/// The `PostResponse` that `success_action_status=201` answers, which escapes quotes.
fn post_response_document(record: &ObjectRecord, etag: &str, api_base_url: &str) -> String {
    let mut document = XmlWriter::with_quotes("PostResponse", Namespace::None, Quotes::Escaped);
    document.element(
        "Location",
        &format!(
            "{}/{}/{}",
            api_base_url.trim_end_matches('/'),
            record.bucket,
            crate::url::encode_object_name(&record.name)
        ),
    );
    document.element("Bucket", &record.bucket);
    document.element("Key", &record.name);
    document.element("ETag", etag);
    document.finish()
}

pub(super) fn success_response(
    success: Success,
    record: &ObjectRecord,
    api_base_url: &str,
) -> Result<Response<ResponseBody>, Error> {
    let mut media = super::media_headers(record, super::CacheScope::Private)?;
    let etag = media
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let mut response = match success {
        Success::NoContent => status_response(StatusCode::NO_CONTENT, String::new()),
        Success::Ok => status_response(StatusCode::OK, String::new()),
        Success::Created => {
            let body = post_response_document(record, &etag, api_base_url);
            let content_length = HeaderValue::from(body.len());
            let mut response = status_response(StatusCode::CREATED, body);
            let headers = response.headers_mut();
            headers.insert(
                "content-type",
                HeaderValue::from_static("application/xml; charset=UTF-8"),
            );
            headers.insert("content-length", content_length);
            response
        }
        Success::Redirect(location) => {
            let mut response = status_response(StatusCode::SEE_OTHER, String::new());
            response.headers_mut().insert("location", location);
            response
        }
    };
    super::move_headers(
        &mut media,
        response.headers_mut(),
        &[
            "etag",
            "x-goog-generation",
            "x-goog-metageneration",
            "x-goog-hash",
            "x-goog-stored-content-length",
            "x-goog-stored-content-encoding",
        ],
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;
    use std::assert_matches;

    use crate::base64::encode as base64;

    fn encode(value: &serde_json::Value) -> String {
        base64(value.to_string().as_bytes())
    }

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    // 2026-10-29T00:00:00Z
    const NOW: u64 = 1_793_232_000;

    fn sdk_fields() -> BTreeMap<String, String> {
        fields(&[
            ("key", "post.txt"),
            ("x-goog-algorithm", "GOOG4-RSA-SHA256"),
            (
                "x-goog-credential",
                "a@example.invalid/20261029/auto/storage/goog4_request",
            ),
            ("x-goog-date", "20261029T000000Z"),
            ("x-goog-signature", "00"),
            ("policy", "ignored"),
        ])
    }

    fn sdk_conditions() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"bucket": "b"}),
            serde_json::json!({"key": "post.txt"}),
            serde_json::json!({"x-goog-date": "20261029T000000Z"}),
            serde_json::json!({"x-goog-credential": "a@example.invalid/20261029/auto/storage/goog4_request"}),
            serde_json::json!({"x-goog-algorithm": "GOOG4-RSA-SHA256"}),
        ]
    }

    fn policy(expiration: &str, conditions: Vec<serde_json::Value>) -> String {
        encode(&serde_json::json!({"expiration": expiration, "conditions": conditions}))
    }

    fn details(error: Error) -> (StatusCode, &'static str, String) {
        (error.status(), error.code.as_str(), error.message)
    }

    /// Checks `encoded` as the policy of a form with these fields and their credentials, and
    /// returns its content-length-range as `(min, max)`.
    fn check(
        encoded: &str,
        fields: &BTreeMap<String, String>,
        bucket: &str,
        now: SystemTime,
        signed_at: Option<SystemTime>,
    ) -> Result<Option<(u64, u64)>, Error> {
        let credentials = PolicyForm::select(fields).unwrap().unwrap().credentials;
        let policy = PolicyForm {
            document: encoded,
            credentials,
        };
        check_policy(&policy, fields, bucket, now, signed_at)
            .map(|range| range.map(|range| (range.min(), range.max())))
    }

    #[test]
    fn policy_accepts_go_and_ruby_sdk_documents_with_any_utc_offset() {
        for expiration in [
            "2026-10-29T00:05:00Z",
            "2026-10-29T09:05:00+09:00",
            "20261029T000500Z",
        ] {
            let encoded = policy(expiration, sdk_conditions());
            assert_eq!(
                check(&encoded, &sdk_fields(), "b", at(NOW), None).unwrap(),
                None,
                "{expiration}"
            );
        }
    }

    #[test]
    fn policy_accepts_the_cpp_sdk_nanosecond_expiration() {
        // google-cloud-cpp 3.13.0 writes the expiration with nanoseconds.
        let encoded = policy("2026-10-29T00:05:00.766459053Z", sdk_conditions());
        assert_eq!(
            check(&encoded, &sdk_fields(), "b", at(NOW), None).unwrap(),
            None
        );
    }

    #[test]
    fn verified_policy_uses_the_signed_url_validity_window() {
        let seven_days = "2026-11-05T00:00:00Z";
        let encoded = policy(seven_days, sdk_conditions());
        let fields = sdk_fields();
        check(&encoded, &fields, "b", at(NOW), Some(at(NOW))).unwrap();
        check(&encoded, &fields, "b", at(NOW - 900), Some(at(NOW))).unwrap();
        // The permissive profile ignores credentials, so only the policy expiration applies.
        let beyond = policy("2026-11-05T00:00:01Z", sdk_conditions());
        check(&beyond, &fields, "b", at(NOW), None).unwrap();
        for (encoded, now, expected) in [
            (&beyond, NOW, "no more than 7 days"),
            (&encoded, NOW - 901, "Policy not yet valid"),
        ] {
            let (status, code, message) =
                details(check(encoded, &fields, "b", at(now), Some(at(NOW))).unwrap_err());
            assert_eq!(
                (status, code),
                (StatusCode::BAD_REQUEST, "InvalidPolicyDocument")
            );
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn policy_rejects_expiry_at_the_exact_instant_and_malformed_documents() {
        let encoded = policy("2026-10-29T00:00:00Z", sdk_conditions());
        let (status, code, message) =
            details(check(&encoded, &sdk_fields(), "b", at(NOW), None).unwrap_err());
        assert_eq!(
            (status, code),
            (StatusCode::BAD_REQUEST, "InvalidPolicyDocument")
        );
        assert!(message.contains("Policy expired"), "{message}");
        check(&encoded, &sdk_fields(), "b", at(NOW - 1), None).unwrap();
        // Sub-microsecond digits are floored, so they cannot keep a policy alive at its instant.
        let floored = policy("2026-10-29T00:00:00.000000999Z", sdk_conditions());
        let (_, _, message) =
            details(check(&floored, &sdk_fields(), "b", at(NOW), None).unwrap_err());
        assert!(message.contains("Policy expired"), "{message}");
        for encoded in [
            "not base64".to_owned(),
            encode(
                &serde_json::json!({"expiration": "2026-10-30T00:00:00.0000000001Z", "conditions": []}),
            ),
            base64(b"[]"),
            encode(&serde_json::json!({"conditions": []})),
            encode(&serde_json::json!({"expiration": "tomorrow", "conditions": []})),
            encode(&serde_json::json!({"expiration": "2026-10-30T00:00:00Z"})),
            encode(
                &serde_json::json!({"expiration": "2026-10-30T00:00:00Z", "conditions": [], "extra": 1}),
            ),
        ] {
            assert_eq!(
                check(&encoded, &sdk_fields(), "b", at(NOW), None)
                    .unwrap_err()
                    .code
                    .as_str(),
                "InvalidPolicyDocument",
                "{encoded}"
            );
        }
    }

    #[test]
    fn policy_enforces_eq_starts_with_bucket_and_field_coverage() {
        let expiration = "2026-10-30T00:00:00Z";
        let mut form = sdk_fields();
        form.insert("content-type".into(), "image/png".into());
        form.insert("x-goog-meta-owner".into(), "alice".into());
        let mut conditions = sdk_conditions();
        conditions.push(serde_json::json!([
            "starts-with",
            "$Content-Type",
            "image/"
        ]));
        conditions.push(serde_json::json!(["eq", "$x-goog-meta-owner", "alice"]));
        check(
            &policy(expiration, conditions.clone()),
            &form,
            "b",
            at(NOW),
            None,
        )
        .unwrap();
        let cases: Vec<(Vec<serde_json::Value>, &str, &str)> = vec![
            // A different bucket target fails the bucket condition.
            (conditions.clone(), "other", "Policy condition failed"),
            // A field with no condition is rejected.
            (
                conditions[..conditions.len() - 1].to_vec(),
                "b",
                "x-goog-meta-owner has no policy condition",
            ),
            // A condition naming an absent field is rejected.
            (
                [
                    conditions.clone(),
                    vec![serde_json::json!(["eq", "$cache-control", "no-cache"])],
                ]
                .concat(),
                "b",
                "Missing key: cache-control",
            ),
            // starts-with is a prefix match, and eq is exact.
            (
                [
                    conditions[..5].to_vec(),
                    vec![
                        serde_json::json!(["starts-with", "$content-type", "text/"]),
                        serde_json::json!({"x-goog-meta-owner": "alice"}),
                    ],
                ]
                .concat(),
                "b",
                "Policy condition failed",
            ),
            (
                [
                    conditions[..6].to_vec(),
                    vec![serde_json::json!({"x-goog-meta-owner": "Alice"})],
                ]
                .concat(),
                "b",
                "Policy condition failed",
            ),
            // A bucket condition is always required.
            (conditions[1..].to_vec(), "b", "requires a bucket condition"),
            // Unknown operators are not silently accepted.
            (
                [
                    conditions.clone(),
                    vec![serde_json::json!(["ends-with", "$key", "txt"])],
                ]
                .concat(),
                "b",
                "Unsupported policy condition",
            ),
        ];
        for (conditions, bucket, expected) in cases {
            let error = check(
                &policy(expiration, conditions),
                &form,
                bucket,
                at(NOW),
                None,
            )
            .unwrap_err();
            assert_eq!(error.code.as_str(), "InvalidPolicyDocument");
            assert!(error.message.contains(expected), "{}", error.message);
        }
    }

    /// Each condition is parsed and checked before the next one is read, so the first bad
    /// condition names the error whichever way it is bad.
    #[test]
    fn an_earlier_failed_condition_is_reported_before_a_later_unsupported_one() {
        let expiration = "2026-10-30T00:00:00Z";
        let failed = serde_json::json!({"key": "other.txt"});
        let unsupported = serde_json::json!(["ends-with", "$key", "txt"]);
        for (conditions, expected) in [
            (
                vec![failed.clone(), unsupported.clone()],
                "Policy condition failed: [\"eq\", \"$key\", \"other.txt\"]",
            ),
            (
                vec![unsupported, failed],
                "Unsupported policy condition [\"ends-with\",\"$key\",\"txt\"].",
            ),
        ] {
            let conditions = [sdk_conditions(), conditions].concat();
            let (status, code, message) = details(
                check(
                    &policy(expiration, conditions),
                    &sdk_fields(),
                    "b",
                    at(NOW),
                    None,
                )
                .unwrap_err(),
            );
            assert_eq!(
                (status, code),
                (StatusCode::BAD_REQUEST, "InvalidPolicyDocument")
            );
            assert!(message.ends_with(expected), "{message}");
        }
    }

    #[test]
    fn policy_reads_one_content_length_range_and_it_covers_content_length() {
        let expiration = "2026-10-30T00:00:00Z";
        let mut form = sdk_fields();
        form.insert("content-length".into(), "5".into());
        let mut conditions = sdk_conditions();
        conditions.push(serde_json::json!(["content-length-range", 1, 10]));
        assert_eq!(
            check(
                &policy(expiration, conditions.clone()),
                &form,
                "b",
                at(NOW),
                None,
            )
            .unwrap(),
            Some((1, 10))
        );
        for range in [
            serde_json::json!(["content-length-range", 10, 1]),
            serde_json::json!(["content-length-range", -1, 1]),
            serde_json::json!(["content-length-range", "1", "2"]),
            serde_json::json!(["content-length-range", 1, 2]),
        ] {
            let conditions = [conditions.clone(), vec![range.clone()]].concat();
            assert_eq!(
                check(&policy(expiration, conditions), &form, "b", at(NOW), None,)
                    .unwrap_err()
                    .code
                    .as_str(),
                "InvalidPolicyDocument",
                "{range}"
            );
        }
    }

    fn form(pairs: &[(&str, &str)], filename: Option<&str>) -> Form {
        Form {
            fields: fields(pairs),
            filename: filename.map(str::to_owned),
        }
    }

    #[test]
    fn plan_maps_documented_fields_and_rejects_unsupported_ones() {
        let documented = form(
            &[
                ("key", "uploads/${filename}"),
                ("bucket", "b"),
                ("content-type", "text/plain"),
                ("cache-control", "no-cache"),
                ("content-disposition", "inline"),
                ("content-encoding", "identity"),
                ("x-goog-custom-time", "2026-10-20T01:02:03Z"),
                ("x-goog-meta-owner", "alice"),
                ("success_action_status", "201"),
            ],
            Some("photo.png"),
        );
        let planned = plan(&documented, "b").unwrap();
        assert_eq!(planned.name, "uploads/photo.png");
        assert_eq!(planned.content_type, "text/plain");
        assert_eq!(
            planned.metadata.cache_control,
            Patch::Set("no-cache".into())
        );
        assert_eq!(
            planned.metadata.metadata,
            MapPatch::Merge(BTreeMap::from([("owner".into(), Some("alice".into()))]))
        );
        assert_matches!(planned.success, Success::Created);
        let minimal = form(&[("key", "a")], None);
        let defaults = plan(&minimal, "b").unwrap();
        assert_eq!(defaults.content_type, "application/octet-stream");
        assert_matches!(defaults.success, Success::NoContent);
        let redirecting = form(
            &[
                ("key", "a"),
                (
                    "success_action_redirect",
                    "https://example.invalid/done?x=1",
                ),
                ("success_action_status", "201"),
            ],
            None,
        );
        let redirect = plan(&redirecting, "b").unwrap();
        assert_matches!(redirect.success, Success::Redirect(url) if url == "https://example.invalid/done?x=1");
        for (pairs, filename) in [
            (vec![("key", "")], None),
            (vec![("key", "${filename}")], None),
            (vec![("key", "${filename}")], Some("")),
            (vec![("key", "a"), ("bucket", "other")], None),
            (vec![("key", "a"), ("acl", "public-read")], None),
            (
                vec![("key", "a"), ("expires", "2026-10-30T00:00:00Z")],
                None,
            ),
            (
                vec![("key", "a"), ("googleaccessid", "x"), ("signature", "y")],
                None,
            ),
            (vec![("key", "a"), ("x-ignore-me", "y")], None),
            (vec![("key", "a"), ("x-goog-meta-", "y")], None),
            (vec![("key", "a"), ("success_action_status", "202")], None),
            (
                vec![("key", "a"), ("success_action_redirect", "/relative")],
                None,
            ),
            (vec![("key", "a"), ("content-length", "-1")], None),
            (
                vec![("key", "a"), ("x-goog-custom-time", "yesterday")],
                None,
            ),
            (vec![("key", "a"), ("cache-control", "a\r\nb")], None),
            (vec![("key", "a"), ("x-goog-signature", "00")], None),
            (vec![("key", "a"), ("policy", "e30=")], None),
        ] {
            let error = plan(&form(&pairs, filename), "b").err();
            assert_eq!(
                error.map(|error| error.status()),
                Some(StatusCode::BAD_REQUEST),
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn disposition_reads_quoted_names_and_filenames_case_insensitively() {
        let parse = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("content-disposition", value.parse().unwrap());
            disposition(&headers)
        };
        assert_eq!(
            parse("form-data; name=\"Content-Type\"").unwrap(),
            PartName {
                name: "content-type".into(),
                filename: None
            }
        );
        assert_eq!(
            parse("Form-Data; name=file; filename=\"a \\\"b\\\";c.txt\"").unwrap(),
            PartName {
                name: "file".into(),
                filename: Some("a \"b\";c.txt".into())
            }
        );
        for value in [
            "attachment; name=\"key\"",
            "form-data",
            "form-data; name=\"\"",
            "form-data; name=\"key",
            "form-data; name=\"a\"; name=\"b\"",
            "form-data; name=\"a\" junk",
            "form-data; name=\"file\"; filename*=UTF-8''a.txt",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
    }

    async fn parse_form(body: &'static str) -> Result<(Form, Vec<u8>), Error> {
        let mut parser = Multipart::new(
            Full::new(Bytes::from_static(body.as_bytes())),
            &"b".parse()?,
        );
        let form = read_form(&mut parser).await?;
        let mut file = Vec::new();
        while let Some(chunk) = parser.next_chunk().await? {
            file.extend_from_slice(&chunk);
        }
        Ok((form, file))
    }

    #[tokio::test]
    async fn form_reader_stops_at_the_file_part_and_bounds_fields() {
        let (form, file) = parse_form(
            "--b\r\nContent-Disposition: form-data; name=\"Key\"\r\n\r\npost.txt\r\n--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"post.txt\"\r\nContent-Type: application/octet-stream\r\n\r\nposted\r\n--b--\r\n",
        )
        .await
        .unwrap();
        assert_eq!(
            form,
            Form {
                fields: fields(&[("key", "post.txt")]),
                filename: Some("post.txt".into())
            }
        );
        assert_eq!(file, b"posted");
        for body in [
            // No file part.
            "--b\r\nContent-Disposition: form-data; name=\"key\"\r\n\r\na\r\n--b--\r\n",
            // Duplicate field.
            "--b\r\nContent-Disposition: form-data; name=\"key\"\r\n\r\na\r\n--b\r\nContent-Disposition: form-data; name=\"KEY\"\r\n\r\nb\r\n--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nx\r\n--b--\r\n",
            // A part without Content-Disposition.
            "--b\r\nContent-Type: text/plain\r\n\r\na\r\n--b--\r\n",
        ] {
            assert_eq!(
                parse_form(body).await.unwrap_err().status(),
                StatusCode::BAD_REQUEST
            );
        }
        let file = "\r\n--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nx\r\n--b--\r\n";
        let head = "--b\r\nContent-Disposition: form-data; name=\"key\"\r\n\r\n";
        let charged = "key".len() + FIELD_ENTRY_CHARGE;
        // One field may take the whole budget, far above the former 64 KiB per field.
        let exact = format!("{head}{}{file}", "x".repeat(FORM_CAP - charged));
        let boundary = "b".parse().unwrap();
        let mut parser = Multipart::new(Full::new(Bytes::from(exact)), &boundary);
        assert_eq!(
            read_form(&mut parser).await.unwrap().fields["key"].len(),
            FORM_CAP - charged
        );
        let long = format!("{head}{}{file}", "x".repeat(FORM_CAP - charged + 1));
        let mut parser = Multipart::new(Full::new(Bytes::from(long)), &boundary);
        assert!(
            read_form(&mut parser)
                .await
                .unwrap_err()
                .message
                .contains("limited to")
        );
    }

    #[tokio::test]
    async fn empty_fields_cannot_grow_the_form_past_its_memory_budget() {
        let form = |count: usize| {
            let mut body = String::new();
            for index in 0..count {
                body.push_str(&format!(
                    "--b\r\nContent-Disposition: form-data; name=\"x-goog-meta-{index:06}\"\r\n\r\n\r\n"
                ));
            }
            body.push_str(
                "--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nx\r\n--b--\r\n",
            );
            Multipart::new(Full::new(Bytes::from(body)), &"b".parse().unwrap())
        };
        // Each entry costs its name and map entry although its value is empty.
        let charge = "x-goog-meta-000000".len() + FIELD_ENTRY_CHARGE;
        let fitting = FORM_CAP / charge;
        assert_eq!(
            read_form(&mut form(fitting)).await.unwrap().fields.len(),
            fitting
        );
        assert!(
            read_form(&mut form(fitting + 1))
                .await
                .unwrap_err()
                .message
                .contains("limited to")
        );
    }

    #[test]
    fn base64_decoder_is_strict_about_padding() {
        assert_eq!(decode_base64("e30=").unwrap(), b"{}");
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
        assert_eq!(decode_base64("YWJj").unwrap(), b"abc");
        for raw in ["", "YQ", "Y===", "YQ==YWJj", "Y=Q=", "YW*j"] {
            assert!(decode_base64(raw).is_none(), "{raw}");
        }
    }
}
