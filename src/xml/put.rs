use super::{Error, HeaderKind, MinValue, ReadRequest, XmlCode, number};
use crate::server::RequestBody as Incoming;
use crate::server::upload_drive::{
    CatalogWait, DriveError, UploadEnd, drive_upload, upload_channel,
};
use crate::{
    auth::{self, Caller},
    bucket::{InvalidStorageClass, StorageClass},
    json,
    metadata_error::MetadataError,
    object::{
        ExpectedHashes, HashAlgorithm, MapPatch, ObjectConditions, ObjectError,
        ObjectMetadataUpdate, Patch,
    },
    server::{ObjectCtx, ResponseBody, anonymous_error, boxed, empty_response, stream_upload},
    storage::ObjectUpload,
};
use hyper::{HeaderMap, Request, Response, StatusCode, body::Bytes};
use std::collections::BTreeMap;

pub(super) mod length_range;
mod streaming;
#[cfg(test)]
mod tests;

use length_range::LengthRange;

pub(super) struct Headers {
    pub(super) content_type: String,
    pub(super) expected: ExpectedHashes,
    pub(super) conditions: ObjectConditions,
    pub(super) metadata: ObjectMetadataUpdate,
    pub(super) sha256: Option<[u8; 32]>,
    pub(super) decoded_length: Option<u64>,
    /// Already checked against a declared body size; a body of undeclared size is checked as it
    /// streams. Only object PUT enforces it: multipart rejects a present value, and copy removes
    /// the header first because Cloud Storage ignores it there.
    pub(super) length_range: Option<LengthRange>,
}

pub(super) fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, Error> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(Error::new(
            XmlCode::ExcessHeaderValues,
            "Expected one header value.",
        ));
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| Error::invalid("Header must be representable as ASCII."))
}

/// How an XML write authenticated, which decides whether its signing headers are accepted.
/// Signature-specific headers are validated before the operation adapter sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Signing {
    /// OAuth or anonymous: signing headers have no purpose and are refused.
    Unsigned,
    /// A signed URL or a signature `Authorization` value.
    Signed,
    /// An aws-chunked upload: signed, with chunk signatures instead of one payload hash.
    Streaming,
}

impl Signing {
    /// `Signed` or `Unsigned` from the request's form; `respond` selects `Streaming`. Extension
    /// header normalization after authentication leaves `Authorization` and the query as sent,
    /// so this matches the form authentication saw.
    pub(super) fn of<B>(request: &Request<B>) -> Self {
        match auth::signature::SignatureKind::of(request.uri().query(), request.headers()) {
            Some(_) => Self::Signed,
            None => Self::Unsigned,
        }
    }

    pub(super) fn accepts_signing_headers(self) -> bool {
        match self {
            Self::Unsigned => false,
            Self::Signed | Self::Streaming => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Framing {
    Required,
    OptionalEmpty,
}

pub(super) fn headers(
    headers: &HeaderMap,
    max_bytes: u64,
    signing: Signing,
    framing: Framing,
) -> Result<Headers, Error> {
    reject_unsupported_headers(headers, signing)?;
    let conditions = conditions(headers)?;
    let declared_size = validate_framing(headers, max_bytes, framing)?;
    let length_range = LengthRange::from_headers(headers)?;
    // A declared size outside the range fails before any byte is read or staged.
    if let Some((range, size)) = length_range.zip(declared_size) {
        range.check(size)?;
    }
    let (content_type, expected) = content_type_and_hashes(headers)?;
    let metadata = metadata(headers)?;
    let sha256 = if signing == Signing::Streaming {
        Ok(None)
    } else {
        auth::signature::PayloadHash::of(headers).map(auth::signature::PayloadHash::digest)
    }
    .map_err(|_| Error::new(XmlCode::InvalidDigest, "Invalid SHA256 declaration."))?;
    Ok(Headers {
        content_type,
        expected,
        conditions,
        metadata,
        sha256,
        decoded_length: single(headers, "x-amz-decoded-content-length")?
            .map(|raw| {
                raw.parse::<u64>()
                    .map_err(|_| Error::unsupported("invalid decoded length"))
            })
            .transpose()?,
        length_range,
    })
}

/// The `x-goog-*` headers besides the common object headers that an object PUT reads.
const PUT_GOOG_HEADERS: &[&str] = &[
    "x-goog-hash",
    "x-goog-storage-class",
    super::CUSTOM_TIME,
    super::canned_acl::HEADER,
    length_range::HEADER,
];

fn reject_unsupported_headers(headers: &HeaderMap, signing: Signing) -> Result<(), Error> {
    for key in headers.keys() {
        let name = key.as_str();
        let unsupported = match HeaderKind::of(name) {
            HeaderKind::CustomMetadata | HeaderKind::CommonObject => false,
            HeaderKind::Signing | HeaderKind::DecodedLength => !signing.accepts_signing_headers(),
            HeaderKind::Goog => !PUT_GOOG_HEADERS.contains(&name),
            HeaderKind::Amz | HeaderKind::HttpPrecondition => true,
            HeaderKind::Other => matches!(name, "content-range" | "range" | "trailer"),
        };
        if unsupported {
            return Err(Error::unsupported(name));
        }
    }
    Ok(())
}

pub(super) fn conditions(headers: &HeaderMap) -> Result<ObjectConditions, Error> {
    let mut conditions = ObjectConditions::default();
    for (key, target, min) in [
        (
            "x-goog-if-generation-match",
            &mut conditions.if_generation_match,
            MinValue::Zero,
        ),
        (
            "x-goog-if-metageneration-match",
            &mut conditions.if_metageneration_match,
            MinValue::One,
        ),
    ] {
        *target = single(headers, key)?
            .map(|raw| number(raw, min))
            .transpose()?;
    }
    if let Some(token) = single(headers, "x-goog-gcs-idempotency-token")? {
        if token.trim().is_empty() {
            return Err(Error::invalid("Expected a non-empty retry token."));
        }
        if conditions.if_generation_match.is_none() {
            return Err(Error::unsupported(
                "x-goog-gcs-idempotency-token without x-goog-if-generation-match; add x-goog-if-generation-match: 0",
            ));
        }
    }
    Ok(conditions)
}

/// Returns the declared body size, if any: the decoded length of an aws-chunked or decoded-length
/// body, otherwise the Content-Length. The body readers fail when the body differs from it.
fn validate_framing(
    headers: &HeaderMap,
    max_bytes: u64,
    framing: Framing,
) -> Result<Option<u64>, Error> {
    let length = single(headers, "content-length")?
        .map(|raw| {
            crate::decimal::Grammar::Digits
                .parse::<u64>(raw)
                .ok_or_else(|| Error::invalid("Invalid Content-Length."))
        })
        .transpose()?;
    let chunked = single(headers, "transfer-encoding")?;
    if chunked.is_some_and(|value| !value.eq_ignore_ascii_case("chunked")) {
        return Err(Error::unsupported(
            "transfer-encoding; use Transfer-Encoding: chunked",
        ));
    }
    if chunked.is_some() && length.is_some() {
        return Err(Error::invalid("Ambiguous body framing."));
    }
    if framing == Framing::Required && length.is_none() && chunked.is_none() {
        return Err(Error::new(
            XmlCode::MissingContentLength,
            "Content-Length or chunked transfer encoding is required.",
        ));
    }
    let declared = single(headers, "x-amz-decoded-content-length")?
        .map(|raw| {
            raw.parse::<u64>()
                .map_err(|_| Error::unsupported("invalid decoded length"))
        })
        .transpose()?;
    let size = declared.or(length);
    if size.is_some_and(|size| size > max_bytes) {
        return Err(storage_error(ObjectError::TooLarge, None));
    }
    Ok(size)
}

pub(super) fn content_type_and_hashes(
    headers: &HeaderMap,
) -> Result<(String, ExpectedHashes), Error> {
    let mut selected = HeaderMap::new();
    if let Some(content_type) = headers.get("content-type") {
        single(headers, "content-type")?;
        selected.insert("content-type", content_type.clone());
    }
    let content_type = json::upload_content_type(&selected)?;
    // The JSON parser stores no type for an upload without one; XML objects default it.
    let content_type = if content_type.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        content_type
    };
    selected.clear();
    for value in headers.get_all("x-goog-hash") {
        selected.append("x-goog-hash", value.clone());
    }
    // Checksum failures omit the algorithm; CRC32C codes await provider parity (SPEC-XML-PUT-EDGES).
    let mut expected = json::declared_hashes(&selected).map_err(|error| {
        if error.kind == json::ErrorKind::Unsupported {
            return Error::from(error);
        }
        Error::new(XmlCode::InvalidDigest, "Invalid x-goog-hash digest.")
    })?;
    if let Some(md5) = single(headers, "content-md5")? {
        let content_md5 = json::decode_hash::<16>(md5)
            .map_err(|_| Error::new(XmlCode::InvalidDigest, "Invalid Content-MD5."))?;
        if expected.md5.is_some_and(|value| value != content_md5) {
            return Err(Error::new(
                XmlCode::InvalidDigest,
                "Conflicting MD5 declarations.",
            ));
        }
        expected.md5 = Some(content_md5);
    }
    Ok((content_type, expected))
}

pub(super) fn metadata(headers: &HeaderMap) -> Result<ObjectMetadataUpdate, Error> {
    let mut metadata = ObjectMetadataUpdate {
        customer_key_sha256: json::request_key(headers)?.map(crate::csek::CustomerKey::sha256),
        predefined_acl: super::canned_acl::parse(headers)?,
        ..Default::default()
    };
    for (name, field) in [
        ("cache-control", &mut metadata.cache_control),
        ("content-disposition", &mut metadata.content_disposition),
        ("content-encoding", &mut metadata.content_encoding),
        ("content-language", &mut metadata.content_language),
    ] {
        *field = single(headers, name)?.map_or(Patch::Keep, |value| Patch::Set(value.to_owned()));
    }
    if let Some(class) = single(headers, "x-goog-storage-class")? {
        let class = StorageClass::from_name(class).map_err(|error| match error {
            InvalidStorageClass::RequiresLocation => Error::unsupported(
                &MetadataError::UnsupportedObjectStorageClass(class.to_owned()).to_string(),
            ),
            InvalidStorageClass::Unknown => Error::invalid(
                "Invalid x-goog-storage-class; use STANDARD, NEARLINE, COLDLINE or ARCHIVE.",
            ),
        })?;
        metadata.storage_class = Some(class);
    }
    if let Some(value) = single(headers, super::CUSTOM_TIME)? {
        metadata.custom_time = Patch::Set(super::custom_time(value)?);
    }
    let mut custom = BTreeMap::new();
    for name in headers.keys() {
        if let Some(key) = name.as_str().strip_prefix("x-goog-meta-") {
            if key.is_empty() {
                return Err(Error::invalid("Custom metadata requires a key."));
            }
            // `name` comes from this map, so the header is present.
            if let Some(value) = single(headers, name.as_str())? {
                custom.insert(key.to_owned(), Some(value.to_owned()));
            }
        }
    }
    if crate::object::custom_metadata_exceeds(&custom, crate::object::RemovedKeys::Ignored) {
        return Err(Error::new(
            XmlCode::MetadataTooLarge,
            "Custom metadata exceeds 8 KiB.",
        ));
    }
    metadata.metadata = MapPatch::Merge(custom);
    Ok(metadata)
}

/// A PUT whose body did not match a declared hash names the hash that failed. The storage layer
/// reports which one and both digests, so a CRC32C failure carries Cloud Storage's `Details`
/// with the declared and computed values (recorded in `xml-put-edges/put-hash-mismatch`).
/// Without that report, MD5 is assumed when it was declared, which no recording distinguishes.
fn upload_error(
    error: ObjectError,
    caller: Option<Caller<'_>>,
    operation: &ReadRequest,
    expected: &ExpectedHashes,
) -> Error {
    let object = format!("{}/{}", operation.target.bucket, operation.target.name);
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every other object error keeps the shared XML object mapping"
    )]
    let crc_failed = match &error {
        ObjectError::HashMismatch(mismatch) => mismatch.algorithm == HashAlgorithm::Crc32c,
        ObjectError::ChecksumMismatch => expected.md5.is_none() && expected.crc32c.is_some(),
        _ => return storage_error(error, caller),
    };
    if crc_failed {
        let mut failure = Error::new(
            XmlCode::CrcMismatch,
            "The CRC32C you specified did not match what we computed.",
        );
        if let ObjectError::HashMismatch(mismatch) = &error {
            failure.details = Some(format!(
                "{object}, CRC mismatch (expected: {}, actual: {})",
                crc_detail(&mismatch.provided),
                crc_detail(&mismatch.calculated)
            ));
        }
        return failure;
    }
    Error::new(
        XmlCode::BadDigest,
        "The MD5 you specified in Content-MD5 or x-goog-hash did not match what we computed.",
    )
    .with_details(object)
}

/// A CRC32C as Cloud Storage prints it in a mismatch: unpadded hex, then the base64 form.
fn crc_detail(digest: &[u8]) -> String {
    let value = digest
        .iter()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte));
    format!("0x{value:x} ({})", json::base64(digest))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other object error keeps the shared JSON error mapping"
)]
pub(super) fn storage_error(error: ObjectError, caller: Option<Caller<'_>>) -> Error {
    match error {
        ObjectError::ChecksumMismatch | ObjectError::HashMismatch(_) => Error::new(
            XmlCode::BadDigest,
            "The supplied checksum does not match the uploaded object.",
        ),
        ObjectError::TooLarge => Error::new(
            XmlCode::EntityTooLarge,
            "The upload exceeds the configured object size limit.",
        ),
        ObjectError::BodyAborted(_) => {
            Error::new(XmlCode::IncompleteBody, "The upload body did not complete.")
        }
        error => anonymous_error(error.into(), caller).into(),
    }
}

pub(crate) async fn respond(
    request: Request<Incoming>,
    operation: ReadRequest,
    ctx: ObjectCtx<'_>,
    streaming: Option<auth::signature::Streaming>,
) -> Result<Response<ResponseBody>, Error> {
    if request.headers().contains_key("x-goog-copy-source") {
        return super::copy::respond(request, operation, ctx).await;
    }
    let ObjectCtx {
        state,
        config,
        caller,
        ..
    } = ctx;
    let access = ctx
        .authorize(auth::OBJECTS_CREATE, &operation.target.bucket)
        .await
        .map_err(|error| anonymous_error(error, caller))?;
    let signing = if streaming.is_some() {
        Signing::Streaming
    } else {
        Signing::of(&request)
    };
    let Headers {
        content_type,
        expected,
        conditions,
        mut metadata,
        sha256,
        decoded_length,
        length_range,
    } = headers(
        request.headers(),
        config.max_object_bytes,
        signing,
        Framing::Required,
    )?;
    super::canned_acl::check(
        metadata.predefined_acl,
        &operation.target.bucket,
        &access,
        ctx,
    )
    .await?;
    if streaming.is_some() {
        metadata.content_encoding = streaming::stored_encoding(request.headers())?;
    }
    let permit = ctx
        .await_catalog(state.catalog.prepare_object_upload(
            &operation.target.bucket,
            &operation.target.name,
            conditions,
            &access,
        ))
        .await
        .map_err(|error| storage_error(error, caller))?;
    let (sender, body) = upload_channel();
    let upload = state.catalog.put_object_prechecked(
        ObjectUpload {
            bucket: &operation.target.bucket,
            name: &operation.target.name,
            content_type,
            expected,
            conditions,
            max_bytes: config.max_object_bytes,
            metadata,
            access,
        },
        body,
        permit,
    );
    let producer = async {
        streaming::upload(
            request.into_body(),
            &sender,
            ctx,
            streaming,
            streaming::Declared {
                decoded_length,
                sha256,
                length_range,
            },
        )
        .await
        .map(|()| UploadEnd::Eof)
    };
    let record = drive_upload(CatalogWait::Upload(ctx), upload, &sender, producer)
        .await
        .map_err(|error| match error {
            DriveError::Upload(error) => upload_error(error, caller, &operation, &expected),
            DriveError::Producer(error) => error,
        })?;
    let mut headers = super::media_headers(&record, super::CacheScope::Private)?;
    let mut response = boxed(empty_response(StatusCode::OK));
    json::insert_response_headers(&record, response.headers_mut());
    super::move_headers(
        &mut headers,
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
