//! Bounded XML bucket listing over the shared authorized catalog cursor.

use super::{Error, MinValue, ReadRequest, element, number, signing_parameter};
use crate::server::RequestBody as Incoming;
use crate::{
    auth, json,
    server::{ObjectCtx, ResponseBody, anonymous_error, boxed},
    storage::{ObjectListCursor, ObjectListPage, ObjectListQuery},
};
use http_body_util::Full;
use hyper::header::HeaderValue;
use hyper::{HeaderMap, Request, Response, StatusCode, body::Bytes};
use std::collections::BTreeMap;

struct Params {
    query: ObjectListQuery,
    limit: usize,
    v2: bool,
    encoded: bool,
    interop: bool,
    marker: String,
    generation_marker: Option<i64>,
    continuation: Option<String>,
    start_after: Option<String>,
}

fn invalid(message: &str) -> Error {
    Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", message)
}

fn parse(bucket: String, raw: &str, headers: &HeaderMap) -> Result<Params, Error> {
    let mut values = crate::auth::signature::xml_query(raw, headers)?;
    let v2 = match values.remove("list-type").as_deref() {
        None | Some("1") => false,
        Some("2") => true,
        _ => return Err(invalid("list-type must be 1 or 2")),
    };
    let limit = match values.remove("max-keys") {
        None => 1000,
        Some(value) if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => value
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("max-keys must be a positive uint32"))?
            as usize,
        Some(_) => return Err(invalid("max-keys must be a uint32")),
    };
    let encoded = match values.remove("encoding-type").as_deref() {
        None => false,
        Some("url") => true,
        _ => return Err(invalid("encoding-type must be url")),
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
            return Err(invalid(
                "x-goog-interop-list-objects-format must be enabled or disabled",
            ));
        }
    };
    let (marker, generation_marker, continuation, start_after) = if v2 {
        match values.remove("fetch-owner").as_deref() {
            None | Some("false") => {}
            Some("true") => return Err(Error::unsupported("fetch-owner=true")),
            Some(_) => return Err(invalid("fetch-owner must be true or false")),
        }
        let continuation = values.remove("continuation-token");
        let start_after = values.remove("start-after");
        if let Some(token) = &continuation {
            let token = token
                .strip_prefix("xml2.")
                .ok_or_else(|| invalid("Invalid XML continuation-token"))?;
            query.cursor = Some(json::decode_object_page_token(
                token,
                &query,
                json::Projection::NoAcl,
            )?);
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
            return Err(invalid(
                "A generation marker requires versions and a non-empty name marker",
            ));
        }
        query.cursor = marker_cursor(&marker, generation, &query);
        (marker, generation, None, None)
    };
    if let Some(key) = values.keys().find(|key| !signing_parameter(key)) {
        return Err(Error::unsupported(key));
    }
    Ok(Params {
        query,
        limit,
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
        return Err(invalid("Use only one name or generation marker spelling"));
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
                .map(|index| &marker[..query.prefix.len() + index + query.delimiter.len()])
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
    mut operation: ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let access = ctx
        .authorize(auth::OBJECTS_LIST, &operation.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    let params = parse(
        operation.bucket.clone(),
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
        return Err(invalid(
            "Require one x-goog-interop-list-objects-format header",
        ));
    }
    headers.remove("x-goog-interop-list-objects-format");
    super::headers(&headers, &mut operation)?;
    if operation.conditions != crate::object::ObjectConditions::default()
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
        .await_catalog(ctx.state.catalog.list_objects_authorized(
            params.query.clone(),
            params.limit.min(1000),
            &access,
        ))
        .await
        .map_err(|error| anonymous_error(error.into(), ctx.caller))?;
    let body = render(&params, &page)?;
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/xml; charset=UTF-8"),
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

fn render(params: &Params, page: &ObjectListPage) -> Result<String, Error> {
    let root = if params.query.versions && params.interop {
        "ListVersionsResult"
    } else {
        "ListBucketResult"
    };
    let mut body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><{root} xmlns=\"http://doc.s3.amazonaws.com/2006-03-01\">"
    );
    element(&mut body, "Name", &params.query.bucket);
    name_element(&mut body, "Prefix", &params.query.prefix, params);
    name_element(&mut body, "Delimiter", &params.query.delimiter, params);
    element(&mut body, "MaxKeys", &params.limit.to_string());
    element(
        &mut body,
        "IsTruncated",
        if page.next.is_some() { "true" } else { "false" },
    );
    if params.encoded {
        element(&mut body, "EncodingType", "url");
    }
    render_cursor(&mut body, params, page);
    for object in &page.items {
        let tag = if params.query.versions {
            "Version"
        } else {
            "Contents"
        };
        body.push('<');
        body.push_str(tag);
        body.push('>');
        name_element(&mut body, "Key", &object.name, params);
        element(
            &mut body,
            if params.interop && params.query.versions {
                "VersionId"
            } else {
                "Generation"
            },
            &object.generation.to_string(),
        );
        element(
            &mut body,
            "MetaGeneration",
            &object.metageneration.to_string(),
        );
        element(&mut body, "LastModified", &json::timestamp(object.updated)?);
        element(&mut body, "ETag", &super::object_etag(object));
        element(&mut body, "Size", &object.size.to_string());
        element(&mut body, "StorageClass", &object.storage_class);
        if params.query.versions {
            element(
                &mut body,
                "IsLatest",
                if object.time_deleted.is_none() {
                    "true"
                } else {
                    "false"
                },
            );
            if let Some(deleted) = object.time_deleted {
                element(&mut body, "DeletedTime", &json::timestamp(deleted)?);
            }
        }
        body.push_str("</");
        body.push_str(tag);
        body.push('>');
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

fn render_cursor(body: &mut String, params: &Params, page: &ObjectListPage) {
    if params.v2 {
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
                &format!(
                    "xml2.{}",
                    json::encode_object_page_token(&params.query, json::Projection::NoAcl, cursor)
                ),
            );
        }
    } else {
        let marker_tag = if params.interop && params.query.versions {
            "KeyMarker"
        } else {
            "Marker"
        };
        let generation_tag = if params.interop && params.query.versions {
            "VersionIdMarker"
        } else {
            "GenerationMarker"
        };
        name_element(body, marker_tag, &params.marker, params);
        if let Some(generation) = params.generation_marker {
            element(body, generation_tag, &generation.to_string());
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
    }
}
