use super::{
    JsonError, Projection, WriteMode,
    buckets::{encode_segment, timestamp, validate_bucket_name_for_profile},
    query::{Decoding, boolean, decode, finish, parse_query, projection},
};
use crate::{
    config::CompatProfile,
    object::{ExpectedHashes, ObjectConditions, ObjectRecord, retention::UnlockedOverride},
};
use hyper::{HeaderMap, Method, StatusCode, Uri, header::HeaderValue};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObjectInsertParams {
    pub conditions: ObjectConditions,
    pub projection: Projection,
    pub predefined_acl: Option<crate::acl::PredefinedObjectAcl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObjectGetParams {
    pub generation: Option<i64>,
    pub conditions: ObjectConditions,
    pub projection: Projection,
    pub media: bool,
    pub soft_deleted: bool,
}

/// A media or multipart upload: the object it names and the options its query declares. A
/// multipart upload can leave the name to its metadata part, so `N` is `Option<String>` there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UploadRequest<N> {
    pub bucket: String,
    pub name: N,
    pub params: ObjectInsertParams,
    pub content_encoding: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObjectRequest {
    Restore {
        bucket: String,
        name: String,
        generation: i64,
        conditions: ObjectConditions,
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
        mode: WriteMode,
        retention_override: UnlockedOverride,
    },
    Insert(UploadRequest<String>),
    Multipart(UploadRequest<Option<String>>),
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
pub(crate) fn route_object(method: &Method, uri: &Uri) -> Option<Result<ObjectRequest, JsonError>> {
    route_object_with_profile(method, uri, CompatProfile::Native)
}

pub(crate) fn route_object_with_profile(
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
            || (method == Method::HEAD && object.is_some())
            || (!download
                && object.is_some()
                && matches!(*method, Method::DELETE | Method::PATCH | Method::PUT))
    };
    if !allowed {
        // Cloud Storage answered `POST` to an object resource with its bare not-found page.
        if !upload && !download && object.is_some() && *method == Method::POST {
            let mut error = JsonError::new(StatusCode::NOT_FOUND, "notFound", "Not Found");
            error.plain = Some(&super::error::NOT_FOUND_HTML);
            return Some(Err(error));
        }
        let mut error = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "HTTP method is not supported for this object endpoint",
        );
        error.allow = Some(if upload {
            "POST"
        } else if object.is_none() {
            "GET"
        } else if download {
            "GET, HEAD"
        } else {
            "GET, HEAD, DELETE, PATCH, PUT"
        });
        return Some(Err(error));
    }
    Some((|| {
        if bucket.len() > 222 * 3 {
            return Err(JsonError::invalid_parameter("Invalid bucket name"));
        }
        let bucket = decode(bucket, Decoding::Path)?;
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
                let name = decode(segment, Decoding::Path)?;
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
pub(crate) fn validate_object_name(name: &str) -> Result<(), JsonError> {
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

/// Parse an object request's query. `name` is `None` exactly for uploads: the router accepts
/// only POST on the upload endpoint, which names the object in the query or metadata.
fn parse_object_query(
    method: &Method,
    bucket: String,
    name: Option<String>,
    raw: &str,
) -> Result<ObjectRequest, JsonError> {
    let mut query = parse_query(raw)?;
    // Uploads apply predefinedAcl; other methods keep the server's explicit rejection.
    let predefined_acl = if name.is_none() {
        query.remove("predefinedAcl")
    } else {
        None
    };
    let content_encoding = take_common_parameters(method, &mut query)?;
    let media = match query.remove("alt").as_deref() {
        None | Some("json") => false,
        Some("media") if matches!(*method, Method::GET | Method::HEAD) => true,
        Some("media") => {
            return Err(JsonError::invalid_parameter(
                "alt=media requires GET or HEAD",
            ));
        }
        Some(other) => {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "invalidAltValue",
                format!("Unknown output format: {other}"),
            )
            .at_parameter("alt"));
        }
    };
    let upload = name.is_none();
    let conditions = (|| {
        Ok::<_, JsonError>(ObjectConditions {
            if_generation_match: integer(&mut query, "ifGenerationMatch")?,
            if_generation_not_match: integer(&mut query, "ifGenerationNotMatch")?,
            if_metageneration_match: integer(&mut query, "ifMetagenerationMatch")?,
            if_metageneration_not_match: integer(&mut query, "ifMetagenerationNotMatch")?,
        })
    })()
    // The upload front end labelled its query problems `text/html`.
    .map_err(|error| if upload { error.html_typed() } else { error })?;
    match name {
        None => {
            let upload = UploadQuery {
                conditions,
                content_encoding,
                predefined_acl,
            };
            parse_upload_query(bucket, query, upload)
        }
        Some(name) => {
            let target = QueryTarget {
                bucket,
                name,
                conditions,
                media,
            };
            parse_named_object_query(method, target, query)
        }
    }
}

/// Removes the options every object method shares and rejects the unsupported ones. Returns
/// the `contentEncoding` of an upload, the only POST this parser receives.
fn take_common_parameters(
    method: &Method,
    query: &mut BTreeMap<String, String>,
) -> Result<Option<String>, JsonError> {
    if method == Method::DELETE {
        query.remove("fields");
    } else if query.contains_key("fields") && query.get("alt").is_some_and(|v| v == "media") {
        return Err(JsonError::unsupported_object("fields with alt=media"));
    } else {
        super::Fields::take(query, "Object")?;
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
    let content_encoding = if method == Method::POST {
        query.remove("contentEncoding")
    } else {
        None
    };
    let method_keys: &[&str] = if method == Method::POST {
        query.remove("predefinedAcl");
        &["kmsKeyName"]
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
    // The routing layer applies the value; prettyPrint=false selects compact bodies.
    boolean(query, "prettyPrint")?;
    Ok(content_encoding)
}

/// Upload options taken from the query before method-specific parsing.
struct UploadQuery {
    conditions: ObjectConditions,
    content_encoding: Option<String>,
    predefined_acl: Option<String>,
}

/// The upload front end's answer to an upload type it does not serve: a bare text page.
pub(crate) fn unsupported_upload_type() -> JsonError {
    let mut error =
        JsonError::invalid_parameter("uploadType must be media, multipart or resumable");
    error.plain = Some(&super::error::BAD_REQUEST_TEXT);
    error
}

fn parse_upload_query(
    bucket: String,
    mut query: BTreeMap<String, String>,
    upload: UploadQuery,
) -> Result<ObjectRequest, JsonError> {
    let UploadQuery {
        conditions,
        content_encoding,
        predefined_acl,
    } = upload;
    // Validated here so an invalid value fails before any body is read.
    if let Some(encoding) = &content_encoding {
        super::validate_fixed_metadata("contentEncoding", encoding)?;
    }
    let predefined_acl = predefined_acl
        .map(|value| {
            crate::acl::PredefinedObjectAcl::parse(&value).ok_or_else(|| {
                JsonError::invalid_parameter(format!(
                    "Invalid predefinedAcl {value:?}; use authenticatedRead, bucketOwnerFullControl, bucketOwnerRead, private, projectPrivate or publicRead (for example, predefinedAcl=publicRead)"
                ))
            })
        })
        .transpose()?;
    let missing_name = JsonError::missing_object_name;
    let (multipart, resumable) = match query.remove("uploadType").as_deref() {
        Some("media") => (false, false),
        Some("multipart") => (true, false),
        Some("resumable") => (true, true),
        // An upload that names no type was stored as a media upload (recorded 200).
        None => (false, false),
        Some(_) => return Err(unsupported_upload_type()),
    };
    let name = query.remove("name");
    if let Some(name) = &name {
        validate_object_name(name)?;
    }
    if !multipart && name.is_none() {
        return Err(missing_name());
    }
    let projection = projection(&mut query)?;
    finish(&query)?;
    let params = ObjectInsertParams {
        conditions,
        projection,
        predefined_acl,
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
        return Ok(ObjectRequest::Multipart(UploadRequest {
            bucket,
            name,
            params,
            content_encoding,
        }));
    }
    let name = name.ok_or_else(missing_name)?;
    Ok(ObjectRequest::Insert(UploadRequest {
        bucket,
        name,
        params,
        content_encoding,
    }))
}

/// The object a non-upload request names, with the options already parsed from its query.
struct QueryTarget {
    bucket: String,
    name: String,
    conditions: ObjectConditions,
    media: bool,
}

fn parse_named_object_query(
    method: &Method,
    target: QueryTarget,
    mut query: BTreeMap<String, String>,
) -> Result<ObjectRequest, JsonError> {
    let QueryTarget {
        bucket,
        name,
        conditions,
        media,
    } = target;
    let generation = integer(&mut query, "generation")?;
    if method == Method::PATCH || method == Method::PUT {
        // A PUT without `projection` answered without `acl` and `owner` (recorded), although
        // the reference documents `full` as its default.
        let projection = projection(&mut query)?;
        let retention_override = if boolean(&mut query, "overrideUnlockedRetention")? {
            UnlockedOverride::Requested
        } else {
            UnlockedOverride::NotRequested
        };
        finish(&query)?;
        return Ok(ObjectRequest::Update {
            bucket,
            name,
            retention_override,
            mode: if method == Method::PUT {
                WriteMode::Replace
            } else {
                WriteMode::Patch
            },
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
        finish(&query)?;
        return Ok(ObjectRequest::Get {
            bucket,
            name,
            params: ObjectGetParams {
                generation,
                conditions,
                projection,
                media,
                soft_deleted,
            },
        });
    }
    // Cloud Storage accepted the parameter on objects.delete and ignored it: a retained object
    // stayed protected (observed 2026-10-31). Only PATCH and PUT honour the override.
    boolean(&mut query, "overrideUnlockedRetention")?;
    finish(&query)?;
    Ok(ObjectRequest::Delete {
        bucket,
        name,
        generation,
        conditions,
    })
}

pub(super) fn integer(
    query: &mut BTreeMap<String, String>,
    key: &str,
) -> Result<Option<i64>, JsonError> {
    query
        .remove(key)
        .map(|value| {
            if value.starts_with('+') {
                return Err(JsonError::invalid_long(&value).at_parameter(key));
            }
            value
                .parse()
                .map_err(|_| JsonError::invalid_long(&value).at_parameter(key))
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
pub(crate) fn parse_object_upload_headers(
    headers: &HeaderMap,
    _conditions: ObjectConditions,
) -> Result<(String, ExpectedHashes), JsonError> {
    unsupported_headers(headers, &[crate::csek::DESTINATION_PREFIX])?;
    validate_read_condition_headers(headers, EtagConditions::Unsupported)?;
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
    // `Content-MD5` is not examined: Cloud Storage stored a JSON upload whose header named the
    // wrong digest (recorded 200 with the computed hashes).
    for name in [
        "content-encoding",
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
        // An upload without a type stores none (recorded: the resource omits `contentType`).
        .unwrap_or_default();
    let mime = content_type.split(';').next().unwrap_or_default().trim();
    let token = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    };
    if headers.contains_key("content-type")
        && !mime
            .split_once('/')
            .is_some_and(|(kind, subtype)| token(kind) && token(subtype))
    {
        return Err(JsonError::invalid_parameter("Invalid Content-Type header"));
    }
    // `X-Goog-Hash` is not examined either. Recorded JSON media uploads that named a wrong,
    // malformed or unknown-algorithm hash all stored the computed hashes with 200, so only a
    // multipart metadata `crc32c`/`md5Hash` (below) is verified on this route.
    Ok((content_type.to_owned(), ExpectedHashes::default()))
}

/// The hashes a resumable initiation declares with `X-Goog-Hash`. Recorded JSON media uploads
/// ignored the header, but the resumable route was not recorded, so it keeps verifying.
pub(crate) fn declared_hashes(headers: &HeaderMap) -> Result<ExpectedHashes, JsonError> {
    let mut hashes = ExpectedHashes::default();
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
    Ok(hashes)
}

/// Reject unsupported encryption headers explicitly. `If-Range` is not one: Cloud Storage
/// honored the `Range` of a media read without evaluating it.
// shortcut: GET/DELETE tokens do not deduplicate retries; add replay storage for strict retry fidelity.
pub(crate) fn validate_object_get_headers(headers: &HeaderMap) -> Result<(), JsonError> {
    validate_keyed_object_headers(headers, &[])
}

/// As [`validate_object_get_headers`], leaving the listed key header prefixes to the caller.
pub(crate) fn validate_keyed_object_headers(
    headers: &HeaderMap,
    keys: &[&str],
) -> Result<(), JsonError> {
    unsupported_headers(headers, keys)?;
    Ok(())
}

/// Whether a route evaluates `If-Match` and `If-None-Match` against an ETag it serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EtagConditions {
    Supported,
    /// A JSON media read: the entity tags are evaluated, while `If-Modified-Since`,
    /// `If-Unmodified-Since` and `If-Range` are ignored as Cloud Storage ignored them.
    MediaRead,
    MatchOnly,
    /// Accepted and not evaluated, matching an observed Cloud Storage write that ignores both.
    Ignored,
    Unsupported,
}

/// Keep unsupported conditional headers explicit on routes without validators.
pub(crate) fn validate_read_condition_headers(
    headers: &HeaderMap,
    etags: EtagConditions,
) -> Result<(), JsonError> {
    if matches!(etags, EtagConditions::Supported | EtagConditions::MatchOnly) {
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
        if headers.contains_key(name)
            && (etags == EtagConditions::Unsupported
                || (etags == EtagConditions::MatchOnly && name == "if-none-match"))
        {
            return Err(JsonError::unsupported_object(name));
        }
    }
    Ok(())
}

/// Resolve one byte range against the selected representation; ignored ranges return None.
pub(crate) fn object_media_range(
    headers: &HeaderMap,
    size: u64,
) -> Result<Option<(u64, u64)>, JsonError> {
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

/// Copy and rewrite responses name the calling creator as `owner` even without the full
/// projection, while still omitting `acl` (recorded 2026-11-01). A project-owners owner is left
/// out: the recordings only show a user owner, so that case is an inference that keeps the
/// legacy emulator profile unchanged.
pub(crate) fn add_derived_owner(value: &mut Value, object: &ObjectRecord) {
    if let Some(crate::object::owner::ObjectOwner::Principal {
        entity: Some(entity),
        ..
    }) = &object.owner
        && value.get("owner").is_none()
    {
        value["owner"] = json!({"entity": entity});
    }
}

/// Render the selected immutable generation using the configured advertised endpoint.
pub(crate) fn object_resource(
    object: &ObjectRecord,
    base_url: &str,
    projection: Projection,
) -> Result<Value, JsonError> {
    // ACL and owner projection requires current catalog authorization in the server adapter.
    let (Projection::NoAcl | Projection::Full) = projection;
    let base = base_url.trim_end_matches('/');
    let path = format!(
        "storage/v1/b/{}/o/{}",
        encode_segment(&object.bucket),
        encode_segment(&object.name)
    );
    // Fields follow the order of recorded Cloud Storage responses. Fields those responses did
    // not contain follow them, in an order that has not been verified.
    let mut value = json!({
        "kind": "storage#object",
        "id": format!("{}/{}/{}", object.bucket, object.name, object.generation),
        "selfLink": format!("{base}/{path}"),
        "mediaLink": format!("{base}/download/{path}?generation={}&alt=media", object.generation),
        "name": object.name, "bucket": object.bucket,
        "generation": object.generation.to_string(), "metageneration": object.metageneration.to_string(),
    });
    if !object.content_type.is_empty() {
        value["contentType"] = json!(object.content_type);
    }
    value["storageClass"] = json!(object.storage_class);
    value["size"] = json!(object.size.to_string());
    if object.has_md5() {
        value["md5Hash"] = json!(base64(&object.md5));
    }
    if let Some(encoding) = &object.content_encoding {
        value["contentEncoding"] = json!(encoding);
    }
    value["crc32c"] = json!(base64(&object.crc32c.to_be_bytes()));
    value["etag"] = json!(object_etag(object));
    value["timeCreated"] = json!(timestamp(object.time_created)?);
    value["updated"] = json!(timestamp(object.updated)?);
    value["timeStorageClassUpdated"] = json!(timestamp(object.time_created)?);
    value["timeFinalized"] = json!(timestamp(object.time_created)?);
    if let Some(count) = object.component_count {
        value["componentCount"] = json!(count);
    }
    for (key, field) in [
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
    if let Some(hold) = object.event_based_hold {
        value["eventBasedHold"] = json!(hold);
    }
    if let Some(hold) = object.temporary_hold {
        value["temporaryHold"] = json!(hold);
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
pub(crate) fn object_media_headers(object: &ObjectRecord) -> Result<HeaderMap, JsonError> {
    let mut headers = HeaderMap::new();
    for (key, value) in [
        // The transport substitutes stored/default cache metadata only for an allUsers read grant.
        ("cache-control", super::NO_STORE_CACHE_CONTROL.to_owned()),
        (
            "content-type",
            if object.content_type.is_empty() {
                "application/octet-stream".into()
            } else {
                object.content_type.clone()
            },
        ),
        ("content-length", object.size.to_string()),
        ("x-goog-generation", object.generation.to_string()),
        ("x-goog-metageneration", object.metageneration.to_string()),
        (
            "x-goog-hash",
            if !object.has_md5() {
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

/// Whether the served representation of the object depends on `Accept-Encoding`: stored gzip
/// without `no-transform` is either sent compressed or gunzipped.
pub(crate) fn object_media_negotiable(object: &ObjectRecord) -> bool {
    object
        .content_encoding
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("gzip"))
        && !object.cache_control.as_deref().is_some_and(|value| {
            value
                .split(',')
                .any(|directive| directive.trim().eq_ignore_ascii_case("no-transform"))
        })
}

/// Select gzip expansion while honoring explicit quality overrides and no-transform.
pub(crate) fn object_media_transcoding(
    headers: &HeaderMap,
    object: &ObjectRecord,
) -> Result<bool, JsonError> {
    if !object_media_negotiable(object) {
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
            // `split` always yields at least one item, so the default is never used.
            let encoding = parts.next().unwrap_or_default().trim();
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

/// The JSON API validator of one object generation. It carries no content hash, so a keyless
/// metadata read of an encrypted object learns nothing about its plaintext.
pub(crate) fn object_etag(object: &ObjectRecord) -> String {
    revision_etag(Some(object.generation), object.metageneration)
}

/// The unquoted base64 protobuf message Cloud Storage uses as a JSON API ETag: the generation
/// as field 1 (objects only) and the metageneration as field 2 (field 1 for buckets). Recorded
/// responses carry the same value in the `ETag` header and the resource `etag` field. ACL
/// entries, buckets, objects and gRPC messages all share this encoding.
pub(crate) fn revision_etag(generation: Option<i64>, metageneration: i64) -> String {
    fn varint(bytes: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            bytes.push((value as u8) | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
    }
    let mut bytes = Vec::new();
    match generation {
        Some(generation) => {
            bytes.push(0x08);
            varint(&mut bytes, generation as u64);
            bytes.push(0x10);
        }
        None => bytes.push(0x08),
    }
    varint(&mut bytes, metageneration as u64);
    base64(&bytes)
}

pub(crate) use crate::base64::encode as base64;

pub(crate) fn decode_hash<const N: usize>(raw: &str) -> Result<[u8; N], JsonError> {
    let invalid = || JsonError::invalid_parameter("Invalid base64 checksum");
    if raw.len() != N.div_ceil(3) * 4 {
        return Err(invalid());
    }
    crate::base64::decode(
        raw,
        crate::base64::Alphabet::Standard,
        crate::base64::Padding::Required,
    )
    .and_then(|bytes| bytes.try_into().ok())
    .ok_or_else(invalid)
}
