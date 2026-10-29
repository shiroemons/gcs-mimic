use super::{
    JsonError, Projection,
    buckets::{encode_segment, timestamp, validate_bucket_name_for_profile},
    query::{boolean, decode, finish, parse_query, projection},
};
use crate::{
    config::CompatProfile,
    object::{ExpectedHashes, ObjectConditions, ObjectRecord},
};
use hyper::{HeaderMap, Method, StatusCode, Uri, header::HeaderValue};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectInsertParams {
    pub conditions: ObjectConditions,
    pub projection: Projection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectGetParams {
    pub generation: Option<i64>,
    pub conditions: ObjectConditions,
    pub projection: Projection,
    pub media: bool,
    pub soft_deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectRequest {
    Restore {
        bucket: String,
        name: String,
        generation: i64,
        conditions: ObjectConditions,
        projection: Projection,
    },
    Move(super::ObjectMoveParams),
    Copy(super::ObjectCopyParams),
    Compose {
        bucket: String,
        name: String,
        conditions: ObjectConditions,
    },
    List(super::ObjectListParams),
    Update {
        bucket: String,
        name: String,
        params: ObjectGetParams,
        replace: bool,
        override_unlocked_retention: bool,
    },
    Insert {
        bucket: String,
        name: String,
        params: ObjectInsertParams,
    },
    Multipart {
        bucket: String,
        name: Option<String>,
        params: ObjectInsertParams,
    },
    Resumable {
        bucket: String,
        name: Option<String>,
        params: ObjectInsertParams,
        content_encoding: Option<String>,
    },
    Get {
        bucket: String,
        name: String,
        params: ObjectGetParams,
    },
    Delete {
        bucket: String,
        name: String,
        generation: Option<i64>,
        conditions: ObjectConditions,
    },
}

/// Match raw path segments before decoding object names exactly once.
pub fn route_object(method: &Method, uri: &Uri) -> Option<Result<ObjectRequest, JsonError>> {
    route_object_with_profile(method, uri, CompatProfile::Native)
}

pub fn route_object_with_profile(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<ObjectRequest, JsonError>> {
    if let Some(operation) = super::object_restore::route(method, uri, profile) {
        return Some(operation);
    }
    if let Some(operation) = super::object_move::route(method, uri, profile) {
        return Some(operation);
    }
    if let Some(operation) = super::object_copy::route(method, uri, profile) {
        return Some(operation);
    }
    if let Some(operation) = super::compose::route(method, uri, profile) {
        return Some(operation);
    }
    let (path, upload, download) =
        if let Some(path) = uri.path().strip_prefix("/upload/storage/v1/b/") {
            (path, true, false)
        } else if let Some(path) = uri.path().strip_prefix("/download/storage/v1/b/") {
            // Discovery uses useMediaDownloadService; the JSON status reference specifies /download.
            (path, false, true)
        } else {
            (uri.path().strip_prefix("/storage/v1/b/")?, false, false)
        };
    let (bucket, rest) = path.split_once('/')?;
    let object = if upload {
        if rest != "o" {
            return None;
        }
        None
    } else if !download && rest == "o" {
        // Metadata-only objects.insert remains an unimplemented route.
        if method == Method::POST {
            return None;
        }
        None
    } else {
        let segment = rest.strip_prefix("o/")?;
        if segment.contains('/') {
            return None;
        }
        Some(segment)
    };
    let allowed = if upload {
        method == Method::POST
    } else {
        method == Method::GET
            || (method == Method::HEAD && object.is_some() && !download)
            || (!download
                && object.is_some()
                && matches!(*method, Method::DELETE | Method::PATCH | Method::PUT))
    };
    if !allowed {
        let mut error = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "HTTP method is not supported for this object endpoint",
        );
        error.allow = Some(if upload {
            "POST"
        } else if download || object.is_none() {
            "GET"
        } else {
            "GET, HEAD, DELETE, PATCH, PUT"
        });
        return Some(Err(error));
    }
    Some((|| {
        if bucket.len() > 222 * 3 {
            return Err(JsonError::invalid_parameter("Invalid bucket name"));
        }
        let bucket = decode(bucket, false)?;
        validate_bucket_name_for_profile(&bucket, profile)?;
        if !upload && object.is_none() {
            return super::object_list::parse_list(bucket, uri.query().unwrap_or_default())
                .map(ObjectRequest::List);
        }
        let name = object
            .map(|segment| {
                if segment.len() > 1024 * 3 {
                    return Err(JsonError::invalid_parameter("Invalid object name"));
                }
                let name = decode(segment, false)?;
                validate_object_name(&name)?;
                Ok(name)
            })
            .transpose()?;
        let operation = parse_object_query(method, bucket, name, uri.query().unwrap_or_default())?;
        if download && matches!(&operation, ObjectRequest::Get { params, .. } if !params.media) {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "notDownload",
                "Download endpoints require alt=media",
            ));
        }
        Ok(operation)
    })())
}

/// Validate the flat-namespace requirements, not optional CLI/filesystem recommendations.
pub fn validate_object_name(name: &str) -> Result<(), JsonError> {
    if name.is_empty()
        || name.len() > 1024
        || name.contains(['\r', '\n'])
        || matches!(name, "." | "..")
        || name.starts_with(".well-known/acme-challenge/")
    {
        return Err(JsonError::invalid_parameter("Invalid object name"));
    }
    Ok(())
}

fn parse_object_query(
    method: &Method,
    bucket: String,
    name: Option<String>,
    raw: &str,
) -> Result<ObjectRequest, JsonError> {
    let mut query = parse_query(raw)?;
    if method == Method::DELETE {
        query.remove("fields");
    } else if query.contains_key("fields") && query.get("alt").is_some_and(|v| v == "media") {
        return Err(JsonError::unsupported_object("fields with alt=media"));
    } else {
        super::Fields::take(&mut query, "Object")?;
    }
    for key in [
        "key",
        "oauth_token",
        "quotaUser",
        "userIp",
        "userProject",
        "callback",
    ] {
        if query.contains_key(key) {
            return Err(JsonError::unsupported_object(key));
        }
    }
    let resumable =
        method == Method::POST && query.get("uploadType").is_some_and(|v| v == "resumable");
    let content_encoding = if resumable {
        query.remove("contentEncoding")
    } else {
        None
    };
    let method_keys: &[&str] = if method == Method::POST {
        query.remove("predefinedAcl");
        &["contentEncoding", "kmsKeyName"]
    } else if matches!(*method, Method::GET | Method::HEAD) {
        &["restoreToken"]
    } else if method == Method::PATCH || method == Method::PUT {
        &["predefinedAcl"]
    } else {
        &[]
    };
    for key in method_keys {
        if query.contains_key(*key) {
            return Err(JsonError::unsupported_object(key));
        }
    }
    if boolean(&mut query, "prettyPrint")? {
        return Err(JsonError::unsupported_object("prettyPrint"));
    }
    let media = match query.remove("alt").as_deref() {
        None | Some("json") => false,
        Some("media") if method == Method::GET => true,
        Some("media") => return Err(JsonError::invalid_parameter("alt=media requires GET")),
        Some(_) => {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "invalidAltValue",
                "alt must be json or media",
            ));
        }
    };
    let conditions = ObjectConditions {
        if_generation_match: integer(&mut query, "ifGenerationMatch")?,
        if_generation_not_match: integer(&mut query, "ifGenerationNotMatch")?,
        if_metageneration_match: integer(&mut query, "ifMetagenerationMatch")?,
        if_metageneration_not_match: integer(&mut query, "ifMetagenerationNotMatch")?,
    };
    if method == Method::POST {
        let multipart = match query.remove("uploadType").as_deref() {
            Some("media") => false,
            Some("multipart") => true,
            Some("resumable") => true,
            None => {
                return Err(JsonError::new(
                    StatusCode::BAD_REQUEST,
                    "required",
                    "Missing required uploadType",
                ));
            }
            Some(_) => {
                return Err(JsonError::invalid_parameter(
                    "uploadType must be media, multipart or resumable",
                ));
            }
        };
        let name = query.remove("name");
        if let Some(name) = &name {
            validate_object_name(name)?;
        }
        if !multipart && name.is_none() {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "required",
                "Missing required name",
            ));
        }
        let projection = projection(&mut query)?;
        finish(query)?;
        let params = ObjectInsertParams {
            conditions,
            projection,
        };
        if resumable {
            if conditions.if_generation_match.is_some_and(|n| n < 0)
                || conditions.if_generation_not_match.is_some_and(|n| n < 0)
                || conditions.if_metageneration_match.is_some_and(|n| n <= 0)
                || conditions
                    .if_metageneration_not_match
                    .is_some_and(|n| n <= 0)
            {
                return Err(JsonError::invalid_parameter("Invalid upload precondition"));
            }
            return Ok(ObjectRequest::Resumable {
                bucket,
                name,
                params,
                content_encoding,
            });
        }
        if multipart {
            return Ok(ObjectRequest::Multipart {
                bucket,
                name,
                params,
            });
        }
        let name = name.expect("media upload requires a query name");
        return Ok(ObjectRequest::Insert {
            bucket,
            name,
            params,
        });
    }
    let generation = integer(&mut query, "generation")?;
    let name = name.expect("non-upload routes have an object segment");
    if method == Method::PATCH || method == Method::PUT {
        let projection = if method == Method::PUT && !query.contains_key("projection") {
            Projection::Full
        } else {
            projection(&mut query)?
        };
        let override_unlocked_retention = boolean(&mut query, "overrideUnlockedRetention")?;
        finish(query)?;
        return Ok(ObjectRequest::Update {
            bucket,
            name,
            override_unlocked_retention,
            replace: method == Method::PUT,
            params: ObjectGetParams {
                generation,
                conditions,
                projection,
                media: false,
                soft_deleted: false,
            },
        });
    }
    if matches!(*method, Method::GET | Method::HEAD) {
        let soft_deleted = boolean(&mut query, "softDeleted")?;
        if soft_deleted && (generation.is_none() || media) {
            return Err(JsonError::invalid_argument(
                "softDeleted=true requires generation and forbids alt=media; use generation=123&softDeleted=true",
            ));
        }
        let projection = projection(&mut query)?;
        finish(query)?;
        Ok(ObjectRequest::Get {
            bucket,
            name,
            params: ObjectGetParams {
                generation,
                conditions,
                projection,
                media,
                soft_deleted,
            },
        })
    } else {
        finish(query)?;
        Ok(ObjectRequest::Delete {
            bucket,
            name,
            generation,
            conditions,
        })
    }
}

pub(super) fn integer(
    query: &mut BTreeMap<String, String>,
    key: &str,
) -> Result<Option<i64>, JsonError> {
    query
        .remove(key)
        .map(|value| {
            if value.starts_with('+') {
                return Err(JsonError::invalid_parameter(format!(
                    "{key} must be an int64"
                )));
            }
            value
                .parse()
                .map_err(|_| JsonError::invalid_parameter(format!("{key} must be an int64")))
        })
        .transpose()
}

/// `keys` lists the customer-supplied key header prefixes the caller parses itself.
fn unsupported_headers(headers: &HeaderMap, keys: &[&str]) -> Result<(), JsonError> {
    let mut tokens = headers.get_all("x-goog-gcs-idempotency-token").iter();
    if let Some(token) = tokens.next()
        && (token.as_bytes().trim_ascii().is_empty() || tokens.next().is_some())
    {
        return Err(JsonError::invalid_parameter(
            "Expected a single non-empty x-goog-gcs-idempotency-token header",
        ));
    }
    for key in headers.keys() {
        let name = key.as_str();
        if keys
            .iter()
            .any(|prefix| crate::csek::is_key_header(name, prefix))
        {
            continue;
        }
        if name.starts_with(crate::csek::DESTINATION_PREFIX)
            || name.starts_with(crate::csek::SOURCE_PREFIX)
            || name.starts_with("x-goog-meta-")
            || matches!(name, "x-goog-user-project" | "x-http-method-override")
        {
            return Err(JsonError::unsupported_object(name));
        }
    }
    Ok(())
}

/// Parse upload metadata without consuming or collecting the body. Callers read the
/// destination customer-supplied key separately with [`super::request_key`].
pub fn parse_object_upload_headers(
    headers: &HeaderMap,
    _conditions: ObjectConditions,
) -> Result<(String, ExpectedHashes), JsonError> {
    unsupported_headers(headers, &[crate::csek::DESTINATION_PREFIX])?;
    validate_read_condition_headers(headers, false)?;
    if let Some(mime) = super::resumable::single(headers, "x-upload-content-type")? {
        crate::object::validate_fixed_metadata("contentType", mime)?;
        let mut advisory = HeaderMap::new();
        advisory.insert(
            "content-type",
            mime.parse()
                .map_err(|_| JsonError::invalid_parameter("Invalid upload content type"))?,
        );
        parse_object_upload_headers(&advisory, ObjectConditions::default())?;
    }
    for name in [
        "content-encoding",
        "content-md5",
        "content-range",
        "x-upload-content-length",
    ] {
        if headers.contains_key(name) {
            return Err(JsonError::unsupported_object(name));
        }
    }
    if headers.get_all("content-type").iter().count() > 1 {
        return Err(JsonError::invalid_parameter(
            "Duplicate Content-Type header",
        ));
    }
    let content_type = headers
        .get("content-type")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| JsonError::invalid_parameter("Invalid Content-Type header"))
        })
        .transpose()?
        .unwrap_or("application/octet-stream");
    let mime = content_type.split(';').next().unwrap_or_default().trim();
    let token = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    };
    if !mime
        .split_once('/')
        .is_some_and(|(kind, subtype)| token(kind) && token(subtype))
    {
        return Err(JsonError::invalid_parameter("Invalid Content-Type header"));
    }
    let mut hashes = ExpectedHashes::default();
    // JSON media uploads accept X-Goog-Hash: https://docs.cloud.google.com/storage/docs/data-validation
    for header in headers.get_all("x-goog-hash") {
        let header = header
            .to_str()
            .map_err(|_| JsonError::invalid_parameter("Invalid x-goog-hash header"))?;
        for hash in header.split(',') {
            let (algorithm, value) = hash
                .trim()
                .split_once('=')
                .ok_or_else(|| JsonError::invalid_parameter("Invalid x-goog-hash header"))?;
            match algorithm {
                "md5" if hashes.md5.is_none() => hashes.md5 = Some(decode_hash(value)?),
                "crc32c" if hashes.crc32c.is_none() => {
                    hashes.crc32c = Some(u32::from_be_bytes(decode_hash(value)?))
                }
                "md5" | "crc32c" => {
                    return Err(JsonError::invalid_parameter(
                        "Duplicate x-goog-hash algorithm",
                    ));
                }
                _ => return Err(JsonError::unsupported_object("x-goog-hash algorithm")),
            }
        }
    }
    Ok((content_type.to_owned(), hashes))
}

/// Reject unsupported encryption and range-validator headers explicitly.
// shortcut: GET/DELETE tokens do not deduplicate retries; add replay storage for strict retry fidelity.
pub fn validate_object_get_headers(headers: &HeaderMap, media: bool) -> Result<(), JsonError> {
    validate_keyed_object_headers(headers, media, &[])
}

/// As [`validate_object_get_headers`], leaving the listed key header prefixes to the caller.
pub fn validate_keyed_object_headers(
    headers: &HeaderMap,
    media: bool,
    keys: &[&str],
) -> Result<(), JsonError> {
    unsupported_headers(headers, keys)?;
    if media && headers.contains_key("if-range") {
        return Err(JsonError::unsupported_object("If-Range"));
    }
    Ok(())
}

/// Keep unsupported conditional headers explicit on routes without validators.
pub(crate) fn validate_read_condition_headers(
    headers: &HeaderMap,
    etags_supported: bool,
) -> Result<(), JsonError> {
    if etags_supported {
        for name in ["if-range", "if-modified-since", "if-unmodified-since"] {
            if (name == "if-modified-since" && headers.contains_key("if-none-match"))
                || (name == "if-unmodified-since" && headers.contains_key("if-match"))
            {
                continue;
            }
            if headers.contains_key(name) {
                return Err(JsonError::unsupported_object(name));
            }
        }
    }
    for name in ["if-match", "if-none-match"] {
        if headers.contains_key(name) && !etags_supported {
            return Err(JsonError::unsupported_object(name));
        }
    }
    Ok(())
}

/// Resolve one byte range against the selected representation; ignored ranges return None.
pub fn object_media_range(headers: &HeaderMap, size: u64) -> Result<Option<(u64, u64)>, JsonError> {
    let mut values = headers.get_all("range").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Ok(None);
    }
    let Some((unit, range)) = value
        .to_str()
        .ok()
        .and_then(|value| value.trim().split_once('='))
    else {
        return Ok(None);
    };
    if !unit.eq_ignore_ascii_case("bytes") || range.contains(',') {
        return Ok(None);
    }
    let Some((first, last)) = range.trim().split_once('-') else {
        return Ok(None);
    };
    // Saturation preserves comparisons with the bounded object size for arbitrarily large numerals.
    let number = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())).then(|| {
            value.bytes().fold(0_u64, |n, byte| {
                n.saturating_mul(10).saturating_add(u64::from(byte - b'0'))
            })
        })
    };
    let (offset, end) = if first.is_empty() {
        let Some(suffix) = number(last) else {
            return Ok(None);
        };
        // RFC 9110 permits ignoring an empty representation's satisfiable nonzero suffix.
        if size == 0 && suffix != 0 {
            return Ok(None);
        }
        (size.saturating_sub(suffix), size.saturating_sub(1))
    } else {
        let first_digits = first.trim_start_matches('0');
        let last_digits = last.trim_start_matches('0');
        if !last.is_empty() && (last_digits.len(), last_digits) < (first_digits.len(), first_digits)
        {
            return Ok(None);
        }
        let Some(first) = number(first) else {
            return Ok(None);
        };
        let last = if last.is_empty() {
            u64::MAX
        } else if let Some(last) = number(last) {
            last
        } else {
            return Ok(None);
        };
        (first, last.min(size.saturating_sub(1)))
    };
    if size == 0 || offset >= size {
        return Err(JsonError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "requestedRangeNotSatisfiable",
            "The requested Range cannot be satisfied",
        ));
    }
    Ok(Some((offset, end - offset + 1)))
}

/// Render the selected immutable generation using the configured advertised endpoint.
pub fn object_resource(
    object: &ObjectRecord,
    base_url: &str,
    projection: Projection,
) -> Result<Value, JsonError> {
    // ACL and owner fields are not modeled; both projections expose the same local subset.
    let (Projection::NoAcl | Projection::Full) = projection;
    let base = base_url.trim_end_matches('/');
    let path = format!(
        "storage/v1/b/{}/o/{}",
        encode_segment(&object.bucket),
        encode_segment(&object.name)
    );
    let mut value = json!({
        "kind": "storage#object",
        "id": format!("{}/{}/{}", object.bucket, object.name, object.generation),
        "selfLink": format!("{base}/{path}"),
        "mediaLink": format!("{base}/download/{path}?generation={}&alt=media", object.generation),
        "name": object.name, "bucket": object.bucket,
        "generation": object.generation.to_string(), "metageneration": object.metageneration.to_string(),
        "storageClass": object.storage_class,
        "size": object.size.to_string(),
        "crc32c": base64(&object.crc32c.to_be_bytes()), "etag": object_etag(object),
        "timeCreated": timestamp(object.time_created)?, "updated": timestamp(object.updated)?,
        "timeFinalized": timestamp(object.time_created)?,
        "timeStorageClassUpdated": timestamp(object.time_created)?,
    });
    if let Some(count) = object.component_count {
        value["componentCount"] = json!(count);
    } else {
        value["md5Hash"] = json!(base64(&object.md5));
    }
    if !object.content_type.is_empty() {
        value["contentType"] = json!(object.content_type);
    }
    for (key, field) in [
        ("contentEncoding", &object.content_encoding),
        ("contentDisposition", &object.content_disposition),
        ("contentLanguage", &object.content_language),
        ("cacheControl", &object.cache_control),
    ] {
        if let Some(field) = field {
            value[key] = json!(field);
        }
    }
    if !object.metadata.is_empty() {
        value["metadata"] = json!(object.metadata);
    }
    if object.event_based_hold {
        value["eventBasedHold"] = json!(true);
    }
    if object.temporary_hold {
        value["temporaryHold"] = json!(true);
    }
    if let Some(time) = super::object_retention::expiration(object.earliest_deletion_time())? {
        value["retentionExpirationTime"] = json!(time);
    }
    if let Some(retention) = &object.retention {
        value["retention"] = super::object_retention::render(retention)?;
    }
    if let Some(time) = &object.custom_time {
        value["customTime"] = json!(time);
    }
    if let Some(deleted) = object.time_deleted {
        value["timeDeleted"] = json!(timestamp(deleted)?);
    }
    if let Some(time) = object.soft_delete_time {
        value["softDeleteTime"] = json!(timestamp(time)?);
    }
    if let Some(time) = object.hard_delete_time {
        value["hardDeleteTime"] = json!(timestamp(time)?);
    }
    super::csek::render(object, &mut value);
    Ok(value)
}

/// Representation headers for an untranscoded media download.
pub fn object_media_headers(object: &ObjectRecord) -> Result<HeaderMap, JsonError> {
    let mut headers = HeaderMap::new();
    for (key, value) in [
        // The transport substitutes stored/default cache metadata only for an allUsers read grant.
        ("cache-control", "no-cache".to_owned()),
        (
            "content-type",
            if object.content_type.is_empty() {
                "application/octet-stream".into()
            } else {
                object.content_type.clone()
            },
        ),
        ("content-length", object.size.to_string()),
        ("accept-ranges", "bytes".to_owned()),
        ("x-goog-generation", object.generation.to_string()),
        ("x-goog-metageneration", object.metageneration.to_string()),
        (
            "x-goog-hash",
            if object.component_count.is_some() {
                format!("crc32c={}", base64(&object.crc32c.to_be_bytes()))
            } else {
                format!(
                    "crc32c={},md5={}",
                    base64(&object.crc32c.to_be_bytes()),
                    base64(&object.md5)
                )
            },
        ),
        ("x-goog-stored-content-length", object.size.to_string()),
        (
            "x-goog-stored-content-encoding",
            object
                .content_encoding
                .clone()
                .unwrap_or_else(|| "identity".into()),
        ),
        ("etag", object_etag(object)),
    ] {
        headers.insert(
            key,
            HeaderValue::from_str(&value).map_err(|_| {
                JsonError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Invalid stored object response header",
                )
            })?,
        );
    }
    for (key, value) in [
        ("content-encoding", &object.content_encoding),
        ("content-disposition", &object.content_disposition),
        ("content-language", &object.content_language),
    ] {
        if let Some(value) = value {
            headers.insert(
                key,
                HeaderValue::from_str(value).map_err(|_| {
                    JsonError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "backendError",
                        "Invalid stored object response header",
                    )
                })?,
            );
        }
    }
    Ok(headers)
}

/// Select gzip expansion while honoring explicit quality overrides and no-transform.
pub(crate) fn object_media_transcoding(
    headers: &HeaderMap,
    object: &ObjectRecord,
) -> Result<bool, JsonError> {
    if !object
        .content_encoding
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("gzip"))
        || object.cache_control.as_deref().is_some_and(|value| {
            value
                .split(',')
                .any(|directive| directive.trim().eq_ignore_ascii_case("no-transform"))
        })
    {
        return Ok(false);
    }
    let mut gzip = None;
    let mut wildcard = None;
    for header in headers.get_all("accept-encoding") {
        let value = header
            .to_str()
            .map_err(|_| JsonError::invalid_parameter("Invalid Accept-Encoding"))?;
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let mut parts = entry.split(';');
            let encoding = parts.next().expect("split has one part").trim();
            let mut allowed = true;
            if let Some(parameter) = parts.next() {
                let (name, quality) = parameter.trim().split_once('=').ok_or_else(|| {
                    JsonError::invalid_parameter("Invalid Accept-Encoding quality")
                })?;
                if !name.eq_ignore_ascii_case("q") || parts.next().is_some() {
                    return Err(JsonError::invalid_parameter(
                        "Invalid Accept-Encoding parameter",
                    ));
                }
                let quality = quality.trim();
                let (whole, fraction) = quality.split_once('.').unwrap_or((quality, ""));
                if !matches!(whole, "0" | "1")
                    || fraction.len() > 3
                    || !fraction.bytes().all(|byte| byte.is_ascii_digit())
                    || (whole == "1" && fraction.bytes().any(|byte| byte != b'0'))
                {
                    return Err(JsonError::invalid_parameter(
                        "Invalid Accept-Encoding quality",
                    ));
                }
                allowed = whole == "1" || fraction.bytes().any(|byte| byte != b'0');
            }
            let selected = if encoding.eq_ignore_ascii_case("gzip") {
                &mut gzip
            } else if encoding == "*" {
                &mut wildcard
            } else {
                continue;
            };
            if selected.replace(allowed).is_some() {
                return Err(JsonError::invalid_parameter(
                    "Duplicate Accept-Encoding token",
                ));
            }
        }
    }
    Ok(!gzip.or(wildcard).unwrap_or(false))
}

/// XML expansion remains outside the JSON media transcoding scope.
pub fn validate_object_media_encoding(
    headers: &HeaderMap,
    object: &ObjectRecord,
) -> Result<(), JsonError> {
    if object_media_transcoding(headers, object)? {
        Err(JsonError::unsupported_object(
            "decompressive transcoding; accept gzip or set no-transform",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn object_etag(object: &ObjectRecord) -> String {
    if object.component_count.is_some() {
        return format!("\"c-{}-{}\"", object.generation, object.metageneration);
    }
    // A keyless metadata read must not learn the plaintext MD5 through the validator.
    if object.customer_key_sha256.is_some() {
        return format!("\"e-{}-{}\"", object.generation, object.metageneration);
    }
    format!(
        "\"o-{}-{}-{}\"",
        base64(&object.md5),
        object.generation,
        object.metageneration
    )
}

pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        result.push(ALPHABET[(a >> 2) as usize] as char);
        result.push(ALPHABET[((a & 3) << 4 | b >> 4) as usize] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[((b & 15) << 2 | c >> 6) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    result
}

pub(crate) fn decode_hash<const N: usize>(raw: &str) -> Result<[u8; N], JsonError> {
    let invalid = || JsonError::invalid_parameter("Invalid base64 checksum");
    if raw.len() != N.div_ceil(3) * 4 {
        return Err(invalid());
    }
    let digit = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut result = [0; N];
    for (index, chunk) in raw.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let a = digit(chunk[0]).ok_or_else(invalid)?;
        let b = digit(chunk[1]).ok_or_else(invalid)?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            digit(chunk[2]).ok_or_else(invalid)?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            digit(chunk[3]).ok_or_else(invalid)?
        };
        for (offset, byte) in [a << 2 | b >> 4, b << 4 | c >> 2, c << 6 | d]
            .into_iter()
            .enumerate()
        {
            if let Some(output) = result.get_mut(index * 3 + offset) {
                *output = byte;
            }
        }
    }
    // Canonical re-encoding checks padding position and unused trailing bits as well.
    if base64(&result) != raw {
        return Err(invalid());
    }
    Ok(result)
}
