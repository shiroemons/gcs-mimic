use super::{
    JsonError, Projection, object_resource,
    query::{boolean, projection, standard_parameters},
};
use crate::percent::parse_query;
use crate::storage::{FoldersAsPrefixes, ListQueryError, ObjectListPage, ObjectListQuery};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObjectListParams {
    pub query: ObjectListQuery,
    pub projection: Projection,
    pub max_results: usize,
    /// The JSON API answered `maxResults=0` with an empty page; other protocols treat a zero
    /// page size as the default.
    pub empty_page: bool,
}

pub(super) fn parse_list(bucket: String, raw: &str) -> Result<ObjectListParams, JsonError> {
    let mut params = parse_query(raw)?;
    super::Fields::take(&mut params, super::Schema::Objects)?;
    standard_parameters(&mut params, &[], JsonError::unsupported_object)?;
    if params
        .remove("filter")
        .is_some_and(|filter| !filter.is_empty())
    {
        return Err(JsonError::unsupported_object("filter"));
    }
    let include_folders_as_prefixes = boolean(&mut params, "includeFoldersAsPrefixes")?;
    // The routing layer applies the value; prettyPrint=false selects compact bodies.
    boolean(&mut params, "prettyPrint")?;
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
    let mut empty_page = false;
    let max_results = match super::integer::take_max_results(&mut params)? {
        None => 1000,
        Some(value) => {
            empty_page = value == 0;
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
            .map(|pattern| crate::match_glob::Glob::parse(&pattern).map_err(JsonError::from))
            .transpose()?,
        start_offset: params.remove("startOffset").unwrap_or_default(),
        end_offset: params.remove("endOffset").unwrap_or_default(),
        include_trailing_delimiter: boolean(&mut params, "includeTrailingDelimiter")?,
        versions: boolean(&mut params, "versions")?,
        soft_deleted: boolean(&mut params, "softDeleted")?,
        cursor: None,
        page_bytes: None,
    };
    query.validate(if include_folders_as_prefixes {
        FoldersAsPrefixes::Requested
    } else {
        FoldersAsPrefixes::Omitted
    })?;
    if let Some(token) = params.remove("pageToken").filter(|token| !token.is_empty()) {
        query.cursor = Some(crate::page_token::decode_object_page_token(&token, &query)?);
    }
    // objects.list ignores a query parameter it does not declare, such as the autoPaginate
    // option @google-cloud/storage 9.0.0 Bucket#getFiles sends: Cloud Storage listed the objects
    // (case obj-list-undeclared-query, recorded 2026-11-13), and fake-gcs-server reads only the
    // parameters it knows. Unsupported standard parameters were rejected above.
    Ok(ObjectListParams {
        query,
        projection,
        max_results,
        empty_page,
    })
}

impl From<ListQueryError> for JsonError {
    fn from(error: ListQueryError) -> Self {
        match error {
            ListQueryError::SoftDeletedWithVersions => Self::invalid_argument(
                "softDeleted=true cannot be combined with versions=true; omit versions",
            ),
            ListQueryError::FoldersAsPrefixesNeedSlash => Self::invalid_parameter(
                "includeFoldersAsPrefixes requires delimiter=/; use includeFoldersAsPrefixes=true&delimiter=/",
            ),
            // Cloud Storage's text and reason (recorded 2026-11).
            ListQueryError::GlobNeedsSlashDelimiter => Self::invalid(
                "When listing with a glob pattern, the only supported delimiter is '/'.",
            ),
        }
    }
}

pub(crate) fn object_list(
    page: &ObjectListPage,
    params: &ObjectListParams,
    base_url: &str,
) -> Result<Value, JsonError> {
    // Recorded order: kind, nextPageToken, items. The position of prefixes is unverified.
    let mut value = Map::from_iter([("kind".into(), "storage#objects".into())]);
    if let Some(cursor) = &page.next {
        value.insert(
            "nextPageToken".into(),
            crate::page_token::encode_object_page_token(cursor).into(),
        );
    }
    if !page.prefixes.is_empty() {
        value.insert("prefixes".into(), Value::from(page.prefixes.as_slice()));
    }
    if !page.items.is_empty() {
        value.insert(
            "items".into(),
            page.items
                .iter()
                .map(|object| object_resource(object, base_url, params.projection))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        );
    }
    Ok(Value::Object(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_glob_page_retains_its_continuation_token() {
        let params = parse_list("example-bucket".into(), "matchGlob=z*&softDeleted=true").unwrap();
        let cursor = crate::storage::ObjectListCursor {
            name: "a".into(),
            is_prefix: false,
            generation: Some(3),
        };
        let page = ObjectListPage {
            items: Vec::new(),
            prefixes: Vec::new(),
            next: Some(cursor.clone()),
            bucket_metageneration: None,
        };
        let value = object_list(&page, &params, "http://localhost:4443").unwrap();
        assert_eq!(value["kind"], "storage#objects");
        assert!(value.get("items").is_none());
        assert!(value.get("prefixes").is_none());
        assert_eq!(
            value["nextPageToken"],
            crate::page_token::encode_object_page_token(&cursor)
        );
    }
}
