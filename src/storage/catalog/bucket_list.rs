use super::*;

/// A bounded keyset page. Continue after the last returned name when `has_more` is true.
pub(crate) struct BucketListPage {
    pub items: Vec<BucketRecord>,
    pub has_more: bool,
}

impl Catalog {
    /// Returns at most `limit` rows within the expanded metadata budget (or one large row).
    /// Use `list_buckets_page` when a continuation indicator is needed.
    #[cfg(test)]
    pub(crate) async fn list_buckets(
        &self,
        project: &str,
        prefix: Option<&str>,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<BucketRecord>, CatalogError> {
        Ok(self
            .list_buckets_page(project, prefix, start_after, limit)
            .await?
            .items)
    }

    /// Bounds expansion before decoding, including lookahead; a single large row still advances.
    pub(crate) async fn list_buckets_page(
        &self,
        project: &str,
        prefix: Option<&str>,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<BucketListPage, CatalogError> {
        self.list_buckets_page_among(Some(vec![project.to_owned()]), prefix, start_after, limit)
            .await
    }

    /// Lists buckets owned by any of `projects`, or by every project when `projects` is `None`.
    pub(crate) async fn list_buckets_page_among(
        &self,
        projects: Option<Vec<String>>,
        prefix: Option<&str>,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<BucketListPage, CatalogError> {
        if limit > MAX_LIST_LIMIT {
            return Err(CatalogError::Unavailable(
                format!("list limit exceeds {MAX_LIST_LIMIT}").into(),
            ));
        }
        if projects.as_ref().is_some_and(Vec::is_empty) {
            return Ok(BucketListPage {
                items: Vec::new(),
                has_more: false,
            });
        }
        let prefix = prefix.unwrap_or("").to_owned();
        let start_after = start_after.map(str::to_owned);
        self.run(move |connection| {
            bucket_list_page(
                connection,
                projects.as_deref(),
                &prefix,
                start_after.as_deref(),
                limit,
            )
        })
        .await
    }
}

pub(super) fn bucket_list_page(
    connection: &Connection,
    projects: Option<&[String]>,
    prefix: &str,
    start_after: Option<&str>,
    limit: usize,
) -> Result<BucketListPage, CatalogError> {
    use objects::SeekStart;
    use rusqlite::types::Value;
    let upper = objects::prefix_end(prefix);
    // Both bounds remain indexable, including when a page starts beyond the prefix.
    let (lower, comparison) = match start_after {
        Some(start) if start >= prefix => (start, SeekStart::Exclusive),
        _ => (prefix, SeekStart::Inclusive),
    };
    // One static text per bound shape keeps both name bounds indexable; the project list is
    // bound as one JSON array so its length does not change the statement text.
    macro_rules! projects {
        () => {
            "project IN (SELECT value FROM json_each(?)) AND "
        };
    }
    macro_rules! page {
        ($projects:expr, $comparison:literal, $upper:literal) => {
            concat!(
                "SELECT ",
                bucket_columns!(),
                " FROM buckets WHERE ",
                $projects,
                "name ",
                $comparison,
                " ? ",
                $upper,
                " ORDER BY name LIMIT ?"
            )
        };
    }
    let sql = match (projects.is_some(), comparison, upper.is_some()) {
        (false, SeekStart::Exclusive, false) => page!("", ">", ""),
        (false, SeekStart::Exclusive, true) => page!("", ">", "AND name < CAST(? AS TEXT)"),
        (false, SeekStart::Inclusive, false) => page!("", ">=", ""),
        (false, SeekStart::Inclusive, true) => page!("", ">=", "AND name < CAST(? AS TEXT)"),
        (true, SeekStart::Exclusive, false) => page!(projects!(), ">", ""),
        (true, SeekStart::Exclusive, true) => page!(projects!(), ">", "AND name < CAST(? AS TEXT)"),
        (true, SeekStart::Inclusive, false) => page!(projects!(), ">=", ""),
        (true, SeekStart::Inclusive, true) => {
            page!(projects!(), ">=", "AND name < CAST(? AS TEXT)")
        }
    };
    let mut values = Vec::new();
    if let Some(projects) = projects {
        values.push(Value::Text(
            serde_json::to_string(projects).map_err(unavailable)?,
        ));
    }
    values.push(Value::Text(lower.to_owned()));
    if let Some(upper) = upper {
        values.push(Value::Blob(upper));
    }
    values.push(Value::Integer(
        i64::try_from(limit).map_err(unavailable)? + 1,
    ));
    let mut statement = connection.prepare_cached(sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(values))?;
    let mut records = Vec::new();
    let mut remaining = BUCKET_LIST_METADATA_BUDGET;
    let mut has_more = false;
    while let Some(row) = rows.next()? {
        // Charge typed rules, JSON values and serialization before allocating any of them.
        let charge = bucket_metadata_charge(row)?;
        if records.len() == limit || (!records.is_empty() && charge > remaining) {
            has_more = !records.is_empty();
            break;
        }
        records.push(read_bucket(row)?);
        remaining = remaining.saturating_sub(charge);
    }
    Ok(BucketListPage {
        items: records,
        has_more,
    })
}

pub(super) fn bucket_metadata_charge(row: &Row<'_>) -> Result<usize, CatalogError> {
    let mut charge = 4096usize;
    for (column, multiplier) in [
        (0, 8),
        (1, 8),
        (2, 8),
        (3, 8),
        (4, 8),
        (5, 16),
        (11, 64),
        (14, 64),
    ] {
        let bytes = row.get_ref(column)?.as_str().map_err(unavailable)?.len();
        charge = charge.saturating_add(bytes.saturating_mul(multiplier));
    }
    Ok(charge)
}
