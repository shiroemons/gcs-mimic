use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObjectListCursor {
    pub name: String,
    pub is_prefix: bool,
    pub generation: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectListQuery {
    pub bucket: String,
    pub prefix: String,
    pub delimiter: String,
    pub(crate) match_glob: Option<crate::match_glob::Glob>,
    pub start_offset: String,
    pub end_offset: String,
    pub include_trailing_delimiter: bool,
    pub versions: bool,
    pub soft_deleted: bool,
    pub(crate) cursor: Option<ObjectListCursor>,
}

#[derive(Debug)]
pub struct ObjectListPage {
    pub items: Vec<ObjectRecord>,
    pub prefixes: Vec<String>,
    pub(crate) next: Option<ObjectListCursor>,
    /// The listed bucket's metageneration, read by the same catalog job as the page so the two
    /// agree. Only an authorized listing fills it; other producers leave it empty.
    pub(crate) bucket_metageneration: Option<i64>,
}

impl Catalog {
    /// Each examined entry requires one indexed seek; unmatched candidates have a page budget.
    pub(crate) async fn list_objects(
        &self,
        query: ObjectListQuery,
        limit: usize,
        access: &BucketAccess,
    ) -> Result<ObjectListPage, ObjectError> {
        debug_assert!(
            (1..=1000).contains(&limit),
            "adapters validate object list limits"
        );
        let limit = limit.clamp(1, 1000);
        let access = access.clone();
        let clock = self.0.clock.clone();
        let soft_deleted = query.soft_deleted;
        let result = self
            .run(move |connection| {
                // The catalog worker serializes mutations; each seek ends before glob matching.
                authorized_bucket(connection, &query.bucket, &access)?;
                let time = clock.now();
                if query.soft_deleted {
                    super::super::soft_delete::purge_expired(
                        connection,
                        &query.bucket,
                        time.try_into()?,
                    )?;
                }
                let mut page = list_page_at(connection, &query, limit, unix_micros(time)?)?;
                page.bucket_metageneration =
                    Some(bucket_metageneration(connection, &query.bucket)?);
                Ok(page)
            })
            .await;
        if soft_deleted {
            self.reclaim_after_commit().await;
        }
        result
    }
}

pub(in crate::storage::catalog) fn prefix_end(prefix: &str) -> Option<Vec<u8>> {
    let mut end = Vec::new();
    prefix_end_into(prefix, &mut end).then_some(end)
}

fn prefix_end_into(prefix: &str, end: &mut Vec<u8>) -> bool {
    end.clear();
    end.extend_from_slice(prefix.as_bytes());
    let Some((index, byte)) = end
        .iter_mut()
        .enumerate()
        .rfind(|(_, byte)| **byte != u8::MAX)
    else {
        return false;
    };
    *byte += 1;
    end.truncate(index + 1);
    true
}

/// Which rows a listing walks: live objects, every live generation, or soft-deleted ones.
#[derive(Clone, Copy)]
pub(super) enum ListSource {
    Live,
    Versions,
    SoftDeleted,
}

impl ListSource {
    fn of(query: &ObjectListQuery) -> Self {
        if query.soft_deleted {
            Self::SoftDeleted
        } else if query.versions {
            Self::Versions
        } else {
            Self::Live
        }
    }
}

/// Whether a seek may return the lower bound itself.
#[derive(Clone, Copy)]
pub(in crate::storage::catalog) enum SeekStart {
    Inclusive,
    Exclusive,
}

/// Whether a listing stops before an upper name bound.
#[derive(Clone, Copy)]
pub(super) enum UpperBound {
    Open,
    Bounded,
}

// Every list statement numbers its parameters the same way: bucket ?1, lower bound ?2, upper
// bound ?3, generation ?4 and the current time ?5. Binding the time instead of embedding it
// gives each listing shape one static text that the connection cache reuses.
macro_rules! visible_live {
    () => {
        "soft_delete_time IS NULL"
    };
}
macro_rules! visible_soft_deleted {
    () => {
        "soft_delete_time IS NOT NULL AND hard_delete_time > ?5"
    };
}
macro_rules! name_upper_bound {
    () => {
        "AND name < CAST(?3 AS TEXT)"
    };
}
macro_rules! list_seek {
    ($source:literal, $comparison:literal, $upper:expr, $visible:expr, $order:literal) => {
        concat!(
            "SELECT ",
            object_columns!(),
            " FROM ",
            $source,
            " WHERE bucket = ?1 AND name ",
            $comparison,
            " CAST(?2 AS TEXT) ",
            $upper,
            " AND ",
            $visible,
            " ORDER BY ",
            $order,
            " LIMIT 1"
        )
    };
}
macro_rules! list_versioned {
    ($upper:expr, $visible:expr) => {
        concat!(
            "SELECT ",
            object_columns!(),
            " FROM objects WHERE bucket = ?1 AND (name, generation) > (CAST(?2 AS TEXT), ?4) ",
            $upper,
            " AND ",
            $visible,
            " ORDER BY name, generation LIMIT 1"
        )
    };
}

pub(super) fn list_seek_sql(
    source: ListSource,
    start: SeekStart,
    upper: UpperBound,
) -> &'static str {
    macro_rules! shapes {
        ($source:literal, $visible:expr, $order:literal) => {
            match (start, upper) {
                (SeekStart::Inclusive, UpperBound::Open) => {
                    list_seek!($source, ">=", "", $visible, $order)
                }
                (SeekStart::Inclusive, UpperBound::Bounded) => {
                    list_seek!($source, ">=", name_upper_bound!(), $visible, $order)
                }
                (SeekStart::Exclusive, UpperBound::Open) => {
                    list_seek!($source, ">", "", $visible, $order)
                }
                (SeekStart::Exclusive, UpperBound::Bounded) => {
                    list_seek!($source, ">", name_upper_bound!(), $visible, $order)
                }
            }
        };
    }
    match source {
        ListSource::SoftDeleted => shapes!(
            "objects INDEXED BY objects_soft_deleted",
            visible_soft_deleted!(),
            "name, generation"
        ),
        ListSource::Versions => shapes!("objects", visible_live!(), "name, generation"),
        // The primary key includes history; a live seek must not walk noncurrent rows.
        ListSource::Live => shapes!(
            "objects INDEXED BY objects_live",
            "time_deleted IS NULL",
            "name"
        ),
    }
}

fn list_versioned_sql(source: ListSource, upper: UpperBound) -> &'static str {
    match (source, upper) {
        (ListSource::SoftDeleted, UpperBound::Open) => {
            list_versioned!("", visible_soft_deleted!())
        }
        (ListSource::SoftDeleted, UpperBound::Bounded) => {
            list_versioned!(name_upper_bound!(), visible_soft_deleted!())
        }
        (_, UpperBound::Open) => list_versioned!("", visible_live!()),
        (_, UpperBound::Bounded) => list_versioned!(name_upper_bound!(), visible_live!()),
    }
}

/// Whether another visible generation of a name follows generation `?4`.
fn later_generation_sql(source: ListSource) -> &'static str {
    match source {
        ListSource::SoftDeleted => concat!(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation > ?4 AND ",
            visible_soft_deleted!(),
            ")"
        ),
        _ => concat!(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation > ?4 AND ",
            visible_live!(),
            ")"
        ),
    }
}

/// Runs a list statement with the leading parameters it declares, so shapes that omit the
/// upper bound, generation or time share one binding convention.
fn query_list_row<T>(
    statement: &mut rusqlite::CachedStatement<'_>,
    values: [&dyn rusqlite::ToSql; 5],
    read: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let count = statement.parameter_count();
    statement.query_row(
        rusqlite::params_from_iter(values.into_iter().take(count)),
        read,
    )
}

/// The three seeks one listing alternates between.
struct ListStatements<'c> {
    inclusive: rusqlite::CachedStatement<'c>,
    exclusive: rusqlite::CachedStatement<'c>,
    versioned: rusqlite::CachedStatement<'c>,
}

impl<'c> ListStatements<'c> {
    fn prepare(
        connection: &'c Connection,
        source: ListSource,
        upper: UpperBound,
    ) -> rusqlite::Result<Self> {
        Ok(Self {
            inclusive: connection.prepare_cached(list_seek_sql(
                source,
                SeekStart::Inclusive,
                upper,
            ))?,
            exclusive: connection.prepare_cached(list_seek_sql(
                source,
                SeekStart::Exclusive,
                upper,
            ))?,
            versioned: connection.prepare_cached(list_versioned_sql(source, upper))?,
        })
    }
}

/// Where the next seek starts, derived from the previous page entry.
struct SeekPosition {
    after: bool,
    after_generation: Option<i64>,
    trailing_prefix: bool,
}

/// Advances `lower` past the previous entry; `None` means the listing is exhausted.
fn seek_position(
    connection: &Connection,
    query: &ObjectListQuery,
    now: i64,
    cursor: Option<&ObjectListCursor>,
    lower: &mut Vec<u8>,
    scratch: &mut Vec<u8>,
) -> Result<Option<SeekPosition>, ObjectError> {
    let mut position = SeekPosition {
        after: false,
        after_generation: None,
        trailing_prefix: false,
    };
    let Some(previous) = cursor else {
        return Ok(Some(position));
    };
    if previous.is_prefix {
        if !prefix_end_into(&previous.name, scratch) {
            return Ok(None);
        }
    } else {
        scratch.clear();
        scratch.extend_from_slice(previous.name.as_bytes());
    }
    if *scratch < *lower {
        return Ok(Some(position));
    }
    std::mem::swap(lower, scratch);
    // A trailing marker emits an item and prefix as separate page entries.
    position.after = !previous.is_prefix && !trailing_marker(&previous.name, query);
    if (query.versions || query.soft_deleted) && !previous.is_prefix {
        position.after_generation = previous.generation;
        if trailing_marker(&previous.name, query) {
            let more: bool = connection
                .prepare_cached(later_generation_sql(ListSource::of(query)))
                .and_then(|mut statement| {
                    let none: Option<Vec<u8>> = None;
                    query_list_row(
                        &mut statement,
                        [
                            &query.bucket,
                            &previous.name,
                            &none,
                            &previous.generation,
                            &now,
                        ],
                        |row| row.get(0),
                    )
                })?;
            if !more {
                position.after_generation = None;
                position.trailing_prefix = true;
            }
        }
    }
    Ok(Some(position))
}

/// The exclusive upper name bound: the prefix's end or the end offset, whichever is lower.
fn list_upper(query: &ObjectListQuery) -> Option<Vec<u8>> {
    let upper = prefix_end(&query.prefix);
    if !query.end_offset.is_empty()
        && upper
            .as_deref()
            .is_none_or(|end| query.end_offset.as_bytes() < end)
    {
        return Some(query.end_offset.as_bytes().to_vec());
    }
    upper
}

pub(in crate::storage::catalog) fn list_page(
    connection: &Connection,
    query: &ObjectListQuery,
    limit: usize,
) -> Result<ObjectListPage, ObjectError> {
    list_page_at(connection, query, limit, 0)
}

fn list_page_at(
    connection: &Connection,
    query: &ObjectListQuery,
    limit: usize,
    now: i64,
) -> Result<ObjectListPage, ObjectError> {
    list_page_within(connection, query, limit, now, LIST_SCAN_BUDGET)
}

/// Unmatched candidates one page may examine before yielding the catalog worker.
const LIST_SCAN_BUDGET: usize = 4096;

pub(super) fn list_page_within(
    connection: &Connection,
    query: &ObjectListQuery,
    limit: usize,
    now: i64,
    budget: usize,
) -> Result<ObjectListPage, ObjectError> {
    let mut page = ObjectListPage {
        items: Vec::new(),
        prefixes: Vec::new(),
        next: None,
        bucket_metageneration: None,
    };
    let mut cursor = query.cursor.clone();
    let mut lower = query
        .prefix
        .as_bytes()
        .max(query.start_offset.as_bytes())
        .to_vec();
    let upper = list_upper(query);
    let bound = if upper.is_some() {
        UpperBound::Bounded
    } else {
        UpperBound::Open
    };
    let mut statements = ListStatements::prepare(connection, ListSource::of(query), bound)?;
    let mut scratch = Vec::new();
    let mut examined = 0;
    loop {
        let Some(position) = seek_position(
            connection,
            query,
            now,
            cursor.as_ref(),
            &mut lower,
            &mut scratch,
        )?
        else {
            break;
        };
        if upper
            .as_deref()
            .is_some_and(|upper| lower.as_slice() >= upper)
        {
            break;
        }
        let statement = if position.after_generation.is_some() {
            &mut statements.versioned
        } else if position.after {
            &mut statements.exclusive
        } else {
            &mut statements.inclusive
        };
        let record = query_list_row(
            statement,
            [
                &query.bucket,
                &lower,
                &upper,
                &position.after_generation,
                &now,
            ],
            read_object,
        )
        .optional()?;
        let Some(StoredObject { record, .. }) = record else {
            break;
        };
        let group = group_prefix(&record.name, query);
        let is_prefix = group.is_some_and(|_| {
            !trailing_marker(&record.name, query)
                || cursor.as_ref().is_some_and(|cursor| {
                    !cursor.is_prefix
                        && cursor.name == record.name
                        && (!(query.versions || query.soft_deleted) || position.trailing_prefix)
                })
        });
        let name = match group {
            Some(group) if is_prefix => group.to_owned(),
            _ => record.name.clone(),
        };
        let matches = query
            .match_glob
            .as_ref()
            .is_none_or(|glob| glob.matches(&name));
        if matches && page.items.len() + page.prefixes.len() == limit {
            page.next = cursor;
            break;
        }
        cursor = Some(ObjectListCursor {
            name: name.clone(),
            is_prefix,
            generation: ((query.versions || query.soft_deleted) && !is_prefix)
                .then_some(record.generation),
        });
        if !matches {
            examined += 1;
            if examined >= budget {
                page.next = cursor;
                break;
            }
            continue;
        }
        if is_prefix {
            page.prefixes.push(name);
        } else {
            page.items.push(record);
        }
    }
    Ok(page)
}

fn group_prefix<'a>(name: &'a str, query: &ObjectListQuery) -> Option<&'a str> {
    if query.delimiter.is_empty() {
        return None;
    }
    let remainder = name.strip_prefix(&query.prefix)?;
    let index = remainder.find(&query.delimiter)?;
    name.get(..query.prefix.len() + index + query.delimiter.len())
}

fn trailing_marker(name: &str, query: &ObjectListQuery) -> bool {
    query.include_trailing_delimiter
        && group_prefix(name, query) == Some(name)
        && name
            .strip_suffix(&query.delimiter)
            .is_some_and(|stem| !stem.ends_with(&query.delimiter))
}

/// The current metageneration of a bucket that `authorized_bucket` has just found.
fn bucket_metageneration(connection: &Connection, bucket: &str) -> Result<i64, ObjectError> {
    sql::optional_scalar(
        connection,
        "SELECT metageneration FROM buckets WHERE name = ?1",
        [bucket],
    )?
    .ok_or(ObjectError::BucketNotFound)
}
