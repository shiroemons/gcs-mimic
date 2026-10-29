use super::{
    JsonError, Projection, object_resource,
    query::{boolean, finish, parse_query, projection},
};
use crate::storage::{ObjectListPage, ObjectListQuery};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectListParams {
    pub query: ObjectListQuery,
    pub projection: Projection,
    pub max_results: usize,
}

impl ObjectListParams {
    pub(crate) fn empty_soft_deleted_message(
        &self,
        page: &ObjectListPage,
        policy: Option<&crate::bucket::SoftDeletePolicy>,
    ) -> Option<&'static str> {
        (self.query.soft_deleted
            && policy.is_none()
            && page.items.is_empty()
            && page.prefixes.is_empty()
            && page.next.is_none())
        .then_some(
            "Returning an empty soft-deleted listing because softDeletePolicy is not configured; set softDeletePolicy.retentionDurationSeconds=604800 on the bucket to retain future deletions for seven days",
        )
    }
}

pub(super) fn parse_list(bucket: String, raw: &str) -> Result<ObjectListParams, JsonError> {
    let mut params = parse_query(raw)?;
    super::Fields::take(&mut params, "Objects")?;
    for name in [
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
        .remove("filter")
        .is_some_and(|filter| !filter.is_empty())
    {
        return Err(JsonError::unsupported_object("filter"));
    }
    for name in ["includeFoldersAsPrefixes", "prettyPrint"] {
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
                1000
            } else {
                value.min(1000) as usize
            }
        }
    };
    let mut query = ObjectListQuery {
        bucket,
        prefix: params.remove("prefix").unwrap_or_default(),
        delimiter: params.remove("delimiter").unwrap_or_default(),
        match_glob: params
            .remove("matchGlob")
            .filter(|pattern| !pattern.is_empty())
            .map(|pattern| {
                crate::match_glob::Glob::parse(&pattern).map_err(JsonError::invalid_parameter)
            })
            .transpose()?,
        start_offset: params.remove("startOffset").unwrap_or_default(),
        end_offset: params.remove("endOffset").unwrap_or_default(),
        include_trailing_delimiter: boolean(&mut params, "includeTrailingDelimiter")?,
        versions: boolean(&mut params, "versions")?,
        soft_deleted: boolean(&mut params, "softDeleted")?,
        cursor: None,
    };
    if query.soft_deleted && query.versions {
        return Err(JsonError::invalid_argument(
            "softDeleted=true cannot be combined with versions=true; omit versions",
        ));
    }
    if query.match_glob.is_some() && !query.delimiter.is_empty() && query.delimiter != "/" {
        return Err(JsonError::invalid_parameter(
            "matchGlob requires delimiter=/ or no delimiter; use matchGlob=**/*.txt&delimiter=/",
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_soft_deleted_listing_explains_missing_policy_only() {
        let params = parse_list("example-bucket".into(), "softDeleted=true").unwrap();
        let mut page = ObjectListPage {
            items: Vec::new(),
            prefixes: Vec::new(),
            next: None,
        };
        let message = params.empty_soft_deleted_message(&page, None).unwrap();
        assert!(message.contains("not configured"));
        assert!(message.contains("retentionDurationSeconds=604800"));
        assert_eq!(
            object_list(&page, &params, "http://localhost:4443").unwrap(),
            json!({"kind":"storage#objects"})
        );
        let policy = crate::bucket::SoftDeletePolicy {
            retention_duration_seconds: 604800,
            effective_time: std::time::UNIX_EPOCH,
        };
        assert!(
            params
                .empty_soft_deleted_message(&page, Some(&policy))
                .is_none()
        );
        let normal = parse_list("example-bucket".into(), "").unwrap();
        assert!(normal.empty_soft_deleted_message(&page, None).is_none());
        page.prefixes.push("retained/".into());
        assert!(params.empty_soft_deleted_message(&page, None).is_none());
        page.prefixes.clear();
        page.next = Some(crate::storage::ObjectListCursor {
            name: "retained".into(),
            is_prefix: false,
            generation: Some(1),
        });
        assert!(params.empty_soft_deleted_message(&page, None).is_none());
    }
}
