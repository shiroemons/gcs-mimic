//! The working-tree entries a scan could not take in, by path. They hold back the baseline and
//! deletion detection for the whole root, so a count alone left a user searching the tree for
//! what to fix; the list names them, up to a bound, and is rebuilt by every complete scan.

use std::path::{Path, PathBuf};

/// Why a scan could not take a working-tree entry in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum EntryProblem {
    /// Its name maps to no object name, or its top-level directory to no bucket name.
    UnmappableName,
    /// A symlink or a special file.
    UnsafeEntry,
    /// An entry that could not be read, such as a directory without read permission.
    Unreadable,
    /// A file that is also the parent directory of an object.
    PathCollision,
    /// The directory of a bucket deleted through the API, which still holds entries.
    DeletedBucket,
}

/// One entry a scan could not take in.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct UnrepresentableEntry {
    /// The entry below the sync root.
    pub(crate) path: PathBuf,
    pub(crate) problem: EntryProblem,
}

/// The first [`EntryReport::LIMIT`] entries one scan could not take in, in scan order, and how
/// many there were. A tree can hold any number of them, so only the count is unbounded.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct EntryReport {
    entries: Vec<UnrepresentableEntry>,
    total: u64,
}

impl EntryReport {
    /// Entries kept by path; the rest are only counted.
    pub(crate) const LIMIT: usize = 256;

    pub(crate) fn record(&mut self, path: &Path, problem: EntryProblem) {
        self.total = self.total.saturating_add(1);
        if self.entries.len() < Self::LIMIT {
            self.entries.push(UnrepresentableEntry {
                path: path.to_owned(),
                problem,
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> &[UnrepresentableEntry] {
        &self.entries
    }

    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past the bound the report keeps counting without growing, so a tree full of unsafe entries
    /// cannot make the mirror's memory grow with it.
    #[test]
    fn the_report_keeps_the_first_entries_and_counts_the_rest() {
        let mut report = EntryReport::default();
        for index in 0..EntryReport::LIMIT + 10 {
            report.record(
                Path::new(&format!("bucket/{index}")),
                EntryProblem::UnsafeEntry,
            );
        }
        assert_eq!(
            report.total(),
            u64::try_from(EntryReport::LIMIT + 10).unwrap()
        );
        assert_eq!(report.entries().len(), EntryReport::LIMIT);
        assert_eq!(report.entries()[0].path, Path::new("bucket/0"));
        assert_eq!(
            serde_json::to_value(&report.entries()[0]).unwrap(),
            serde_json::json!({"path":"bucket/0","problem":"unsafe-entry"})
        );
    }
}
