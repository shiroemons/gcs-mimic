//! Bounded XML bucket listing over the shared authorized catalog cursor.

use super::name_encoding::NameEncoding;
use super::{Error, Namespace, ReadRequest, XmlWriter, accepted_parameter, number};
use crate::server::RequestBody as Incoming;
use crate::{
    auth,
    object::generation::Generation,
    server::{ObjectCtx, ResponseBody, boxed},
    storage::{ObjectListCursor, ObjectListPage, ObjectListQuery},
};
use hyper::header::HeaderValue;
use hyper::{HeaderMap, Request, Response};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// Opens every version 2 continuation token, so that a JSON page token is not taken for one.
const CONTINUATION_PREFIX: &str = "xml2.";

/// The listing version's cursor, echoed in its response.
enum Pagination {
    /// `list-type` 1 (default): `marker`/`key-marker` and `generation-marker`/`version-id-marker`.
    V1 {
        marker: String,
        generation: Option<Generation>,
    },
    /// `list-type=2`: `continuation-token` or `start-after`.
    V2 {
        continuation: ContinuationToken,
        start_after: Option<String>,
    },
}

/// A version 2 `continuation-token` as the request sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ContinuationToken {
    /// Not sent.
    Absent,
    /// Sent empty. Cloud Storage answered 200, listed from the start, or from `start-after`
    /// when that was sent too, and echoed an empty `ContinuationToken` (recorded).
    Empty,
    /// The token of an earlier truncated page, echoed as sent.
    Issued(String),
}

impl From<Option<String>> for ContinuationToken {
    fn from(value: Option<String>) -> Self {
        match value {
            None => Self::Absent,
            Some(token) if token.is_empty() => Self::Empty,
            Some(token) => Self::Issued(token),
        }
    }
}

impl ContinuationToken {
    /// The `ContinuationToken` text, written whenever the request sent the parameter.
    fn echo(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Empty => Some(""),
            Self::Issued(token) => Some(token),
        }
    }
}

/// `x-goog-interop-list-objects-format`: the interoperable versions listing uses S3's names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListFormat {
    Native,
    Interoperable,
}

pub(super) struct Params {
    query: ObjectListQuery,
    /// A usable `max-keys`; both versions echo `MaxKeys` only when it is present (recorded).
    max_keys: Option<usize>,
    pagination: Pagination,
    encoding: NameEncoding,
    format: ListFormat,
}

impl Params {
    fn limit(&self) -> usize {
        self.max_keys.unwrap_or(1000)
    }

    /// Only the interoperable format writes `StorageClass` per item; the recorded native
    /// version 1, version 2 and versions entries carry none.
    fn writes_storage_class(&self) -> bool {
        self.format == ListFormat::Interoperable
    }

    fn interoperable_versions(&self) -> bool {
        self.query.versions && self.format == ListFormat::Interoperable
    }
}

pub(super) fn parse(bucket: String, raw: &str, headers: &HeaderMap) -> Result<Params, Error> {
    let mut values = crate::auth::signature::xml_query(raw, headers)?;
    let version2 = match values.remove("list-type").as_deref() {
        None | Some("1") => false,
        Some("2") => true,
        _ => return Err(Error::invalid("list-type must be 1 or 2")),
    };
    // Cloud Storage answered `max-keys=-1` with the whole listing, as if the parameter were
    // absent, and echoed no `MaxKeys` (recorded). Other values that are not an unsigned
    // decimal are treated the same way by assumption: they cannot be told apart in the
    // record, and ignoring them returns more data instead of dropping any. Zero and values
    // beyond uint32 are digits-only and stay rejected; the record does not cover them.
    let max_keys = match values.remove("max-keys") {
        Some(value) if crate::decimal::Grammar::Digits.admits(&value) => Some(
            value
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| Error::invalid("max-keys must be a positive uint32"))?
                as usize,
        ),
        None | Some(_) => None,
    };
    let encoding = match values.remove("encoding-type").as_deref() {
        None => NameEncoding::Plain,
        Some("url") if version2 => NameEncoding::Form,
        Some("url") => NameEncoding::Percent,
        _ => return Err(Error::invalid("encoding-type must be url")),
    };
    let mut query = ObjectListQuery {
        bucket,
        prefix: values.remove("prefix").unwrap_or_default(),
        delimiter: values.remove("delimiter").unwrap_or_default(),
        versions: values.remove("versions").is_some(),
        ..ObjectListQuery::default()
    };
    let format = match headers
        .get("x-goog-interop-list-objects-format")
        .map(HeaderValue::as_bytes)
    {
        None | Some(b"disabled") => ListFormat::Native,
        Some(b"enabled") => ListFormat::Interoperable,
        Some(_) => {
            return Err(Error::invalid(
                "x-goog-interop-list-objects-format must be enabled or disabled",
            ));
        }
    };
    let pagination = if version2 {
        parse_v2(&mut values, &mut query)?
    } else {
        parse_v1(&mut values, &mut query)?
    };
    if let Some(key) = values.keys().find(|key| !accepted_parameter(key)) {
        return Err(Error::unsupported(key));
    }
    Ok(Params {
        query,
        max_keys,
        pagination,
        encoding,
        format,
    })
}

/// Takes the version 1 markers and sets the cursor they select.
fn parse_v1(
    values: &mut BTreeMap<String, String>,
    query: &mut ObjectListQuery,
) -> Result<Pagination, Error> {
    let marker = take_alias(values, "marker", "key-marker")?.unwrap_or_default();
    let generation = take_alias(values, "generation-marker", "version-id-marker")?
        .map(|value| number(&value))
        .transpose()?;
    if generation.is_some() && (marker.is_empty() || !query.versions) {
        return Err(Error::invalid(
            "A generation marker requires versions and a non-empty name marker",
        ));
    }
    query.cursor = marker_cursor(&marker, generation, query);
    Ok(Pagination::V1 { marker, generation })
}

/// Takes the version 2 parameters and sets the cursor they select: an issued token's position
/// takes precedence over `start-after`, which an empty token leaves in effect (recorded).
fn parse_v2(
    values: &mut BTreeMap<String, String>,
    query: &mut ObjectListQuery,
) -> Result<Pagination, Error> {
    match values.remove("fetch-owner").as_deref() {
        None | Some("false") => {}
        Some("true") => return Err(Error::unsupported("fetch-owner=true")),
        Some(_) => return Err(Error::invalid("fetch-owner must be true or false")),
    }
    let continuation = ContinuationToken::from(values.remove("continuation-token"));
    let start_after = values.remove("start-after");
    query.cursor = match &continuation {
        ContinuationToken::Issued(token) => Some(continuation_cursor(token, query)?),
        ContinuationToken::Absent | ContinuationToken::Empty => start_after
            .as_deref()
            .and_then(|marker| marker_cursor(marker, None, query)),
    };
    Ok(Pagination::V2 {
        continuation,
        start_after,
    })
}

/// The position an issued continuation token names. Cloud Storage answered a token it had not
/// issued (`bogus`) with one fixed document (recorded), so a token without the version 2 prefix
/// and one whose payload does not open are refused alike, without the token's text.
fn continuation_cursor(token: &str, query: &ObjectListQuery) -> Result<ObjectListCursor, Error> {
    token
        .strip_prefix(CONTINUATION_PREFIX)
        .and_then(|payload| crate::page_token::decode_object_page_token(payload, query).ok())
        .ok_or_else(Error::invalid_page_token)
}

fn take_alias(
    values: &mut BTreeMap<String, String>,
    name: &str,
    alias: &str,
) -> Result<Option<String>, Error> {
    let value = values.remove(name);
    let other = values.remove(alias);
    if value.is_some() && other.is_some() {
        return Err(Error::invalid(
            "Use only one name or generation marker spelling",
        ));
    }
    Ok(value.or(other))
}

fn marker_cursor(
    marker: &str,
    generation: Option<Generation>,
    query: &ObjectListQuery,
) -> Option<ObjectListCursor> {
    if marker.is_empty() {
        return None;
    }
    let prefix = (!query.delimiter.is_empty())
        .then(|| {
            let index = marker.strip_prefix(&query.prefix)?.find(&query.delimiter)?;
            marker.get(..query.prefix.len() + index + query.delimiter.len())
        })
        .flatten();
    Some(ObjectListCursor {
        name: prefix.unwrap_or(marker).to_owned(),
        is_prefix: prefix.is_some(),
        generation: if prefix.is_some() {
            None
        } else {
            generation.map(Generation::get)
        },
    })
}

pub(crate) async fn respond(
    request: Request<Incoming>,
    operation: ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let access = ctx
        .authorize(auth::OBJECTS_LIST, operation.target.bucket.as_str())
        .await
        .map_err(|error| Error::from(error).for_caller(ctx.caller))?;
    let params = parse(
        operation.target.bucket.as_str().to_owned(),
        request.uri().query().unwrap_or_default(),
        request.headers(),
    )?;
    let mut headers = request.headers().clone();
    if crate::http_header::sole(&headers, "x-goog-interop-list-objects-format")
        .value()
        .is_err()
    {
        return Err(Error::invalid(
            "Require one x-goog-interop-list-objects-format header",
        ));
    }
    headers.remove("x-goog-interop-list-objects-format");
    let conditions = super::headers(&headers, super::HeaderPolicy::Strict)?;
    if conditions != crate::object::ObjectConditions::default()
        || headers
            .keys()
            .any(|key| crate::csek::is_key_header(key.as_str(), crate::csek::DESTINATION_PREFIX))
    {
        return Err(Error::unsupported(
            "object conditions or encryption on bucket listing",
        ));
    }
    super::empty_body(request.into_body()).await?;
    let page = ctx
        .await_catalog(ctx.state.catalog.list_objects(
            params.query.clone(),
            params.limit().min(1000),
            &access,
        ))
        .await
        .map_err(|error| Error::from(error).for_caller(ctx.caller))?;
    let body = render(&params, &page)?;
    let mut response = super::document_response(body);
    // The catalog read that produced the page also reports the bucket's metageneration, so the
    // header and the listing describe one moment of the bucket.
    if let Some(metageneration) = page.bucket_metageneration {
        response
            .headers_mut()
            .insert("x-goog-metageneration", HeaderValue::from(metageneration));
    }
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static(super::PRIVATE_CACHE_CONTROL),
    );
    Ok(boxed(response))
}

pub(super) fn render(params: &Params, page: &ObjectListPage) -> Result<String, Error> {
    let root = if params.interoperable_versions() {
        "ListVersionsResult"
    } else {
        "ListBucketResult"
    };
    let mut w = XmlWriter::new(root, Namespace::Listing);
    w.element("Name", &params.query.bucket);
    params
        .encoding
        .element(&mut w, "Prefix", &params.query.prefix);
    match &params.pagination {
        Pagination::V1 { marker, generation } => {
            render_header_v1(&mut w, params, marker, *generation, page);
        }
        Pagination::V2 {
            continuation,
            start_after,
        } => render_header_v2(&mut w, params, continuation, start_after.as_deref(), page),
    }
    for object in &page.items {
        render_item(&mut w, params, object)?;
    }
    for prefix in &page.prefixes {
        w.group("CommonPrefixes", |w| {
            params.encoding.element(w, "Prefix", prefix);
        });
    }
    Ok(w.finish())
}

/// Recorded version 1 order: Marker, GenerationMarker (versions only, empty when absent),
/// Delimiter, NextMarker, MaxKeys (only when requested), IsTruncated.
fn render_header_v1(
    w: &mut XmlWriter,
    params: &Params,
    marker: &str,
    generation: Option<Generation>,
    page: &ObjectListPage,
) {
    let encoding = params.encoding;
    let (marker_tag, generation_tag) = marker_tags(params);
    encoding.element(w, marker_tag, marker);
    if params.query.versions {
        let generation = generation.map_or_default(|generation| generation.to_string());
        w.element(generation_tag, &generation);
    }
    if !params.query.delimiter.is_empty() {
        encoding.element(w, "Delimiter", &params.query.delimiter);
    }
    if let Some(cursor) = &page.next {
        encoding.element(w, &format!("Next{marker_tag}"), &cursor.name);
        if let Some(generation) = cursor.generation {
            w.value(&format!("Next{generation_tag}"), generation);
        }
    }
    if let Some(max_keys) = params.max_keys {
        w.value("MaxKeys", max_keys);
    }
    w.value("IsTruncated", page.next.is_some());
    encoding.write_encoding_type(w);
}

/// Recorded version 2 order: StartAfter and ContinuationToken (each only when the request sent
/// it, even empty), NextContinuationToken (truncated pages only), KeyCount, MaxKeys (only when
/// requested), Delimiter (only when non-empty), IsTruncated, EncodingType.
fn render_header_v2(
    w: &mut XmlWriter,
    params: &Params,
    continuation: &ContinuationToken,
    start_after: Option<&str>,
    page: &ObjectListPage,
) {
    let encoding = params.encoding;
    if let Some(start) = start_after {
        encoding.element(w, "StartAfter", start);
    }
    // A page that both continues a token and issues one was not recorded with the two tokens
    // together; the request's echo comes first, as StartAfter precedes ContinuationToken.
    if let Some(token) = continuation.echo() {
        w.element("ContinuationToken", token);
    }
    if let Some(cursor) = &page.next {
        w.element(
            "NextContinuationToken",
            &format!(
                "{CONTINUATION_PREFIX}{}",
                crate::page_token::encode_object_page_token(cursor)
            ),
        );
    }
    // Common prefixes are not keys: three objects and one prefix answered KeyCount 3 (recorded).
    w.value("KeyCount", page.items.len());
    if let Some(max_keys) = params.max_keys {
        w.value("MaxKeys", max_keys);
    }
    if !params.query.delimiter.is_empty() {
        encoding.element(w, "Delimiter", &params.query.delimiter);
    }
    w.value("IsTruncated", page.next.is_some());
    encoding.write_encoding_type(w);
}

/// A recorded `Contents` entry: Key, Generation, MetaGeneration, LastModified, ETag, Size, in
/// version 1 and version 2 alike. A recorded `Version` adds IsLatest after MetaGeneration and an
/// always-present, empty-unless-deleted DeletedTime after LastModified. None carries a
/// StorageClass; the interoperable form was not recorded and keeps one after Size.
fn render_item(
    w: &mut XmlWriter,
    params: &Params,
    object: &crate::object::ObjectRecord,
) -> Result<(), Error> {
    let versions = params.query.versions;
    let tag = if versions { "Version" } else { "Contents" };
    w.try_group(tag, |w| {
        params.encoding.element(w, "Key", &object.name);
        let generation = if params.interoperable_versions() {
            "VersionId"
        } else {
            "Generation"
        };
        w.value(generation, object.generation);
        w.value("MetaGeneration", object.metageneration);
        if versions {
            w.value("IsLatest", object.time_deleted.is_none());
        }
        w.element(
            "LastModified",
            &crate::time_format::timestamp(object.updated)?,
        );
        if versions {
            let deleted = object
                .time_deleted
                .map(crate::time_format::timestamp)
                .transpose()?
                .unwrap_or_default();
            w.element("DeletedTime", &deleted);
        }
        w.element("ETag", &super::object_etag(object));
        w.value("Size", object.size);
        if params.writes_storage_class() {
            w.element("StorageClass", object.storage_class.as_str());
        }
        Ok(())
    })
}

/// Version 1 marker element names: the interoperable versions listing uses S3's names.
fn marker_tags(params: &Params) -> (&'static str, &'static str) {
    if params.interoperable_versions() {
        ("KeyMarker", "VersionIdMarker")
    } else {
        ("Marker", "GenerationMarker")
    }
}
