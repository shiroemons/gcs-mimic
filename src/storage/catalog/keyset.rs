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
    /// The largest page, for internal callers that read as much as one call allows.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "sync state listings adopt it next")
    )]
    pub(crate) const LARGEST: Self = match NonZeroUsize::new(MAX) {
        Some(max) => Self(max),
        // A listing returns at least one item per page; `MAX` 0 would be a declaration error.
        None => Self(NonZeroUsize::MIN),
    };

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

/// A keyset position over a non-negative INTEGER ordinal column: rows after it have a larger
/// ordinal, and [`Ordinal::START`] precedes every row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "sync state listings adopt it next")
)]
pub(crate) struct Ordinal(i64);

/// A negative keyset position.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "sync state listings adopt it next")
)]
pub(crate) struct InvalidOrdinal;

impl fmt::Display for InvalidOrdinal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("keyset position is negative")
    }
}

impl std::error::Error for InvalidOrdinal {}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "sync state listings adopt it next")
)]
impl Ordinal {
    pub(crate) const START: Self = Self(0);

    pub(crate) const fn get(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for Ordinal {
    type Error = InvalidOrdinal;

    fn try_from(value: i64) -> Result<Self, InvalidOrdinal> {
        if value < 0 {
            Err(InvalidOrdinal)
        } else {
            Ok(Self(value))
        }
    }
}

impl rusqlite::ToSql for Ordinal {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

/// A stored negative ordinal is catalog corruption (`IntegralValueOutOfRange`, classified corrupt).
impl rusqlite::types::FromSql for Ordinal {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let value = i64::column_result(value)?;
        Self::try_from(value).map_err(|_| rusqlite::types::FromSqlError::OutOfRange(value))
    }
}

#[cfg(test)]
mod tests;
