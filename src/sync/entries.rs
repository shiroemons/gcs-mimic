//! What a scan makes of each working-tree entry, and the entries it could not take in, by path.
//! Those hold back the baseline and deletion detection for the whole root, so a count alone left
//! a user searching the tree for what to fix; the list names them, up to a bound, and is rebuilt
//! by every complete scan.

use super::{control::Correction, mapping, naming::RootNaming, tree::TreeEntry};
use crate::{
    diagnostics::{self, LogLevel},
    fsutil::FileIdentity,
    storage::admin::{BucketName, ObjectName},
};
use std::{
    borrow::Cow,
    collections::HashSet,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

/// What a scan makes of one working-tree entry. Ingestion and the reconciliation's count of
/// external files both take it from [`classify`] and only count it, so they cannot disagree about
/// an entry: a root-level `.DS_Store` once held back ingestion's deletion detection while the
/// reconciliation counted it as ignored.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EntryClass {
    /// A directory directly in the root, which holds one bucket's objects.
    Bucket(BucketName),
    /// A directory below a bucket's, which the scan descends into.
    Directory,
    /// A regular file whose path names an object.
    File(MappedFile),
    /// A regular file no object is made from, which holds nothing back.
    Skipped(Skip),
    /// An entry that could not be read this time.
    Unreadable(io::ErrorKind),
    /// An entry no object can take, where a missing tracked file may have gone.
    Unrepresentable(EntryProblem),
}

/// A regular file and the object its path names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MappedFile {
    pub(crate) bucket: BucketName,
    pub(crate) name: ObjectName,
    /// What the scan observed of the file.
    pub(crate) identity: FileIdentity,
}

/// Why a scan passes over a regular file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    /// An operating-system or editor file, which no object is made from wherever it lies.
    IgnoredName,
    /// A file directly in the root, where only bucket directories belong, such as a `README.md`
    /// or a `.gitkeep`. Its path names no object, and a synchronized file moved there has left
    /// the buckets as one moved out of the root has, so it holds nothing back.
    RootFile,
}

/// Classifies one entry of a tree scan. Ignored names are checked before name mapping and only
/// for regular files: a directory named like an editor file is traversed, and a symlink so named
/// is as unsafe as any other.
pub(crate) fn classify(entry: &TreeEntry, naming: RootNaming) -> EntryClass {
    match (entry.error, entry.directory, entry.identity) {
        (Some(kind), ..) => EntryClass::Unreadable(kind),
        (None, true, _) => directory(&entry.path),
        (None, false, None) => EntryClass::Unrepresentable(EntryProblem::UnsafeEntry),
        (None, false, Some(identity)) => regular_file(&entry.path, identity, naming),
    }
}

fn directory(path: &Path) -> EntryClass {
    if path.components().count() != 1 {
        return EntryClass::Directory;
    }
    match path.to_str().map(BucketName::new) {
        Some(Ok(bucket)) => EntryClass::Bucket(bucket),
        Some(Err(_)) | None => EntryClass::Unrepresentable(EntryProblem::UnmappableName),
    }
}

fn regular_file(path: &Path, identity: FileIdentity, naming: RootNaming) -> EntryClass {
    if path
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(crate::source_tree::is_ignored_regular_file)
    {
        return EntryClass::Skipped(Skip::IgnoredName);
    }
    if path.components().count() == 1 {
        return EntryClass::Skipped(Skip::RootFile);
    }
    match mapping::path_to_name(path, naming) {
        Ok((bucket, name)) => EntryClass::File(MappedFile {
            bucket,
            name,
            identity,
        }),
        // The mapping repeats the check of the file's own name made above.
        Err(mapping::Reason::Ignored) => EntryClass::Skipped(Skip::IgnoredName),
        Err(
            mapping::Reason::InvalidName
            | mapping::Reason::HostRestriction
            | mapping::Reason::Reserved
            | mapping::Reason::Collision
            | mapping::Reason::UnsafeEntry
            | mapping::Reason::DirectoryOccupied,
        ) => EntryClass::Unrepresentable(EntryProblem::UnmappableName),
    }
}

/// Warns about a file a scan skipped directly in the root, as seeding warns about one in its
/// root.
pub(crate) fn warn_root_file(path: &Path) {
    diagnostics::emit(
        LogLevel::Warn,
        "sync_skipped",
        serde_json::json!({
            "path":path_text(path),
            "reason":"top-level entries must be bucket directories",
            "remedy":"Move the file into a bucket directory to make it an object; where it is, it is never synchronized and holds nothing back",
        }),
    );
}

/// Why a scan could not take a working-tree entry in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
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

impl EntryProblem {
    /// The event that names an entry with this problem in the log. The unsafe entry keeps the
    /// name the rescan advice refers to, and the others the names they were logged under before
    /// each entry was named once.
    fn event(self) -> &'static str {
        match self {
            Self::UnmappableName => "sync_unmappable_entry",
            Self::UnsafeEntry => "sync_unsafe_entry",
            Self::Unreadable => "sync_unreadable_entry",
            Self::PathCollision => "sync_path_collision",
            Self::DeletedBucket => "sync_bucket_missing",
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::UnmappableName => {
                "The entry's name maps to no object name, or the top-level directory's to no bucket name"
            }
            Self::UnsafeEntry => {
                "Symbolic links and special files are never followed or synchronized"
            }
            Self::Unreadable => "The entry could not be read",
            Self::PathCollision => "The file is also the parent directory of an object",
            Self::DeletedBucket => "The previously synchronized bucket was deleted through the API",
        }
    }

    /// What the user corrects, which names no command; [`Correction::Entry`] adds the wait that
    /// takes the correction in. Until a scan no longer finds the entry, it holds back every
    /// inferred deletion of the root.
    pub(crate) const fn correction(self) -> &'static str {
        match self {
            Self::UnmappableName => {
                "Rename the entry to a name the API can hold, or move it out of the directory"
            }
            Self::UnsafeEntry => {
                "Replace the symlink or special file with a regular file; for example, copy its intended bytes into a new regular file"
            }
            Self::Unreadable => {
                "Restore read and search permissions on the entry and its parents (for example, chmod u+rx DIRECTORY), or move it out of the directory"
            }
            Self::PathCollision => {
                "Rename the external file or the API object so a file is not also a parent directory; for example, use files/object instead of files"
            }
            Self::DeletedBucket => {
                "Remove or move this directory, or recreate the bucket through the API (for example, create the same bucket name in the intended project)"
            }
        }
    }
}

/// A working-tree path as logs, remedies, an entry report and a wait name it. An entry is often
/// unrepresentable because its name is not UTF-8, which a JSON string cannot hold as it is: the
/// text shows U+FFFD for each invalid sequence, where serializing the path itself would fail the
/// answer, or panic inside `json!`, because of the very entry it reports.
pub(crate) fn path_text(path: &Path) -> Cow<'_, str> {
    path.to_string_lossy()
}

/// One entry a scan could not take in. The status lists it by its path and the reason no object
/// can take it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UnrepresentableEntry {
    /// The entry below the sync root.
    pub(crate) path: PathBuf,
    pub(crate) problem: EntryProblem,
}

/// An entry is listed by the text of its path, which cannot fail to serialize.
impl serde::Serialize for UnrepresentableEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Listed<'a> {
            path: Cow<'a, str>,
            reason: EntryProblem,
        }
        Listed {
            path: path_text(&self.path),
            reason: self.problem,
        }
        .serialize(serializer)
    }
}

/// Warns about an entry no object can take, once when a complete scan first finds it.
pub(crate) fn warn_unrepresentable(entry: &UnrepresentableEntry) {
    diagnostics::emit(
        LogLevel::Warn,
        entry.problem.event(),
        serde_json::json!({
            "path":path_text(&entry.path),
            "reason":entry.problem.reason(),
            "remedy":super::advice::logged(&Correction::Entry(entry.problem).remedy()),
        }),
    );
}

/// The first [`EntryReport::LIMIT`] entries one scan could not take in, in scan order, and how
/// many there were. A tree can hold any number of them, so only the count is unbounded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EntryReport {
    entries: Vec<UnrepresentableEntry>,
    total: u64,
    /// The entries among `total` that could not be read, which hold back the baseline and
    /// deletions as unreadable rather than as unrepresentable.
    unreadable: u64,
    /// The first [`EntryReport::LIMIT`] files the scan skipped directly in the root. Each is
    /// warned about when a scan first finds it, not on every scan while it stays.
    root_files: Vec<PathBuf>,
}

/// The status lists every named entry with the count of those past the bound, which only the
/// total accounts for.
impl serde::Serialize for EntryReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.listing(Self::LIMIT).serialize(serializer)
    }
}

/// Entries as an entry report and a wait list them: the first ones by path and reason, and how
/// many the total counts past them.
#[derive(Debug, serde::Serialize)]
pub(crate) struct EntryListing<'a> {
    total: u64,
    entries: &'a [UnrepresentableEntry],
    omitted: u64,
}

impl EntryListing<'_> {
    /// Whether the scan found no entry at all, named or counted.
    pub(crate) const fn is_empty(&self) -> bool {
        self.total == 0
    }
}

impl EntryReport {
    /// Entries kept by path; the rest are only counted.
    pub(crate) const LIMIT: usize = 256;

    pub(crate) fn record(&mut self, path: &Path, problem: EntryProblem) {
        self.total = self.total.saturating_add(1);
        match problem {
            EntryProblem::Unreadable => self.unreadable = self.unreadable.saturating_add(1),
            EntryProblem::UnmappableName
            | EntryProblem::UnsafeEntry
            | EntryProblem::PathCollision
            | EntryProblem::DeletedBucket => {}
        }
        if self.entries.len() < Self::LIMIT {
            self.entries.push(UnrepresentableEntry {
                path: path.to_owned(),
                problem,
            });
        }
    }

    pub(crate) fn record_root_file(&mut self, path: &Path) {
        if self.root_files.len() < Self::LIMIT {
            self.root_files.push(path.to_owned());
        }
    }

    /// The root files this report holds that `previous` did not.
    pub(crate) fn root_files_since(&self, previous: &Self) -> Vec<PathBuf> {
        let previous: HashSet<&PathBuf> = previous.root_files.iter().collect();
        self.root_files
            .iter()
            .filter(|path| !previous.contains(path))
            .cloned()
            .collect()
    }

    /// Adds the entries of another report of the same pass, within the same bound.
    pub(crate) fn extend(&mut self, other: &Self) {
        let room = Self::LIMIT.saturating_sub(self.entries.len());
        self.entries
            .extend(other.entries.iter().take(room).cloned());
        self.total = self.total.saturating_add(other.total);
        self.unreadable = self.unreadable.saturating_add(other.unreadable);
    }

    /// The entries this report names that `previous` did not, so that each is warned about when
    /// a scan first finds it, not on every scan while it stays.
    pub(crate) fn entries_since(&self, previous: &Self) -> Vec<UnrepresentableEntry> {
        let previous: HashSet<&UnrepresentableEntry> = previous.entries.iter().collect();
        self.entries
            .iter()
            .filter(|entry| !previous.contains(entry))
            .cloned()
            .collect()
    }

    /// The entries named by path, in scan order.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> &[UnrepresentableEntry] {
        &self.entries
    }

    /// The first `count` named entries, in scan order, with the count of the rest.
    pub(crate) fn listing(&self, count: usize) -> EntryListing<'_> {
        let entries = self.entries.get(..count).unwrap_or(&self.entries);
        let named = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        EntryListing {
            total: self.total,
            entries,
            omitted: self.total.saturating_sub(named),
        }
    }

    /// How many entries the scan could not take in or could not read, named or only counted.
    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    /// The entries no object can take, which hold back the baseline and deletions as
    /// unrepresentable.
    pub(crate) fn unrepresentable(&self) -> u64 {
        self.total.saturating_sub(self.unreadable)
    }

    /// The entries the scan could not read for a reason waiting does not clear, which hold back
    /// the baseline and deletions as unreadable.
    pub(crate) fn unreadable(&self) -> u64 {
        self.unreadable
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
        let listed = serde_json::to_value(&report).unwrap();
        assert_eq!(listed["total"], EntryReport::LIMIT + 10);
        assert_eq!(listed["omitted"], 10);
        assert_eq!(
            listed["entries"][0],
            serde_json::json!({"path":"bucket/0","reason":"unsafe-entry"})
        );
        assert_eq!(
            listed["entries"].as_array().unwrap().len(),
            EntryReport::LIMIT
        );
        // A shorter listing names fewer entries and counts the rest as omitted.
        let first = serde_json::to_value(report.listing(2)).unwrap();
        assert_eq!(first["total"], EntryReport::LIMIT + 10);
        assert_eq!(first["omitted"], EntryReport::LIMIT + 8);
        assert_eq!(first["entries"].as_array().unwrap().len(), 2);
        assert_eq!(
            serde_json::to_value(report.listing(EntryReport::LIMIT + 1)).unwrap(),
            listed
        );
    }

    /// An entry whose name is not UTF-8, which is often why no object can take it, is listed and
    /// logged under a lossy path instead of failing the status or panicking the scan.
    #[cfg(unix)]
    #[test]
    fn an_entry_that_is_not_utf8_is_listed_and_logged_lossily() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(OsStr::from_bytes(b"bucket/bad\xff"));
        assert_eq!(path_text(path), "bucket/bad\u{fffd}");
        let mut report = EntryReport::default();
        report.record(path, EntryProblem::UnmappableName);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["entries"][0],
            serde_json::json!({"path":"bucket/bad\u{fffd}","reason":"unmappable-name"})
        );
        warn_unrepresentable(&report.entries()[0]);
    }

    /// A report warns only about the entries that the previous complete scan did not name, so an
    /// entry that stays is warned about once and one that comes back is warned about again.
    #[test]
    fn only_entries_the_previous_scan_did_not_name_are_new() {
        let entry = |path: &str, problem| {
            let mut report = EntryReport::default();
            report.record(Path::new(path), problem);
            report
        };
        let link = entry("bucket/link", EntryProblem::UnsafeEntry);
        let mut both = link.clone();
        both.record(Path::new("bad name"), EntryProblem::UnmappableName);
        assert_eq!(link.entries_since(&EntryReport::default()), link.entries());
        assert!(link.entries_since(&both).is_empty());
        assert_eq!(both.entries_since(&link), &both.entries()[1..]);
        let replaced = entry("bucket/link", EntryProblem::Unreadable);
        assert_eq!(replaced.entries_since(&link), replaced.entries());
    }

    /// What the scan could not read decides before what the entry is, and only a regular file
    /// is ignored or skipped by its name or place.
    #[cfg(unix)]
    #[test]
    fn an_entry_is_unreadable_unsafe_or_skipped_before_its_name_is_mapped() {
        let identity = FileIdentity {
            device: 1,
            inode: 1,
            size: 0,
            modified_ns: 0,
            changed_ns: 0,
        };
        let entry = |path: &str, directory: bool, identity: Option<FileIdentity>| TreeEntry {
            path: path.into(),
            identity,
            directory,
            error: None,
        };
        let unreadable = TreeEntry {
            error: Some(io::ErrorKind::PermissionDenied),
            ..entry("bucket", true, None)
        };
        let cases = [
            (
                unreadable,
                EntryClass::Unreadable(io::ErrorKind::PermissionDenied),
            ),
            (
                entry(".DS_Store", false, None),
                EntryClass::Unrepresentable(EntryProblem::UnsafeEntry),
            ),
            (
                entry("bucket/x.swp", false, None),
                EntryClass::Unrepresentable(EntryProblem::UnsafeEntry),
            ),
            (entry("bucket/x.swp", true, None), EntryClass::Directory),
            (
                entry(".DS_Store", false, Some(identity)),
                EntryClass::Skipped(Skip::IgnoredName),
            ),
            (
                entry("README.md", false, Some(identity)),
                EntryClass::Skipped(Skip::RootFile),
            ),
            (
                entry("bucket", true, None),
                EntryClass::Bucket(BucketName::new("bucket").unwrap()),
            ),
            (
                entry("Bucket", true, None),
                EntryClass::Unrepresentable(EntryProblem::UnmappableName),
            ),
            (
                entry("bucket/name", false, Some(identity)),
                EntryClass::File(MappedFile {
                    bucket: BucketName::new("bucket").unwrap(),
                    name: ObjectName::new("name").unwrap(),
                    identity,
                }),
            ),
            (
                entry("bucket/a\\b", false, Some(identity)),
                EntryClass::Unrepresentable(EntryProblem::UnmappableName),
            ),
        ];
        for (entry, expected) in cases {
            assert_eq!(
                classify(&entry, RootNaming::posix()),
                expected,
                "{}",
                entry.path.display()
            );
        }
    }
}
