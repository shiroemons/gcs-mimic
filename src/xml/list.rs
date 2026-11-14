//! Bounded XML bucket listing over the shared authorized catalog cursor.

use super::{Error, MinValue, ReadRequest, accepted_parameter, element, number};
use crate::server::RequestBody as Incoming;
use crate::{
    auth, json,
    server::{ObjectCtx, ResponseBody, anonymous_error, boxed},
    storage::{ObjectListCursor, ObjectListPage, ObjectListQuery},
};
use hyper::header::HeaderValue;
use hyper::{HeaderMap, Request, Response};
use std::collections::BTreeMap;

pub(super) struct Params {
    query: ObjectListQuery,
    limit: usize,
    /// Whether the request named a usable `max-keys`; only then does a version 1 listing echo it.
    max_keys_given: bool,
    v2: bool,
    encoded: bool,
    interop: bool,
    marker: String,
    generation_marker: Option<i64>,
    continuation: Option<String>,
    start_after: Option<String>,
}

pub(super) fn parse(bucket: String, raw: &str, headers: &HeaderMap) -> Result<Params, Error> {
    let mut values = crate::auth::signature::xml_query(raw, headers)?;
    let v2 = match values.remove("list-type").as_deref() {
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
        None => None,
        Some(value) if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => Some(
            value
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| Error::invalid("max-keys must be a positive uint32"))?
                as usize,
        ),
        Some(_) => None,
    };
    let limit = max_keys.unwrap_or(1000);
    let encoded = match values.remove("encoding-type").as_deref() {
        None => false,
        Some("url") => true,
        _ => return Err(Error::invalid("encoding-type must be url")),
    };
    let mut query = ObjectListQuery {
        bucket,
        prefix: values.remove("prefix").unwrap_or_default(),
        delimiter: values.remove("delimiter").unwrap_or_default(),
        versions: values.remove("versions").is_some(),
        ..ObjectListQuery::default()
    };
    let interop = match headers.get("x-goog-interop-list-objects-format") {
        None => false,
        Some(value) if value == "disabled" => false,
        Some(value) if value == "enabled" => true,
        Some(_) => {
            return Err(Error::invalid(
                "x-goog-interop-list-objects-format must be enabled or disabled",
            ));
        }
    };
    let (marker, generation_marker, continuation, start_after) = if v2 {
        match values.remove("fetch-owner").as_deref() {
            None | Some("false") => {}
            Some("true") => return Err(Error::unsupported("fetch-owner=true")),
            Some(_) => return Err(Error::invalid("fetch-owner must be true or false")),
        }
        let continuation = values.remove("continuation-token");
        let start_after = values.remove("start-after");
        if let Some(token) = &continuation {
            let token = token
                .strip_prefix("xml2.")
                .ok_or_else(|| Error::invalid("Invalid XML continuation-token"))?;
            query.cursor = Some(json::decode_object_page_token(token, &query)?);
        } else if let Some(marker) = &start_after {
            query.cursor = marker_cursor(marker, None, &query);
        }
        (String::new(), None, continuation, start_after)
    } else {
        let marker = take_alias(&mut values, "marker", "key-marker")?.unwrap_or_default();
        let generation = take_alias(&mut values, "generation-marker", "version-id-marker")?
            .map(|value| number(&value, MinValue::One))
            .transpose()?;
        if generation.is_some() && (marker.is_empty() || !query.versions) {
            return Err(Error::invalid(
                "A generation marker requires versions and a non-empty name marker",
            ));
        }
        query.cursor = marker_cursor(&marker, generation, &query);
        (marker, generation, None, None)
    };
    if let Some(key) = values.keys().find(|key| !accepted_parameter(key)) {
        return Err(Error::unsupported(key));
    }
    Ok(Params {
        query,
        limit,
        max_keys_given: max_keys.is_some(),
        v2,
        encoded,
        interop,
        marker,
        generation_marker,
        continuation,
        start_after,
    })
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
    generation: Option<i64>,
    query: &ObjectListQuery,
) -> Option<ObjectListCursor> {
    if marker.is_empty() {
        return None;
    }
    let prefix = (!query.delimiter.is_empty())
        .then(|| {
            marker
                .strip_prefix(&query.prefix)
                .and_then(|tail| tail.find(&query.delimiter))
                .and_then(|index| marker.get(..query.prefix.len() + index + query.delimiter.len()))
        })
        .flatten();
    Some(ObjectListCursor {
        name: prefix.unwrap_or(marker).to_owned(),
        is_prefix: prefix.is_some(),
        generation: if prefix.is_some() { None } else { generation },
    })
}

pub(crate) async fn respond(
    request: Request<Incoming>,
    operation: ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let access = ctx
        .authorize(auth::OBJECTS_LIST, &operation.target.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    let params = parse(
        operation.target.bucket.clone(),
        request.uri().query().unwrap_or_default(),
        request.headers(),
    )?;
    let mut headers = request.headers().clone();
    if headers
        .get_all("x-goog-interop-list-objects-format")
        .iter()
        .count()
        > 1
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
            params.limit.min(1000),
            &access,
        ))
        .await
        .map_err(|error| anonymous_error(error.into(), ctx.caller))?;
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

fn name_element(body: &mut String, name: &str, value: &str, params: &Params) {
    if params.encoded {
        element(
            body,
            name,
            &crate::url::encode_object_name(value).replace('/', "%2F"),
        );
    } else {
        element(body, name, value);
    }
}

pub(super) fn render(params: &Params, page: &ObjectListPage) -> Result<String, Error> {
    let root = if params.query.versions && params.interop {
        "ListVersionsResult"
    } else {
        "ListBucketResult"
    };
    let mut body = format!(
        "{}<{root} xmlns='http://doc.s3.amazonaws.com/2006-03-01'>",
        super::DECLARATION
    );
    element(&mut body, "Name", &params.query.bucket);
    name_element(&mut body, "Prefix", &params.query.prefix, params);
    if params.v2 {
        render_header_v2(&mut body, params, page);
    } else {
        render_header_v1(&mut body, params, page);
    }
    for object in &page.items {
        render_item(&mut body, params, object)?;
    }
    for prefix in &page.prefixes {
        body.push_str("<CommonPrefixes>");
        name_element(&mut body, "Prefix", prefix, params);
        body.push_str("</CommonPrefixes>");
    }
    body.push_str("</");
    body.push_str(root);
    body.push('>');
    Ok(body)
}

/// Recorded version 1 order: Marker, GenerationMarker (versions only, empty when absent),
/// Delimiter, NextMarker, MaxKeys (only when requested), IsTruncated.
fn render_header_v1(body: &mut String, params: &Params, page: &ObjectListPage) {
    let (marker_tag, generation_tag) = marker_tags(params);
    name_element(body, marker_tag, &params.marker, params);
    if params.query.versions {
        let generation = params
            .generation_marker
            .map(|generation| generation.to_string())
            .unwrap_or_default();
        element(body, generation_tag, &generation);
    }
    if !params.query.delimiter.is_empty() {
        name_element(body, "Delimiter", &params.query.delimiter, params);
    }
    if let Some(cursor) = &page.next {
        name_element(body, &format!("Next{marker_tag}"), &cursor.name, params);
        if let Some(generation) = cursor.generation {
            element(
                body,
                &format!("Next{generation_tag}"),
                &generation.to_string(),
            );
        }
    }
    if params.max_keys_given {
        element(body, "MaxKeys", &params.limit.to_string());
    }
    element(
        body,
        "IsTruncated",
        if page.next.is_some() { "true" } else { "false" },
    );
    if params.encoded {
        element(body, "EncodingType", "url");
    }
}

/// Version 2 listings were not recorded; they keep MaxKeys always and the S3 field order.
fn render_header_v2(body: &mut String, params: &Params, page: &ObjectListPage) {
    if !params.query.delimiter.is_empty() {
        name_element(body, "Delimiter", &params.query.delimiter, params);
    }
    element(body, "MaxKeys", &params.limit.to_string());
    element(
        body,
        "IsTruncated",
        if page.next.is_some() { "true" } else { "false" },
    );
    if params.encoded {
        element(body, "EncodingType", "url");
    }
    element(
        body,
        "KeyCount",
        &(page.items.len() + page.prefixes.len()).to_string(),
    );
    if let Some(token) = &params.continuation {
        element(body, "ContinuationToken", token);
    }
    if let Some(start) = &params.start_after {
        name_element(body, "StartAfter", start, params);
    }
    if let Some(cursor) = &page.next {
        element(
            body,
            "NextContinuationToken",
            &format!("xml2.{}", json::encode_object_page_token(cursor)),
        );
    }
}

/// A recorded `Contents` entry: Key, Generation, MetaGeneration, LastModified, ETag, Size. A
/// recorded `Version` adds IsLatest after MetaGeneration and an always-present, empty-unless-
/// deleted DeletedTime after LastModified. Neither carries a StorageClass; the version 2 and
/// interoperable forms were not recorded and keep one after Size.
fn render_item(
    body: &mut String,
    params: &Params,
    object: &crate::object::ObjectRecord,
) -> Result<(), Error> {
    let versions = params.query.versions;
    let tag = if versions { "Version" } else { "Contents" };
    body.push('<');
    body.push_str(tag);
    body.push('>');
    name_element(body, "Key", &object.name, params);
    element(
        body,
        if params.interop && versions {
            "VersionId"
        } else {
            "Generation"
        },
        &object.generation.to_string(),
    );
    element(body, "MetaGeneration", &object.metageneration.to_string());
    if versions {
        element(
            body,
            "IsLatest",
            if object.time_deleted.is_none() {
                "true"
            } else {
                "false"
            },
        );
    }
    element(body, "LastModified", &json::timestamp(object.updated)?);
    if versions {
        let deleted = object
            .time_deleted
            .map(json::timestamp)
            .transpose()?
            .unwrap_or_default();
        element(body, "DeletedTime", &deleted);
    }
    element(body, "ETag", &super::object_etag(object));
    element(body, "Size", &object.size.to_string());
    if params.v2 || params.interop {
        element(body, "StorageClass", object.storage_class.as_str());
    }
    body.push_str("</");
    body.push_str(tag);
    body.push('>');
    Ok(())
}

/// Version 1 marker element names: the interoperable versions listing uses S3's names.
fn marker_tags(params: &Params) -> (&'static str, &'static str) {
    if params.interop && params.query.versions {
        ("KeyMarker", "VersionIdMarker")
    } else {
        ("Marker", "GenerationMarker")
    }
}
