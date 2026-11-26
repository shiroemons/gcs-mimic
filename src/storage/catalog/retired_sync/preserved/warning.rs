//! What the operator is told about the files that retired synchronization rows named: once, when
//! the transaction that drops the rows commits, and at every start while a manifest remains or
//! `sync-conflicts/` holds entries. Deleting a manifest after checking it acknowledges it; the
//! copies stay until the operator deletes them. These warnings only read, and a start goes on
//! when they cannot.

use super::{CONFLICTS, Folder, MANIFEST, WORKSPACE, manifest_name};
use crate::diagnostics::{Event, LogLevel, emit};
use crate::error::Report;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use std::{
    collections::BTreeSet,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};

/// The workspace entries a manifest lists in one folder.
#[derive(Serialize)]
pub(in crate::storage::catalog::retired_sync) struct FolderEntries {
    #[serde(flatten)]
    folder: Folder,
    entries: u64,
}

impl FolderEntries {
    /// Counts one entry in `folder` into `tally`, which the listing fills in folder order.
    pub(super) fn count(tally: &mut Vec<Self>, folder: &Folder) {
        if let Some(last) = tally.last_mut().filter(|last| last.folder == *folder) {
            last.entries += 1;
        } else {
            tally.push(Self {
                folder: folder.clone(),
                entries: 1,
            });
        }
    }
}

/// What a written manifest lists.
pub(in crate::storage::catalog::retired_sync) struct Listing {
    pub(super) manifest: PathBuf,
    /// `sync-conflicts/` in the data directory.
    pub(super) conflicts: PathBuf,
    pub(super) conflict_copies: u64,
    pub(super) workspace_entries: Vec<FolderEntries>,
}

#[derive(Serialize)]
struct Listed<'a> {
    message: String,
    manifest: String,
    conflict_copies: u64,
    workspace_entries: &'a [FolderEntries],
}

impl Listing {
    /// Warns, once the rows are gone, that the manifest is the only record of the files they
    /// named.
    pub(in crate::storage::catalog::retired_sync) fn warn(&self) {
        emit(LogLevel::Warn, Event::SyncRetiredFilesListed, self.listed());
    }

    /// The fields of the warning [`Self::warn`] gives.
    #[cfg(test)]
    pub(in crate::storage::catalog::retired_sync) fn warning(
        &self,
    ) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self.listed())
    }

    fn listed(&self) -> Listed<'_> {
        let manifest = self.manifest.display().to_string();
        let copies = plural(self.conflict_copies, "conflict copy", "conflict copies");
        let entries = self
            .workspace_entries
            .iter()
            .map(|folder| folder.entries)
            .sum();
        let workspace = if entries == 0 {
            String::new()
        } else {
            let folders = self
                .workspace_entries
                .iter()
                .map(|folder| format!("{} in {}", folder.entries, folder.folder.folder))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                ", and {} that may remain in the private workspace ({WORKSPACE}/) of formerly \
                 synchronized folders: {folders}",
                plural(entries, "workspace entry", "workspace entries")
            )
        };
        let message = format!(
            "Catalog schema 58 removed the records of directory synchronization. {manifest} lists \
             the files they named: {copies} in {}{workspace}. gcs-mimic never deletes these \
             files. Inspect them and delete what you no longer need, then delete the manifest to \
             acknowledge it.",
            self.conflicts.display()
        );
        Listed {
            message,
            manifest,
            conflict_copies: self.conflict_copies,
            workspace_entries: &self.workspace_entries,
        }
    }
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What a start finds of the files the retired rows named.
#[derive(Serialize)]
struct Remaining {
    message: String,
    /// `sync-conflicts/` in the data directory.
    directory: String,
    /// Its entries, without the files an operating system or editor adds.
    entries: u64,
    /// The entries a manifest lists as conflict copies.
    listed: u64,
    manifests: Vec<String>,
    workspace_entries: u64,
}

impl Remaining {
    fn new(directory: &Path, (entries, listed): (u64, u64), found: &Found) -> Self {
        let directory = directory.display().to_string();
        let manifests: Vec<_> = found
            .manifests
            .iter()
            .map(|manifest| manifest.display().to_string())
            .collect();
        let named = manifests.join(", ");
        let held = plural(entries, "entry", "entries");
        let copies = if manifests.is_empty() {
            format!("{held} in {directory}, none of them listed in a manifest")
        } else {
            format!("{held} in {directory}, {listed} of them listed in {named}")
        };
        let workspace = if found.workspace_entries == 0 {
            String::new()
        } else {
            let verb = if manifests.len() == 1 {
                "lists"
            } else {
                "list"
            };
            format!(
                " {named} also {verb} {} that may remain in the private workspace ({WORKSPACE}/) \
                 of formerly synchronized folders.",
                plural(
                    found.workspace_entries,
                    "workspace entry",
                    "workspace entries"
                )
            )
        };
        let message = format!(
            "The removed directory synchronization left files that may hold edits: \
             {copies}.{workspace} gcs-mimic never deletes these files. Inspect them and delete \
             what you no longer need; deleting a manifest after checking it acknowledges it, and \
             the copies stay until you delete them."
        );
        Self {
            message,
            directory,
            entries,
            listed,
            manifests,
            workspace_entries: found.workspace_entries,
        }
    }
}

/// The manifests in a data directory and what they list.
#[derive(Default)]
struct Found {
    manifests: Vec<PathBuf>,
    /// The names in `sync-conflicts/` of the copies they list.
    copies: BTreeSet<String>,
    workspace_entries: u64,
}

/// What the start-up warning reads from a manifest. A manifest someone edited may lack a list.
#[derive(Deserialize)]
struct Summary {
    #[serde(default)]
    conflict_copies: Vec<ListedCopy>,
    #[serde(default)]
    workspace_entries: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
struct ListedCopy {
    path: String,
}

/// Warns at every start while a manifest remains in `data_dir` or `sync-conflicts/` holds
/// entries, so the files stay visible until the operator deletes them. A directory or manifest
/// the check cannot read gets a warning of its own, and the start goes on.
pub(in crate::storage::catalog) fn warn_retired_files(data_dir: &Path) {
    let manifests = manifests(data_dir).unwrap_or_else(|error| {
        unreadable(data_dir, &error);
        Vec::new()
    });
    let mut found = Found::default();
    for manifest in manifests {
        match summary(&manifest) {
            Ok(summary) => found.add(summary),
            Err(error) => unreadable(&manifest, &error),
        }
        found.manifests.push(manifest);
    }
    let directory = data_dir.join(CONFLICTS);
    let counted = count(&directory, &found.copies).unwrap_or_else(|error| {
        unreadable(&directory, &error);
        (0, 0)
    });
    if found.manifests.is_empty() && counted.0 == 0 {
        return;
    }
    emit(
        LogLevel::Warn,
        Event::SyncRetiredFiles,
        Remaining::new(&directory, counted, &found),
    );
}

impl Found {
    fn add(&mut self, summary: Summary) {
        let prefix = format!("{CONFLICTS}/");
        self.copies.extend(
            summary
                .conflict_copies
                .into_iter()
                .filter_map(|copy| copy.path.strip_prefix(&prefix).map(str::to_owned)),
        );
        self.workspace_entries +=
            u64::try_from(summary.workspace_entries.len()).unwrap_or(u64::MAX);
    }
}

/// The number of the manifest called `name`, if that is a manifest's name.
fn manifest_number(name: &str) -> Option<u32> {
    if name == MANIFEST {
        return Some(0);
    }
    name.strip_prefix("sync-retired.")?
        .strip_suffix(".json")?
        .parse()
        .ok()
        .filter(|&number| manifest_name(number) == name)
}

/// The manifests in `data_dir`, in the order they were written.
fn manifests(data_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut numbered = Vec::new();
    for entry in fs::read_dir(data_dir)? {
        let name = entry?.file_name();
        if let Some(number) = name.to_str().and_then(manifest_number) {
            numbered.push((number, data_dir.join(name)));
        }
    }
    numbered.sort_unstable();
    Ok(numbered.into_iter().map(|(_, path)| path).collect())
}

fn summary(manifest: &Path) -> Result<Summary, serde_json::Error> {
    let file = fs::File::open(manifest).map_err(serde_json::Error::io)?;
    serde_json::from_reader(io::BufReader::new(file))
}

/// The entries in `directory`, not counting the files an operating system or editor adds, and
/// how many of them `listed` names.
fn count(directory: &Path, listed: &BTreeSet<String>) -> io::Result<(u64, u64)> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(error),
    };
    let (mut held, mut named) = (0, 0);
    for entry in entries {
        let name = entry?.file_name();
        let name = name.to_str();
        if name.is_some_and(crate::source_tree::is_ignored_regular_file) {
            continue;
        }
        held += 1;
        if name.is_some_and(|name| listed.contains(name)) {
            named += 1;
        }
    }
    Ok((held, named))
}

/// A path a warning names, with the error that stopped the work on it.
#[derive(Serialize)]
struct Unfinished {
    message: String,
    path: String,
    error: String,
}

/// Warns that the start could not check `path`, and goes on. The error is printed with its
/// causes, such as the I/O error under a manifest that could not be parsed.
pub(super) fn unreadable(path: &Path, error: &(dyn Error + 'static)) {
    let path = path.display().to_string();
    let error = Report::chain(error).to_string();
    emit(
        LogLevel::Warn,
        Event::SyncRetiredFilesUnchecked,
        Unfinished {
            message: format!(
                "could not check {path} for files the removed directory synchronization left: \
                 {error}; the start goes on"
            ),
            path,
            error,
        },
    );
}

/// Warns that the staged manifest at `path`, which an attempt left, could not be removed yet. A
/// later attempt removes it; this one stages under a name of its own.
pub(super) fn staged_kept(path: &Path, error: &(dyn Error + 'static)) {
    let path = path.display().to_string();
    let error = Report::chain(error).to_string();
    emit(
        LogLevel::Warn,
        Event::SyncRetiredStagedKept,
        Unfinished {
            message: format!(
                "could not remove {path}, a staged retired synchronization manifest: {error}; \
                 a later start removes it"
            ),
            path,
            error,
        },
    );
}
