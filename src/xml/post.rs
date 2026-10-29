//! XML POST Object HTML form uploads, optionally authorized by a V4 policy document.
//!
//! Form fields before `file` are read into a bounded map; the file part then streams
//! through the shared object write path, so generations, checksums and catalog
//! admission are the same as for an XML PUT.

use super::{Error, ObjectPath, put::storage_error};
use crate::{
    auth::{
        self,
        signature::{self, Failure, MAX_EXPIRES},
    },
    config::Config,
    json::{self, JsonError},
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, UploadChunk,
        custom_time_key,
    },
    server::{
        ObjectActivity, ResponseBody, State, anonymous_error, authorize_bucket,
        await_upload_catalog, body_progress, boxed,
    },
};
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Request, Response, StatusCode, Uri,
    body::{Body, Bytes, Incoming},
    header::HeaderValue,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, watch};

/// One form field value, which bounds the policy document as well.
const FIELD_CAP: usize = 64 * 1024;
/// All non-file fields together; they are held in memory until the file part starts.
const FORM_CAP: usize = 256 * 1024;
const FIELD_LIMIT: usize = 64;
const CUSTOM_METADATA_CAP: usize = 8192;

fn invalid(message: impl Into<String>) -> Error {
    Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", message)
}

/// Cloud Storage reports unmet conditions as InvalidPolicyDocument with this wording.
fn policy_error(details: impl std::fmt::Display) -> Error {
    Error::new(
        StatusCode::BAD_REQUEST,
        "InvalidPolicyDocument",
        format!(
            "The content of the form does not meet the conditions specified in the policy document. {details}"
        ),
    )
}

fn signature_error(failure: Failure) -> Error {
    match failure {
        Failure::UnknownSigner => Error::new(
            StatusCode::FORBIDDEN,
            "InvalidAccessKeyId",
            "The policy signer or its RSA public key is unknown; register the x-goog-credential identity and its RSA public_keys in the identity file (for example, local-admin@example.invalid). HMAC/GOOG4-HMAC-SHA256 policies are unsupported.",
        ),
        Failure::Mismatch => Error::new(
            StatusCode::FORBIDDEN,
            "SignatureDoesNotMatch",
            "The policy signature does not match.",
        ),
        Failure::Malformed | Failure::Expired | Failure::NotYetValid => invalid(
            "Invalid V4 policy credentials; use x-goog-algorithm GOOG4-RSA-SHA256, x-goog-credential SIGNER/YYYYMMDD/auto/storage/goog4_request matching x-goog-date YYYYMMDDTHHMMSSZ, and a hex x-goog-signature.",
        ),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Form {
    fields: BTreeMap<String, String>,
    filename: Option<String>,
}

/// Parse `form-data; name="..."; filename="..."`. Field names compare case-insensitively.
fn disposition(headers: &HeaderMap) -> Result<(String, Option<String>), Error> {
    let malformed =
        || invalid("Each form part requires Content-Disposition: form-data; name=\"FIELD\".");
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
            (value, &tail[end + 1..])
        } else {
            let end = tail.find(';').unwrap_or(tail.len());
            (tail[..end].trim().to_owned(), &tail[end..])
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
    Ok((name.to_ascii_lowercase(), filename))
}

async fn read_form<B: Body<Data = Bytes> + Unpin>(
    parser: &mut json::Multipart<B>,
) -> Result<Form, Error> {
    parser.start().await?;
    let mut fields = BTreeMap::new();
    let mut total = 0;
    loop {
        if parser.closed() {
            return Err(invalid(
                "The form requires a file field; send it as the last form field.",
            ));
        }
        let (name, filename) = disposition(&parser.part_headers().await?)?;
        if name == "file" {
            return Ok(Form { fields, filename });
        }
        if fields.len() == FIELD_LIMIT {
            return Err(invalid(format!(
                "The form has more than {FIELD_LIMIT} fields before the file field."
            )));
        }
        let mut value = Vec::new();
        while let Some(chunk) = parser.next_chunk().await? {
            if value.len() + chunk.len() > FIELD_CAP || total + value.len() + chunk.len() > FORM_CAP
            {
                return Err(invalid(format!(
                    "Form fields before the file field are limited to {FIELD_CAP} bytes each and {FORM_CAP} bytes in total."
                )));
            }
            value.extend_from_slice(&chunk);
        }
        total += value.len();
        let value = String::from_utf8(value)
            .map_err(|_| invalid(format!("Form field {name} must be UTF-8.")))?;
        if fields.insert(name.clone(), value).is_some() {
            return Err(invalid(format!("Duplicate form field {name}.")));
        }
    }
}

fn decode_base64(raw: &str) -> Option<Vec<u8>> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let digit = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let chunks = bytes.len() / 4;
    let mut decoded = Vec::with_capacity(chunks * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let padding = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != chunks) {
            return None;
        }
        let mut value = 0u32;
        for (offset, byte) in chunk.iter().enumerate() {
            let bits = if offset >= 4 - padding {
                0
            } else {
                digit(*byte)?
            };
            value = value << 6 | u32::from(bits);
        }
        decoded.extend_from_slice(&value.to_be_bytes()[1..4 - padding]);
    }
    Some(decoded)
}

/// The documented format is ISO 8601 basic; the official SDKs send RFC 3339 instead.
fn expiration(raw: &str) -> Option<SystemTime> {
    if let Ok(time) = auth::signature::parse_timestamp(raw) {
        return Some(time);
    }
    let micros = crate::import::codec::parse_timestamp(raw).ok()?;
    if micros >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_micros(micros as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_micros(micros.unsigned_abs()))
    }
}

/// Fields that need no condition: the signature, the policy itself and the file part.
fn unconditioned(field: &str) -> bool {
    matches!(field, "x-goog-signature" | "policy")
}

/// Check every condition against the submitted form and require a condition for every
/// field. Returns the inclusive content-length-range, if any.
///
/// `signed_at` is the x-goog-date of a verified signature. It is absent in the permissive
/// profile, which ignores policy credentials.
fn check_policy(
    encoded: &str,
    fields: &BTreeMap<String, String>,
    bucket: &str,
    now: SystemTime,
    signed_at: Option<SystemTime>,
) -> Result<Option<(u64, u64)>, Error> {
    let document = decode_base64(encoded)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .ok_or_else(|| policy_error("The policy field must be base64-encoded UTF-8 JSON."))?;
    let document = document
        .as_object()
        .ok_or_else(|| policy_error("The policy document must be a JSON object."))?;
    if let Some(key) = document
        .keys()
        .find(|key| !matches!(key.as_str(), "expiration" | "conditions"))
    {
        return Err(policy_error(format!(
            "Unsupported policy document field {key}."
        )));
    }
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
            .checked_add(Duration::from_secs(900))
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
    let conditions = document
        .get("conditions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| policy_error("The policy conditions must be an array."))?;
    let mut covered = BTreeSet::new();
    let mut range = None;
    for condition in conditions {
        let unsupported = || policy_error(format!("Unsupported policy condition {condition}."));
        let (operator, field, expected) = match condition {
            serde_json::Value::Object(entry) if entry.len() == 1 => {
                let Some((field, value)) = entry.iter().next() else {
                    return Err(unsupported());
                };
                (
                    "eq",
                    field.to_ascii_lowercase(),
                    value.as_str().ok_or_else(unsupported)?,
                )
            }
            serde_json::Value::Array(items) if items.len() == 3 => {
                let operator = items[0].as_str().ok_or_else(unsupported)?;
                if operator.eq_ignore_ascii_case("content-length-range") {
                    let (min, max) = items[1]
                        .as_u64()
                        .zip(items[2].as_u64())
                        .filter(|(min, max)| min <= max)
                        .ok_or_else(unsupported)?;
                    if range.replace((min, max)).is_some() {
                        return Err(policy_error(
                            "The policy document has more than one content-length-range condition.",
                        ));
                    }
                    covered.insert("content-length".to_owned());
                    continue;
                }
                let operator = if operator.eq_ignore_ascii_case("eq") {
                    "eq"
                } else if operator.eq_ignore_ascii_case("starts-with") {
                    "starts-with"
                } else {
                    return Err(unsupported());
                };
                let field = items[1]
                    .as_str()
                    .and_then(|field| field.strip_prefix('$'))
                    .filter(|field| !field.is_empty())
                    .ok_or_else(unsupported)?;
                (
                    operator,
                    field.to_ascii_lowercase(),
                    items[2].as_str().ok_or_else(unsupported)?,
                )
            }
            _ => return Err(unsupported()),
        };
        // The bucket condition applies to the request target, whether or not a field names it.
        let actual = if field == "bucket" {
            bucket
        } else {
            fields
                .get(&field)
                .ok_or_else(|| policy_error(format!("Missing key: {field}")))?
        };
        let matched = if operator == "eq" {
            actual == expected
        } else {
            actual.starts_with(expected)
        };
        if !matched {
            return Err(policy_error(format!(
                "Policy condition failed: [\"{operator}\", \"${field}\", {}]",
                serde_json::Value::from(expected)
            )));
        }
        covered.insert(field);
    }
    if !covered.contains("bucket") {
        return Err(policy_error(
            "The policy document requires a bucket condition.",
        ));
    }
    if let Some(field) = fields
        .keys()
        .find(|field| !unconditioned(field) && !covered.contains(*field))
    {
        return Err(policy_error(format!(
            "Form field {field} has no policy condition."
        )));
    }
    Ok(range)
}

enum Success {
    Status(StatusCode),
    /// The validated `success_action_redirect`, already in its `Location` header form.
    Redirect(HeaderValue),
}

struct Plan {
    name: String,
    content_type: String,
    metadata: ObjectMetadataUpdate,
    declared_length: Option<u64>,
    success: Success,
}

fn header_value(field: &str, value: &str) -> Result<String, Error> {
    if value
        .bytes()
        .any(|byte| byte.is_ascii_control() && byte != b'\t')
    {
        return Err(invalid(format!(
            "Form field {field} must not contain control characters."
        )));
    }
    Ok(value.to_owned())
}

/// Map form fields onto the shared upload request. Unknown or unsupported fields fail
/// instead of being stored or ignored.
fn plan(form: &Form, bucket: &str) -> Result<Plan, Error> {
    let fields = &form.fields;
    for name in fields.keys() {
        match name.as_str() {
            "key"
            | "bucket"
            | "policy"
            | "x-goog-algorithm"
            | "x-goog-credential"
            | "x-goog-date"
            | "x-goog-signature"
            | "success_action_status"
            | "success_action_redirect"
            | "content-type"
            | "cache-control"
            | "content-disposition"
            | "content-encoding"
            | "content-length"
            | "x-goog-custom-time" => {}
            "googleaccessid" | "signature" => {
                return Err(Error::unsupported(
                    "V2 POST policy form fields GoogleAccessId and signature; sign a V4 policy (GOOG4-RSA-SHA256)",
                ));
            }
            name if name.len() > "x-goog-meta-".len() && name.starts_with("x-goog-meta-") => {}
            name => return Err(Error::unsupported(&format!("POST form field {name}"))),
        }
    }
    let credentials = [
        "x-goog-algorithm",
        "x-goog-credential",
        "x-goog-date",
        "x-goog-signature",
    ];
    if fields.contains_key("policy") {
        if let Some(missing) = credentials.iter().find(|name| !fields.contains_key(**name)) {
            return Err(invalid(format!(
                "Form field {missing} is required with policy."
            )));
        }
    } else if let Some(present) = credentials.iter().find(|name| fields.contains_key(**name)) {
        return Err(invalid(format!(
            "Form field {present} requires a policy field."
        )));
    }
    if fields.get("bucket").is_some_and(|value| value != bucket) {
        return Err(invalid(
            "The bucket form field must match the bucket in the request URL.",
        ));
    }
    let key = fields
        .get("key")
        .filter(|key| !key.is_empty())
        .ok_or_else(|| invalid("The form requires a non-empty key field."))?;
    let name = if key.contains("${filename}") {
        let filename = form
            .filename
            .as_deref()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                invalid("The key uses ${filename}, but the file part has no filename.")
            })?;
        key.replace("${filename}", filename)
    } else {
        key.clone()
    };
    json::validate_object_name(&name)?;
    let success = if let Some(url) = fields.get("success_action_redirect") {
        let location = url
            .parse()
            .ok()
            .filter(|uri: &Uri| {
                matches!(uri.scheme_str(), Some("http" | "https")) && uri.authority().is_some()
            })
            .and_then(|uri| HeaderValue::try_from(uri.to_string()).ok())
            .ok_or_else(|| invalid("success_action_redirect must be an absolute HTTP(S) URL."))?;
        Success::Redirect(location)
    } else {
        match fields.get("success_action_status").map(String::as_str) {
            None | Some("204") => Success::Status(StatusCode::NO_CONTENT),
            Some("200") => Success::Status(StatusCode::OK),
            Some("201") => Success::Status(StatusCode::CREATED),
            Some(_) => {
                return Err(invalid("success_action_status must be 200, 201 or 204."));
            }
        }
    };
    let mut selected = HeaderMap::new();
    if let Some(value) = fields.get("content-type") {
        selected.insert(
            "content-type",
            value
                .parse()
                .map_err(|_| invalid("Invalid Content-Type form field."))?,
        );
    }
    let (content_type, _) =
        json::parse_object_upload_headers(&selected, ObjectConditions::default())?;
    let declared_length = fields
        .get("content-length")
        .map(|raw| {
            raw.parse::<u64>()
                .ok()
                .filter(|_| raw.bytes().all(|byte| byte.is_ascii_digit()))
                .ok_or_else(|| invalid("Invalid Content-Length form field."))
        })
        .transpose()?;
    let mut metadata = ObjectMetadataUpdate::default();
    for (field, target) in [
        ("cache-control", &mut metadata.cache_control),
        ("content-disposition", &mut metadata.content_disposition),
        ("content-encoding", &mut metadata.content_encoding),
    ] {
        if let Some(value) = fields.get(field) {
            *target = Some(Some(header_value(field, value)?));
        }
    }
    if let Some(value) = fields.get("x-goog-custom-time") {
        custom_time_key(value).map_err(|_| {
            invalid("x-goog-custom-time must be RFC 3339, for example 2026-10-20T01:02:03Z.")
        })?;
        metadata.custom_time = Some(Some(value.clone()));
    }
    let mut custom = BTreeMap::new();
    for (field, value) in fields {
        if let Some(key) = field.strip_prefix("x-goog-meta-") {
            custom.insert(key.to_owned(), Some(header_value(field, value)?));
        }
    }
    if custom
        .iter()
        .map(|(key, value)| key.len() + value.as_ref().map_or(0, String::len))
        .sum::<usize>()
        > CUSTOM_METADATA_CAP
    {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "MetadataTooLarge",
            "Custom metadata exceeds 8 KiB.",
        ));
    }
    metadata.metadata = Some(Some(custom));
    Ok(Plan {
        name,
        content_type,
        metadata,
        declared_length,
        success,
    })
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) async fn respond(
    request: Request<Incoming>,
    object: &ObjectPath,
    state: &State,
    config: &Config,
    activity: &watch::Sender<ObjectActivity>,
    deadline: tokio::time::Instant,
) -> Result<Response<ResponseBody>, Error> {
    let bucket = object.bucket.clone();
    json::validate_bucket_name_for_profile(&bucket, config.compat_profile)?;
    // POST Object takes every option from the form; query options belong to other operations.
    if let Some(query) = request.uri().query() {
        let key = query.split('&').next().unwrap_or_default();
        return Err(Error::unsupported(&format!(
            "POST /BUCKET query parameter {}; send POST Object options as form fields",
            key.split('=').next().unwrap_or_default()
        )));
    }
    let boundary = json::form_data_boundary(request.headers()).map_err(|_| {
        invalid("POST Object requires Content-Type: multipart/form-data with a valid boundary.")
    })?;
    let headers = request.headers().clone();
    let body = request.into_body().map_frame(|frame| {
        body_progress(activity);
        frame
    });
    let mut parser = json::Multipart::new(body, &boundary);
    let form = read_form(&mut parser).await?;
    let Plan {
        name,
        content_type,
        metadata,
        declared_length,
        success,
    } = plan(&form, &bucket)?;
    let fields = &form.fields;
    let caller = if let Some(policy) = fields.get("policy") {
        if headers.contains_key("authorization") {
            return Err(invalid(
                "Send either a policy form or an Authorization header, not both.",
            ));
        }
        state
            .auth
            .authenticate_policy(
                &fields["x-goog-algorithm"],
                &fields["x-goog-credential"],
                &fields["x-goog-date"],
                &fields["x-goog-signature"],
                policy,
            )
            .map_err(signature_error)?
    } else {
        state
            .auth
            .authenticate(&headers, None)
            .map_err(|error| Error::from(JsonError::from(error)))?
    };
    let range = fields
        .get("policy")
        .map(|policy| {
            let signed_at = caller
                .is_some()
                .then(|| signature::parse_timestamp(&fields["x-goog-date"]).ok())
                .flatten();
            check_policy(policy, fields, &bucket, state.clock.now(), signed_at)
        })
        .transpose()?
        .flatten();
    let access = authorize_bucket(state, caller, auth::OBJECTS_CREATE, &bucket)
        .await
        .map_err(|error| anonymous_error(error, caller))?;
    let max_bytes = config.max_object_bytes;
    let timeout = Duration::from_millis(config.catalog_wait_timeout_ms);
    let permit = await_upload_catalog(
        state
            .catalog
            .prepare_object_upload(&bucket, &name, ObjectConditions::default(), &access),
        timeout,
        deadline,
        activity,
    )
    .await
    .map_err(|error| storage_error(error, caller))?;
    let (sender, receiver) = mpsc::channel(2);
    let upload = state.catalog.put_object_prechecked(
        &bucket,
        &name,
        content_type,
        ExpectedHashes::default(),
        ObjectConditions::default(),
        max_bytes,
        receiver,
        permit,
        metadata,
        access,
    );
    tokio::pin!(upload);
    let streaming = async {
        let mut size = 0_u64;
        while let Some(bytes) = parser.next_chunk().await? {
            size = size
                .checked_add(bytes.len() as u64)
                .filter(|size| *size <= max_bytes)
                .ok_or_else(|| storage_error(ObjectError::TooLarge, None))?;
            if range.is_some_and(|(_, max)| size > max) {
                return Err(Error::new(
                    StatusCode::BAD_REQUEST,
                    "EntityTooLarge",
                    "The file exceeds the policy content-length-range maximum.",
                ));
            }
            if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
                // The catalog's own failure is reported by the select below.
                return Ok(());
            }
        }
        if !parser.closed() {
            return Err(invalid(
                "The file field must be the last field in the form.",
            ));
        }
        if range.is_some_and(|(min, _)| size < min) {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "EntityTooSmall",
                "The file is smaller than the policy content-length-range minimum.",
            ));
        }
        if declared_length.is_some_and(|length| length != size) {
            return Err(invalid(
                "The Content-Length form field does not match the file size.",
            ));
        }
        parser.finish().await?;
        Ok(())
    };
    let record = tokio::select! {
        result = &mut upload => result.map_err(|error| storage_error(error, caller))?,
        result = streaming => {
            if let Err(error) = result {
                let _ = sender.send(Err(io::Error::other(error.message.clone()))).await;
                drop(sender);
                // Await cleanup so a rejected form never leaves a partial object behind.
                match await_upload_catalog(upload, timeout, deadline, activity).await {
                    Ok(_) | Err(ObjectError::BodyAborted(_)) => {}
                    Err(storage) => return Err(storage_error(storage, caller)),
                }
                return Err(error);
            }
            await_upload_catalog(async {
                let _ = sender.send(Ok(UploadChunk::End)).await;
                drop(sender);
                upload.await
            }, timeout, deadline, activity)
            .await
            .map_err(|error| storage_error(error, caller))?
        }
    };
    let mut media = super::media_headers(&record, super::CacheScope::Private)?;
    let etag = media["etag"].to_str().unwrap_or_default().to_owned();
    let (status, body, content_type) = match success {
        Success::Redirect(_) => (StatusCode::SEE_OTHER, String::new(), None),
        Success::Status(StatusCode::CREATED) => (
            StatusCode::CREATED,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><PostResponse><Location>{}</Location><Bucket>{}</Bucket><Key>{}</Key><ETag>{}</ETag></PostResponse>",
                escape(&format!(
                    "{}/{}/{}",
                    state.api_base_url.trim_end_matches('/'),
                    record.bucket,
                    crate::url::encode_object_name(&record.name)
                )),
                escape(&record.bucket),
                escape(&record.name),
                escape(&etag)
            ),
            Some("application/xml; charset=UTF-8"),
        ),
        Success::Status(status) => (status, String::new(), None),
    };
    let content_length = HeaderValue::from(body.len());
    let mut response = boxed(Response::new(Full::new(Bytes::from(body))));
    *response.status_mut() = status;
    if let Success::Redirect(location) = success {
        response.headers_mut().insert("location", location);
    }
    if let Some(content_type) = content_type {
        let headers = response.headers_mut();
        headers.insert("content-type", HeaderValue::from_static(content_type));
        headers.insert("content-length", content_length);
    }
    for name in [
        "etag",
        "x-goog-generation",
        "x-goog-metageneration",
        "x-goog-hash",
        "x-goog-stored-content-length",
        "x-goog-stored-content-encoding",
    ] {
        if let Some(value) = media.remove(name) {
            response.headers_mut().insert(name, value);
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;

    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();
        for chunk in bytes.chunks(3) {
            let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
                value | u32::from(*byte) << (16 - 8 * index)
            });
            for index in 0..4 {
                result.push(if index <= chunk.len() {
                    ALPHABET[(value >> (18 - 6 * index) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        result
    }

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
        (error.status, error.code, error.message)
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
                check_policy(&encoded, &sdk_fields(), "b", at(NOW), None).unwrap(),
                None,
                "{expiration}"
            );
        }
    }

    #[test]
    fn verified_policy_uses_the_signed_url_validity_window() {
        let seven_days = "2026-11-05T00:00:00Z";
        let encoded = policy(seven_days, sdk_conditions());
        let fields = sdk_fields();
        check_policy(&encoded, &fields, "b", at(NOW), Some(at(NOW))).unwrap();
        check_policy(&encoded, &fields, "b", at(NOW - 900), Some(at(NOW))).unwrap();
        // The permissive profile ignores credentials, so only the policy expiration applies.
        let beyond = policy("2026-11-05T00:00:01Z", sdk_conditions());
        check_policy(&beyond, &fields, "b", at(NOW), None).unwrap();
        for (encoded, now, expected) in [
            (&beyond, NOW, "no more than 7 days"),
            (&encoded, NOW - 901, "Policy not yet valid"),
        ] {
            let (status, code, message) =
                details(check_policy(encoded, &fields, "b", at(now), Some(at(NOW))).unwrap_err());
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
            details(check_policy(&encoded, &sdk_fields(), "b", at(NOW), None).unwrap_err());
        assert_eq!(
            (status, code),
            (StatusCode::BAD_REQUEST, "InvalidPolicyDocument")
        );
        assert!(message.contains("Policy expired"), "{message}");
        check_policy(&encoded, &sdk_fields(), "b", at(NOW - 1), None).unwrap();
        for encoded in [
            "not base64".to_owned(),
            base64(b"[]"),
            encode(&serde_json::json!({"conditions": []})),
            encode(&serde_json::json!({"expiration": "tomorrow", "conditions": []})),
            encode(&serde_json::json!({"expiration": "2026-10-30T00:00:00Z"})),
            encode(
                &serde_json::json!({"expiration": "2026-10-30T00:00:00Z", "conditions": [], "extra": 1}),
            ),
        ] {
            assert_eq!(
                check_policy(&encoded, &sdk_fields(), "b", at(NOW), None)
                    .unwrap_err()
                    .code,
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
        check_policy(
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
            let error = check_policy(
                &policy(expiration, conditions),
                &form,
                bucket,
                at(NOW),
                None,
            )
            .unwrap_err();
            assert_eq!(error.code, "InvalidPolicyDocument");
            assert!(error.message.contains(expected), "{}", error.message);
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
            check_policy(
                &policy(expiration, conditions.clone()),
                &form,
                "b",
                at(NOW),
                None
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
                check_policy(&policy(expiration, conditions), &form, "b", at(NOW), None)
                    .unwrap_err()
                    .code,
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
        let planned = plan(
            &form(
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
            ),
            "b",
        )
        .unwrap();
        assert_eq!(planned.name, "uploads/photo.png");
        assert_eq!(planned.content_type, "text/plain");
        assert_eq!(
            planned.metadata.cache_control,
            Some(Some("no-cache".into()))
        );
        assert_eq!(
            planned.metadata.metadata,
            Some(Some(BTreeMap::from([(
                "owner".into(),
                Some("alice".into())
            )])))
        );
        assert!(matches!(
            planned.success,
            Success::Status(StatusCode::CREATED)
        ));
        let defaults = plan(&form(&[("key", "a")], None), "b").unwrap();
        assert_eq!(defaults.content_type, "application/octet-stream");
        assert!(matches!(
            defaults.success,
            Success::Status(StatusCode::NO_CONTENT)
        ));
        let redirect = plan(
            &form(
                &[
                    ("key", "a"),
                    (
                        "success_action_redirect",
                        "https://example.invalid/done?x=1",
                    ),
                    ("success_action_status", "201"),
                ],
                None,
            ),
            "b",
        )
        .unwrap();
        assert!(
            matches!(redirect.success, Success::Redirect(url) if url == "https://example.invalid/done?x=1")
        );
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
                error.map(|error| error.status),
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
            ("content-type".into(), None)
        );
        assert_eq!(
            parse("Form-Data; name=file; filename=\"a \\\"b\\\";c.txt\"").unwrap(),
            ("file".into(), Some("a \"b\";c.txt".into()))
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
        let mut parser = json::Multipart::new(Full::new(Bytes::from_static(body.as_bytes())), "b");
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
                parse_form(body).await.unwrap_err().status,
                StatusCode::BAD_REQUEST
            );
        }
        let mut long = "--b\r\nContent-Disposition: form-data; name=\"key\"\r\n\r\n".to_owned();
        long.push_str(&"x".repeat(FIELD_CAP + 1));
        long.push_str(
            "\r\n--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nx\r\n--b--\r\n",
        );
        let mut parser = json::Multipart::new(Full::new(Bytes::from(long)), "b");
        assert!(
            read_form(&mut parser)
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
