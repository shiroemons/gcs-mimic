use super::{
    JsonError, Projection, object_resource,
    query::{finish, parse_query, projection},
};
use crate::storage::{ObjectListPage, ObjectListQuery};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectListParams {
    pub query: ObjectListQuery,
    pub projection: Projection,
    pub max_results: usize,
}

pub(super) fn parse_list(bucket: String, raw: &str) -> Result<ObjectListParams, JsonError> {
    let mut params = parse_query(raw)?;
    for name in [
        "filter",
        "fields",
        "userProject",
        "key",
        "oauth_token",
        "quotaUser",
        "userIp",
        "callback",
    ] {
        if params.contains_key(name) {
            return Err(JsonError::unsupported_object(name));
        }
    }
    if params
        .remove("matchGlob")
        .is_some_and(|glob| !glob.is_empty())
    {
        return Err(JsonError::unsupported_object("matchGlob"));
    }
    for name in [
        "versions",
        "softDeleted",
        "includeFoldersAsPrefixes",
        "prettyPrint",
    ] {
        if boolean(&mut params, name)? {
            return Err(JsonError::unsupported_object(name));
        }
    }
    match params.remove("alt").as_deref() {
        None | Some("json") => {}
        Some("media") => return Err(JsonError::unsupported_object("alt=media on objects.list")),
        Some(_) => {
            return Err(JsonError::new(
                hyper::StatusCode::BAD_REQUEST,
                "invalidAltValue",
                "alt must be json for object listing",
            ));
        }
    }
    let projection = projection(&mut params)?;
    let max_results = match params.remove("maxResults") {
        None => 1000,
        Some(value) => {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(JsonError::invalid_parameter("maxResults must be a uint32"));
            }
            let value = value
                .parse::<u32>()
                .map_err(|_| JsonError::invalid_parameter("maxResults must be a uint32"))?;
            if value == 0 {
                return Err(JsonError::invalid_parameter("maxResults must be positive"));
            }
            value.min(1000) as usize
        }
    };
    let mut query = ObjectListQuery {
        bucket,
        prefix: params.remove("prefix").unwrap_or_default(),
        delimiter: params.remove("delimiter").unwrap_or_default(),
        start_offset: params.remove("startOffset").unwrap_or_default(),
        end_offset: params.remove("endOffset").unwrap_or_default(),
        include_trailing_delimiter: boolean(&mut params, "includeTrailingDelimiter")?,
        cursor: None,
    };
    if let Some(token) = params.remove("pageToken").filter(|token| !token.is_empty()) {
        query.cursor = Some(super::page_token::decode_object_page_token(
            &token, &query, projection,
        )?);
    }
    finish(params)?;
    Ok(ObjectListParams {
        query,
        projection,
        max_results,
    })
}

fn boolean(
    params: &mut std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<bool, JsonError> {
    match params.remove(name).as_deref() {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(JsonError::invalid_parameter(format!(
            "{name} must be true or false"
        ))),
    }
}

pub fn object_list(
    page: &ObjectListPage,
    params: &ObjectListParams,
    base_url: &str,
) -> Result<Value, JsonError> {
    let mut value = json!({"kind": "storage#objects"});
    if !page.items.is_empty() {
        value["items"] = page
            .items
            .iter()
            .map(|object| object_resource(object, base_url, params.projection))
            .collect::<Result<Vec<_>, _>>()?
            .into();
    }
    if !page.prefixes.is_empty() {
        value["prefixes"] = json!(page.prefixes);
    }
    if let Some(cursor) = &page.next {
        value["nextPageToken"] =
            super::page_token::encode_object_page_token(&params.query, params.projection, cursor)
                .into();
    }
    Ok(value)
}
