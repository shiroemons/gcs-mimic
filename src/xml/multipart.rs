//! XML multipart protocol adapter; storage owns session transitions and durable bytes.

use super::{Error, ReadRequest, XmlCode, accepted_parameter, put};
use crate::server::upload_drive::{
    CatalogWait, DriveError, UploadEnd, drive_upload, upload_channel,
};
use crate::{
    object::ObjectError,
    server::{
        ObjectCtx, RequestBody, ResponseBody, anonymous_error, boxed, empty_response, stream_upload,
    },
    storage::xml_multipart as storage,
};
use hyper::{
    Method, Request, Response, StatusCode, Uri,
    header::{self, HeaderValue},
};
use std::{collections::BTreeMap, str::FromStr};

mod manifest;
mod render;
#[cfg(test)]
mod tests;

const MANIFEST_LIMIT: usize = 2 * 1024 * 1024;

pub(crate) fn matches(uri: &Uri) -> bool {
    uri.query().is_some_and(|query| {
        query.split('&').any(|field| {
            crate::percent::decode(
                field.split('=').next().unwrap_or_default(),
                crate::percent::Decoding::Form,
            )
            .is_ok_and(|name| matches!(name.as_str(), "uploads" | "uploadId" | "partNumber"))
        })
    })
}

fn xml_text(value: &str) -> Result<(), Error> {
    if value.chars().all(|ch| {
        matches!(ch, '\t' | '\n' | '\r')
            || (ch >= '\u{20}' && !matches!(ch, '\u{fffe}' | '\u{ffff}'))
    }) {
        Ok(())
    } else {
        Err(Error::invalid(
            "This multipart value cannot be represented in XML 1.0",
        ))
    }
}

/// The largest part number, which also bounds a part number marker.
const MAX_PART_NUMBER: u16 = 10_000;

fn out_of_range() -> Error {
    Error::invalid("Multipart number is out of range")
}

/// A multipart part number, 1 through 10000.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PartNumber(u16);

impl FromStr for PartNumber {
    type Err = Error;

    /// Decimal digits only; `u16` holds every allowed value, so one range check suffices.
    fn from_str(raw: &str) -> Result<Self, Error> {
        crate::decimal::Grammar::Digits
            .parse::<u16>(raw)
            .filter(|number| (1..=MAX_PART_NUMBER).contains(number))
            .map(Self)
            .ok_or_else(out_of_range)
    }
}

impl PartNumber {
    fn get(self) -> u16 {
        self.0
    }
}

/// A listing request.
enum Listing {
    /// `GET /BUCKET?uploads`.
    Uploads,
    /// `GET /BUCKET/OBJECT?uploadId=ID`.
    Parts { upload_id: String },
}

/// A request that writes or ends an upload.
enum Write {
    /// `POST /BUCKET/OBJECT?uploads`.
    Begin,
    /// `PUT /BUCKET/OBJECT?uploadId=ID&partNumber=N`.
    Part {
        upload_id: String,
        number: PartNumber,
    },
    /// `POST /BUCKET/OBJECT?uploadId=ID`.
    Complete { upload_id: String },
    /// `DELETE /BUCKET/OBJECT?uploadId=ID`.
    Abort { upload_id: String },
}

/// One XML multipart request, decided from its method, object name and query.
enum Operation {
    List(Listing),
    Write(Write),
}

impl Operation {
    /// Checks the `uploads` and `uploadId` combination, then the method and query, then a part
    /// number, then the upload ID, so a PUT with a bad part number and a bad upload ID names
    /// the part number.
    fn parse(
        method: &Method,
        name: &str,
        query: &mut BTreeMap<String, String>,
    ) -> Result<Self, Error> {
        let uploads = query.remove("uploads");
        let id = query.remove("uploadId");
        let number = query.remove("partNumber");
        if uploads.as_ref().is_some_and(|value| !value.is_empty())
            || (uploads.is_some() && id.is_some())
        {
            return Err(Error::invalid(
                "Use uploads or uploadId, with an empty uploads value",
            ));
        }
        Ok(
            match (method, name.is_empty(), uploads.is_some(), id, number) {
                (&Method::POST, false, true, None, None) => Self::Write(Write::Begin),
                (&Method::GET, true, true, None, None) => Self::List(Listing::Uploads),
                (&Method::GET, false, false, Some(id), None) => Self::List(Listing::Parts {
                    upload_id: upload_id(id)?,
                }),
                (&Method::POST, false, false, Some(id), None) => Self::Write(Write::Complete {
                    upload_id: upload_id(id)?,
                }),
                (&Method::DELETE, false, false, Some(id), None) => Self::Write(Write::Abort {
                    upload_id: upload_id(id)?,
                }),
                (&Method::PUT, false, false, Some(id), Some(number)) => {
                    let number = number.parse::<PartNumber>()?;
                    Self::Write(Write::Part {
                        upload_id: upload_id(id)?,
                        number,
                    })
                }
                _ => {
                    return Err(Error::invalid(
                        "Invalid multipart method or query combination",
                    ));
                }
            },
        )
    }

    fn permission(&self) -> crate::auth::Permission {
        match self {
            Self::Write(Write::Begin | Write::Part { .. } | Write::Complete { .. }) => {
                crate::auth::Permission::MultipartCreate
            }
            Self::List(Listing::Parts { .. }) => crate::auth::Permission::MultipartListParts,
            Self::List(Listing::Uploads) => crate::auth::Permission::MultipartList,
            Self::Write(Write::Abort { .. }) => crate::auth::Permission::MultipartAbort,
        }
    }
}

impl Write {
    fn framing(&self) -> put::Framing {
        match self {
            Self::Abort { .. } => put::Framing::OptionalEmpty,
            Self::Begin | Self::Part { .. } | Self::Complete { .. } => put::Framing::Required,
        }
    }

    fn max_body(&self, config: &crate::config::Config) -> u64 {
        match self {
            Self::Part { .. } => config.max_object_bytes().min(storage::MAX_PART_BYTES),
            Self::Complete { .. } => MANIFEST_LIMIT as u64,
            Self::Begin | Self::Abort { .. } => 0,
        }
    }

    fn metadata(&self) -> InitiationMetadata {
        match self {
            Self::Begin => InitiationMetadata::Accepted,
            Self::Part { .. } | Self::Complete { .. } | Self::Abort { .. } => {
                InitiationMetadata::Refused
            }
        }
    }
}

/// Whether a request may carry object metadata headers, which only initiation stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitiationMetadata {
    Accepted,
    Refused,
}

/// An upload ID of 1 through 1024 bytes.
fn upload_id(id: String) -> Result<String, Error> {
    if id.is_empty() || id.len() > 1024 {
        return Err(Error::invalid("Invalid uploadId"));
    }
    Ok(id)
}

/// `part-number-marker`, 0 through 10000. A marker is not a part number: 0 starts the list.
fn part_marker(query: &mut BTreeMap<String, String>) -> Result<u16, Error> {
    query
        .remove("part-number-marker")
        .map(|value| {
            crate::decimal::Grammar::Digits
                .parse::<u16>(&value)
                .filter(|marker| *marker <= MAX_PART_NUMBER)
                .ok_or_else(out_of_range)
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

/// `max-parts` or `max-uploads`: at most 1000 entries, 1000 when absent, never zero.
fn page_limit(query: &mut BTreeMap<String, String>, name: &str) -> Result<usize, Error> {
    let limit = query
        .remove(name)
        .map(|value| {
            crate::decimal::Grammar::Digits
                .parse::<u32>(&value)
                .ok_or_else(out_of_range)
        })
        .transpose()?
        .unwrap_or(1000)
        .min(1000) as usize;
    if limit == 0 {
        return Err(Error::invalid(format!("{name} must be positive")));
    }
    Ok(limit)
}

fn remaining(query: &BTreeMap<String, String>) -> Result<(), Error> {
    if let Some(name) = query.keys().find(|key| !accepted_parameter(key)) {
        return Err(Error::unsupported(name));
    }
    Ok(())
}

fn request_headers(
    request: &Request<RequestBody>,
    max: u64,
    framing: put::Framing,
    metadata: InitiationMetadata,
) -> Result<put::Headers, Error> {
    for name in request.headers().keys() {
        let name = name.as_str();
        if name.starts_with("x-goog-if-") || name == crate::object::IdempotencyToken::HEADER {
            return Err(Error::unsupported(
                "multipart preconditions and invocation-token replay",
            ));
        }
        if metadata == InitiationMetadata::Refused
            && (name.starts_with("x-goog-meta-")
                || matches!(
                    name,
                    "x-goog-storage-class"
                        | super::canned_acl::HEADER
                        | "cache-control"
                        | "content-disposition"
                        | "content-language"
                        | "content-encoding"
                ))
        {
            return Err(Error::unsupported("multipart metadata outside initiation"));
        }
    }
    let signing = put::Signing::of(request);
    let headers = put::headers(request.headers(), max, signing, framing)?;
    // Only object PUT enforces the range; accepting it here would drop the limit silently.
    if headers.length_range.is_some() {
        return Err(Error::unsupported(
            "x-goog-content-length-range on multipart uploads",
        ));
    }
    Ok(headers)
}

fn storage_error(error: storage::Error, ctx: ObjectCtx<'_>) -> Error {
    let code = match error {
        storage::Error::Object(error) => return put::storage_error(error, ctx.caller),
        storage::Error::NoSuchUpload => XmlCode::NoSuchUpload,
        storage::Error::InvalidPart => XmlCode::InvalidPart,
        storage::Error::InvalidPartOrder => XmlCode::InvalidPartOrder,
        storage::Error::PartTooSmall => XmlCode::InvalidArgument,
        storage::Error::Busy => XmlCode::ServiceUnavailable,
    };
    Error::new(code, error.to_string())
}

/// Cloud Storage sends every successful multipart document (initiation, both listings and
/// completion) as `text/html` (recorded 2026-11-16); errors stay `application/xml`.
const DOCUMENT_CONTENT_TYPE: &str = "text/html; charset=UTF-8";

fn document(body: String) -> Response<ResponseBody> {
    let mut response = super::document_response(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(DOCUMENT_CONTENT_TYPE),
    );
    boxed(response)
}

/// A listing document, which Cloud Storage marks private like its bucket listings.
fn listing_document(body: String) -> Response<ResponseBody> {
    let mut response = document(body);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(super::PRIVATE_CACHE_CONTROL),
    );
    response
}

pub(crate) async fn respond(
    request: Request<RequestBody>,
    path: ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let mut query = crate::auth::signature::xml_query(
        request.uri().query().unwrap_or_default(),
        request.headers(),
    )?;
    let operation = Operation::parse(request.method(), &path.target.name, &mut query)?;
    xml_text(&path.target.name)?;
    let access = ctx
        .authorize(operation.permission(), &path.target.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    match operation {
        Operation::List(listing) => render::list(request, path, listing, query, access, ctx).await,
        Operation::Write(write) => {
            remaining(&query)?;
            write_upload(request, path, write, access, ctx).await
        }
    }
}

/// The upload an object path and upload ID name.
fn upload_target(path: ReadRequest, upload_id: String) -> storage::Target {
    storage::Target {
        bucket: path.target.bucket,
        name: path.target.name,
        id: upload_id,
    }
}

async fn write_upload(
    request: Request<RequestBody>,
    path: ReadRequest,
    write: Write,
    access: crate::bucket::BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let headers = request_headers(
        &request,
        write.max_body(ctx.config),
        write.framing(),
        write.metadata(),
    )?;
    match write {
        Write::Begin => {
            if headers.expected != Default::default() {
                return Err(Error::unsupported(
                    "whole-object checksums during multipart initiation",
                ));
            }
            super::canned_acl::check(
                headers.metadata.acl.predefined(),
                &path.target.bucket,
                &access,
                ctx,
            )
            .await?;
            empty_body(request, headers.sha256).await?;
            let options = storage::Options {
                content_type: headers.content_type,
                metadata: headers.metadata,
            };
            let upload = ctx
                .await_catalog(ctx.state.catalog.begin_xml_multipart(
                    &path.target.bucket,
                    &path.target.name,
                    options,
                    access,
                ))
                .await
                .map_err(|error| storage_error(error, ctx))?;
            Ok(document(render::initiate_body(&upload.target)))
        }
        Write::Part { upload_id, number } => {
            let target = upload_target(path, upload_id);
            let lease = ctx
                .await_session_writer(ctx.state.catalog.lease_xml_part(
                    target,
                    number.get(),
                    access,
                ))
                .await
                .map_err(|error| storage_error(error, ctx))?;
            check_customer_key(&request, &lease.upload, ctx)?;
            upload_part(request, lease, headers, ctx).await
        }
        Write::Complete { upload_id } => {
            let lease = whole_upload(upload_target(path, upload_id), access, ctx).await?;
            check_customer_key(&request, &lease.upload, ctx)?;
            complete(request, lease, headers, ctx).await
        }
        Write::Abort { upload_id } => {
            let lease = ctx
                .await_session_writer(
                    ctx.state
                        .catalog
                        .lease_xml_abort(upload_target(path, upload_id), access),
                )
                .await
                .map_err(|error| storage_error(error, ctx))?;
            empty_body(request, headers.sha256).await?;
            ctx.await_catalog(ctx.state.catalog.abort_xml_multipart(lease))
                .await
                .map_err(|error| storage_error(error, ctx))?;
            // No body, so no Content-Type, as Cloud Storage answers.
            Ok(boxed(empty_response(StatusCode::NO_CONTENT)))
        }
    }
}

async fn whole_upload(
    target: storage::Target,
    access: crate::bucket::BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<storage::UploadLease, Error> {
    ctx.await_session_writer(ctx.state.catalog.lease_xml_multipart(target, access))
        .await
        .map_err(|error| storage_error(error, ctx))
}

fn check_customer_key(
    request: &Request<RequestBody>,
    upload: &storage::Upload,
    ctx: ObjectCtx<'_>,
) -> Result<(), Error> {
    let key =
        crate::csek::CustomerKey::from_headers(request.headers(), crate::csek::DESTINATION_PREFIX)?;
    crate::csek::check_data(upload.options.metadata.customer_key_sha256, key)
        .map_err(|error| put::storage_error(ObjectError::CustomerEncryption(error), ctx.caller))
}

async fn empty_body(
    request: Request<RequestBody>,
    expected: Option<[u8; 32]>,
) -> Result<(), Error> {
    super::empty_body(request.into_body()).await?;
    if super::sha256_mismatch(expected, b"") {
        return Err(signed_digest_mismatch());
    }
    Ok(())
}

/// A multipart body, empty or a completion document, that differs from its signed SHA-256.
fn signed_digest_mismatch() -> Error {
    Error::new(XmlCode::BadDigest, "The signed body digest does not match")
}

async fn upload_part(
    request: Request<RequestBody>,
    lease: storage::PartLease,
    headers: put::Headers,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let (sender, body) = upload_channel();
    let upload = ctx.state.catalog.put_xml_part(
        lease,
        storage::PartUpload {
            expected: headers.expected,
            max_bytes: ctx.config.max_object_bytes(),
        },
        body,
    );
    let producer = async {
        stream_upload(
            request.into_body(),
            &sender,
            ctx.config.max_object_bytes().min(storage::MAX_PART_BYTES),
            headers.sha256,
        )
        .await
        .map(|_| UploadEnd::Eof)
    };
    let part = drive_upload(CatalogWait::Upload(ctx), upload, &sender, producer)
        .await
        .map_err(|error| match error {
            DriveError::Upload(error) => storage_error(error, ctx),
            DriveError::Producer(error) => put::storage_error(error, ctx.caller),
        })?;
    // The envelope gives this empty PUT answer the text/html type Cloud Storage sends.
    let mut response = boxed(empty_response(StatusCode::OK));
    response.headers_mut().insert(
        "etag",
        HeaderValue::from_str(&part.etag)
            .map_err(|_| Error::invalid("Invalid stored part ETag"))?,
    );
    // Clients check each part against these: the Python SDK's upload_chunks_concurrently fails
    // a part whose response lacks the CRC32C field.
    super::insert_hashes(response.headers_mut(), part.hashes);
    Ok(response)
}

async fn complete(
    request: Request<RequestBody>,
    lease: storage::UploadLease,
    headers: put::Headers,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let bytes = crate::bounded_body::read_body(request.into_body(), MANIFEST_LIMIT).await?;
    if super::sha256_mismatch(headers.sha256, &bytes) {
        return Err(signed_digest_mismatch());
    }
    if headers.expected != Default::default() {
        return Err(Error::unsupported(
            "checksums on a multipart completion document",
        ));
    }
    let parts = manifest::parse(&bytes)?;
    // The parts are copied in detached work that commits whatever this request reports, and the
    // time it takes grows with the object. Timing out would answer 503 for an upload that then
    // succeeds and leave a retry with NoSuchUpload, so wait for the durable outcome.
    let wait = crate::server::begin_catalog_wait(ctx.activity).await;
    let record = ctx
        .state
        .catalog
        .complete_xml_multipart(lease, parts, ctx.config.max_object_bytes())
        .await
        .map_err(|error| storage_error(error, ctx))?;
    drop(wait);
    let mut response = document(render::complete_body(ctx.endpoints.api_base_url(), &record));
    crate::csek::insert_response_headers(&record, response.headers_mut());
    let mut headers = super::media_headers(&record, super::CacheScope::Private)?;
    // The completion document carries the ETag; Cloud Storage sends no ETag header beside it
    // (recorded 2026-11-16).
    super::move_headers(
        &mut headers,
        response.headers_mut(),
        &[
            "x-goog-hash",
            "x-goog-generation",
            "x-goog-metageneration",
            "x-goog-stored-content-length",
        ],
    );
    Ok(response)
}
