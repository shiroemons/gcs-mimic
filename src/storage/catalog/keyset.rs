//! Keyset pages, page sizes and positions that catalog listings share.

use crate::bucket::CatalogError;
use std::{fmt, num::NonZeroUsize};

/// A bounded keyset page. Continue after the last item when `has_more` is true.
#[derive(Debug)]
pub(crate) struct Page<T> {
    pub items: Vec<T>,
    pub has_more: bool,
}

/// Reads rows into a page of at most `limit` items whose charges fit `budget`. A row over the
/// remaining budget still fills an empty page, so a scan always advances.
pub(super) fn budgeted_page<T>(
    rows: &mut rusqlite::Rows<'_>,
    limit: usize,
    budget: usize,
    mut charge: impl FnMut(&rusqlite::Row<'_>) -> Result<usize, CatalogError>,
    mut read: impl FnMut(&rusqlite::Row<'_>) -> Result<T, CatalogError>,
) -> Result<Page<T>, CatalogError> {
    let mut items = Vec::new();
    let mut remaining = budget;
    while let Some(row) = rows.next()? {
        // Charge before decoding, so a row that does not fit allocates nothing.
        let charge = charge(row)?;
        if items.len() == limit || (!items.is_empty() && charge > remaining) {
            return Ok(Page {
                has_more: !items.is_empty(),
                items,
            });
        }
        items.push(read(row)?);
        remaining = remaining.saturating_sub(charge);
    }
    Ok(Page {
        items,
        has_more: false,
    })
}

/// Where a keyset scan resumes over a TEXT key column whose stored values are never empty
/// (hex IDs, bucket names). The first page binds `''`, which SQLite orders before every
/// non-empty TEXT value, so one static `key > ?` statement keeps its index range scan on every
/// page (`sql::keep_plans_across_bindings` plans each statement once).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum After<K> {
    #[default]
    Start,
    Key(K),
}

/// Keys stored as non-empty TEXT. `Start` binds `''`, `Key` binds the key's own TEXT. Only these
/// types get a binding: `''` sorts after every INTEGER, so an integer key would skip every row.
macro_rules! text_cursor {
    ($($key:ty),+ $(,)?) => {$(
        impl rusqlite::ToSql for After<$key> {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                match self {
                    Self::Start => Ok(rusqlite::types::ToSqlOutput::Borrowed(
                        rusqlite::types::ValueRef::Text(b""),
                    )),
                    Self::Key(key) => key.to_sql(),
                }
            }
        }
    )+};
}

text_cursor!(crate::storage::BlobId, super::SessionId, String);

/// A page size from 1 to `MAX`, the most the listing that takes it returns at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PageLimit<const MAX: usize>(NonZeroUsize);

/// A requested page size outside the listing's 1..=MAX. Callers report it in their own words.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvalidPageLimit;

impl fmt::Display for InvalidPageLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("page limit out of range")
    }
}

impl std::error::Error for InvalidPageLimit {}

impl<const MAX: usize> PageLimit<MAX> {
    pub(crate) const fn get(self) -> usize {
        self.0.get()
    }
}

impl<const MAX: usize> TryFrom<usize> for PageLimit<MAX> {
    type Error = InvalidPageLimit;

    fn try_from(limit: usize) -> Result<Self, InvalidPageLimit> {
        NonZeroUsize::new(limit)
            .filter(|limit| limit.get() <= MAX)
            .map(Self)
            .ok_or(InvalidPageLimit)
    }
}

/// Binds the page size as an INTEGER (`LIMIT ?`), through rusqlite's fallible `NonZeroUsize`.
impl<const MAX: usize> rusqlite::ToSql for PageLimit<MAX> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(test)]
mod tests;
