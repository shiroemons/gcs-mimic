//! Reading the live object, the tracked path and the tree, and deciding what an export does.

use super::alias::Freeze;
use super::{
    Content, LiveObject, LivePlan, MissingFile, Occasion, reconcile::hierarchy_collision,
    retry::file_job_retry,
};
use crate::storage::sync_state::findings::alias::Holding;
#[cfg(test)]
use crate::sync::mirror::export_checkpoint;
use crate::{
    diagnostics::{self, LogLevel},
    fsutil,
    object::ObjectRecord,
    storage::{
        admin::{Admin, AdminError, Generation},
        sync_state::{SyncBinding, SyncRecord, root::RootError},
    },
    sync::{
        SyncError,
        error::{superseded, unless_superseded},
        mapping::{self, PathMapping, Reason},
        naming::RootNaming,
        report::remedy::Correction,
        tree::{ParentError, ParentOpen, parent, structural_entry_error},
    },
};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

impl LiveObject<'_> {
    /// Reads current state once and decides what to export; `None` means nothing to do now,
    /// with any finding or deferral already counted in `summary`.
    pub(in crate::sync) async fn plan(&mut self) -> Result<Option<LivePlan>, SyncError> {
        let (catalog, bucket, name) = (self.catalog, self.bucket, self.name);
        let admin = self.root.sync_admin();
        #[cfg(test)]
        export_checkpoint("before-binding").await;
        let binding = match self.root.sync_binding(bucket).await {
            Ok(binding) => binding,
            Err(AdminError::NotFound) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        #[cfg(test)]
        export_checkpoint("after-binding").await;
        let record = match admin.get_object(bucket, name, None).await {
            Ok(record) => Some(record),
            Err(AdminError::NotFound) => None,
            Err(error) => return Err(error.into()),
        };
        let mapped = match mapping::name_to_path(bucket, name, self.root.naming()?) {
            PathMapping::File(path) if self.root.tree_limits().admits_file(&path) => Ok(path),
            // Exporting a level the scans refuse would stop the whole root, so this object alone
            // stays out of the tree.
            PathMapping::File(_) => Err(Reason::HostRestriction),
            PathMapping::Unrepresentable(reason) => Err(reason),
        };
        let path = match mapped {
            Ok(path) => path,
            Err(reason) => {
                match record {
                    Some(record) => self.finding(&record, reason).await?,
                    None => {
                        unless_superseded(
                            self.root
                                .remove_deleted_finding(binding, name.clone())
                                .await,
                        )?;
                    }
                }
                return Ok(None);
            }
        };
        let tracked = match catalog
            .sync_state()
            .get_path(
                binding.clone(),
                path.to_str()
                    .ok_or(SyncError::Invariant("invalid export path"))?
                    .to_owned(),
            )
            .await
        {
            Ok(tracked) => tracked,
            Err(AdminError::Precondition | AdminError::NotFound) => {
                self.defer();
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let Some(record) = record else {
            return self.plan_removal(binding, path, tracked).await;
        };
        let Some(content) = self
            .check_target(&admin, &binding, &record, path, tracked.as_ref())
            .await?
        else {
            return Ok(None);
        };
        let generation = Generation::new(record.generation)?;
        #[cfg(test)]
        export_checkpoint("before-open").await;
        let opened = match content {
            Content::Identical => admin
                .get_object(bucket, name, Some(generation))
                .await
                .map(|_| None),
            Content::Changed => admin
                .open_object(bucket, name, Some(generation))
                .await
                .map(|(_, reader)| Some(reader)),
        };
        let source = match opened {
            Ok(Some(reader)) => Some(reader.into_file()?),
            Ok(None) => None,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                self.defer();
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Some(LivePlan {
            binding,
            tracked,
            generation,
            source,
            content,
        }))
    }

    /// Decides what the deletion of this object through the API does to its file at `path`,
    /// which the tree tracks as `tracked`, if at all.
    async fn plan_removal(
        &mut self,
        binding: SyncBinding,
        path: PathBuf,
        tracked: Option<SyncRecord>,
    ) -> Result<Option<LivePlan>, SyncError> {
        let name = self.name;
        let Some(tracked) = tracked else {
            // Nothing to remove from the tree, but a finding that kept the deleted object out
            // of it would still count it.
            unless_superseded(
                self.root
                    .remove_deleted_finding(binding, name.clone())
                    .await,
            )?;
            return Ok(None);
        };
        // The file may stand for other objects of an alias, or hold an edit made for the one
        // left, which ingestion takes in before the file goes.
        match self
            .root
            .alias_holding(binding.clone(), name.clone(), tracked.path.clone())
            .await
        {
            Ok(Holding::Alone) => {}
            Ok(Holding::Shared | Holding::Edited) => return Ok(None),
            Err(error) if superseded(&error) => {
                self.defer();
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
        if self.replaced(path).await? {
            return Ok(None);
        }
        Ok(Some(LivePlan {
            binding,
            generation: tracked.generation,
            tracked: Some(tracked),
            source: None,
            content: Content::Changed,
        }))
    }

    /// Whether a directory or special file took the place of the tracked file at `path`, which
    /// then stops that path alone: removing or replacing what is there now would destroy it.
    async fn replaced(&self, path: PathBuf) -> Result<bool, SyncError> {
        let path = path
            .into_os_string()
            .into_string()
            .map_err(|_| SyncError::Invariant("invalid export path"))?;
        match crate::sync::deletion::presence(self.root, &self.root.lookup()?, path).await? {
            crate::sync::deletion::Presence::Unobservable(reason) if reason.stops_path() => {
                crate::sync::deletion::record_replaced(self.root, self.bucket, self.name).await?;
                Ok(true)
            }
            crate::sync::deletion::Presence::Absent(_)
            | crate::sync::deletion::Presence::Present
            | crate::sync::deletion::Presence::Unobservable(_) => Ok(false),
        }
    }

    /// Whether the tracked file of `record`, missing from the tree, is left to the deletion
    /// inventory instead of being written back: only where the inventory looks at it, so an
    /// object it skips, such as one with a finding, is written back rather than left to nobody.
    async fn missing_is_deleted(
        &self,
        record: &ObjectRecord,
        tracked: Option<&SyncRecord>,
    ) -> Result<bool, SyncError> {
        let Some(tracked) = tracked else {
            return Ok(false);
        };
        Ok(self.missing == MissingFile::Inventory
            && self.root.current_baseline().await?
                == crate::storage::sync_state::root::Baseline::Complete
            && self
                .root
                .inventories(self.bucket, tracked, record.generation)
                .await?)
    }

    /// Compares the live object with its working-tree path; `None` means a finding, a path stop
    /// or a deferral was recorded, or a deletion left to the inventory, instead of an export.
    async fn check_target(
        &mut self,
        admin: &Admin,
        binding: &SyncBinding,
        record: &ObjectRecord,
        path: PathBuf,
        tracked: Option<&SyncRecord>,
    ) -> Result<Option<Content>, SyncError> {
        let collision = match hierarchy_collision(admin, self.bucket, self.name).await {
            Ok(collision) => collision,
            Err(AdminError::Precondition | AdminError::NotFound) => {
                self.defer();
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if collision {
            self.finding(record, Reason::Collision).await?;
            return Ok(None);
        }
        // A frozen file is not read at all, whatever it shows: only an API change ends an alias.
        let released = match self.hold_alias(binding, record).await? {
            Freeze::NotAliased => false,
            Freeze::Held => return Ok(None),
            Freeze::Released => true,
        };
        let Some(target) = self.read_target(record, path).await? else {
            return Ok(None);
        };
        let content = match target {
            // A tracked file now listed under another spelling was renamed in the tree, which
            // the deletion inventory takes as a rename. A released alias whose file is still
            // listed under the other spelling waits for the deleted object that holds it.
            Target::Alias(_) if tracked.is_some() || released => return Ok(None),
            Target::Alias(spelling) => {
                self.alias(binding, record, spelling).await?;
                return Ok(None);
            }
            Target::Directory | Target::Replaced(_) if tracked.is_some() => {
                crate::sync::deletion::record_replaced(self.root, self.bucket, self.name).await?;
                return Ok(None);
            }
            Target::Finding(reason) | Target::Replaced(reason) => {
                self.finding(record, reason).await?;
                return Ok(None);
            }
            Target::Identical => TargetContent::Identical,
            Target::Absent => TargetContent::Absent,
            Target::Conflicting | Target::Directory => TargetContent::Changed,
        };
        // The tree already tracks this generation, so a file that differs holds an edit, which
        // ingestion takes in: neither the end of an alias that kept it out nor another look at
        // a finding exports over it.
        if (released || self.occasion == Occasion::Finding)
            && content == TargetContent::Changed
            && tracked.is_some_and(|tracked| tracked.generation.get() == record.generation)
        {
            return Ok(None);
        }
        match content {
            TargetContent::Identical => Ok(Some(Content::Identical)),
            // A file that went while it stood for several objects tells nothing about this one,
            // which gets its file back like any object with a finding.
            TargetContent::Absent
                if !released && self.missing_is_deleted(record, tracked).await? =>
            {
                Ok(None)
            }
            TargetContent::Absent | TargetContent::Changed => Ok(Some(Content::Changed)),
        }
    }

    /// Reads what the tree holds at `path` for `record`. A change retries a failed read and then
    /// fails the pass; a finding observed again leaves its row for a later pass, reporting an
    /// I/O failure, since the scan reports the entry and the object's own changes still come
    /// through the journal.
    async fn read_target(
        &mut self,
        record: &ObjectRecord,
        path: PathBuf,
    ) -> Result<Option<Target>, SyncError> {
        let observed = record.clone();
        let naming = self.root.naming()?;
        let lookup = self.root.lookup()?;
        match self.occasion {
            Occasion::Change => {
                let target = file_job_retry(self.root, move |root| {
                    match inspect(root, naming, &lookup, &path, &observed) {
                        Ok(target) => Ok(Some(target)),
                        // The spelling of the entry changed while it was read, as when the
                        // frozen file of an alias is renamed: the next pass reads it again.
                        Err(AdminError::Precondition) => Ok(None),
                        Err(error) => Err(error),
                    }
                })
                .await?;
                if target.is_none() {
                    self.defer();
                }
                Ok(target)
            }
            Occasion::Finding => match self
                .root
                .file_job(move |root| inspect(root, naming, &lookup, &path, &observed))
                .await
            {
                Ok(target) => Ok(Some(target)),
                Err(RootError::Admin(AdminError::Precondition)) => Ok(None),
                Err(RootError::Admin(AdminError::Io(error))) => {
                    diagnostics::emit(
                        LogLevel::Warn,
                        "sync_finding_unreadable",
                        serde_json::json!({
                            "bucket":self.bucket.as_str(),"object":self.name.as_str(),
                            "reason":crate::error::Report(&error).to_string(),
                            "remedy":Correction::UnreadablePath.remedy()
                        }),
                    );
                    Ok(None)
                }
                Err(error) => Err(error.into()),
            },
        }
    }
}

fn same_inode(left: &File, right: &File) -> Result<bool, AdminError> {
    let a = fsutil::stat_identity(left)?;
    let b = fsutil::stat_identity(right)?;
    Ok(a.node() == b.node())
}

enum Target {
    Absent,
    Identical,
    Conflicting,
    /// A directory holds the name; installation removes it if it may and it is empty.
    Directory,
    Finding(Reason),
    /// A directory or special file holds the path: a tracked file replaced, or a name the tree
    /// cannot hold for an object it never tracked, for the reason given.
    Replaced(Reason),
    /// The directory holds a regular file under another letter case or normalization of the
    /// name, at this path below the root.
    Alias(String),
}

/// What an export target that no finding keeps out holds, which an alias that still holds
/// keeps the export from acting on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetContent {
    Identical,
    Absent,
    /// A file with other bytes, or a directory that installation may remove.
    Changed,
}

fn inspect(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    path: &Path,
    record: &ObjectRecord,
) -> Result<Target, AdminError> {
    let Some(name) = path.file_name() else {
        return Ok(Target::Finding(Reason::InvalidName));
    };
    if !naming.admits(name) {
        return Ok(Target::Finding(Reason::HostRestriction));
    }
    let directory = match parent(
        root,
        naming,
        lookup,
        path.parent()
            .ok_or_else(|| AdminError::Invalid("missing parent".into()))?,
        ParentOpen::Existing,
    ) {
        Ok(directory) => directory,
        Err(ParentError::Missing) => return Ok(Target::Absent),
        Err(ParentError::Finding(reason)) => return Ok(Target::Finding(reason)),
        Err(ParentError::Io(error)) => return Err(error.into()),
    };
    let file = match lookup.regular(&directory, name) {
        Ok(fsutil::RegularChild::File(file)) => file,
        // Without one spelling to name, such as for a file other names link to, the entry
        // stays a collision of this object alone.
        Ok(fsutil::RegularChild::Alias) => {
            return match fsutil::listed_spelling(&directory, name)? {
                // The listing the lookup kept is older than this one, which holds the exact
                // spelling now: a later read decides.
                Some(spelling) if spelling.as_str() == name => Err(AdminError::Precondition),
                Some(spelling) => Ok(path
                    .with_file_name(spelling)
                    .into_os_string()
                    .into_string()
                    // An export path is UTF-8, and so is a spelling the listing returns.
                    .map_or(Target::Finding(Reason::Collision), Target::Alias)),
                None => Ok(Target::Finding(Reason::Collision)),
            };
        }
        Ok(fsutil::RegularChild::Missing) => return Ok(Target::Absent),
        Err(error) if !structural_entry_error(&error) => return Err(error.into()),
        Err(_)
            if fsutil::directory_at(&directory, name, fsutil::DirectoryOpen::Existing).is_ok() =>
        {
            return Ok(Target::Directory);
        }
        Err(_) => return Ok(Target::Replaced(Reason::UnsafeEntry)),
    };
    let before = fsutil::stat_identity(&file)?;
    if before.size != record.size {
        return Ok(Target::Conflicting);
    }
    let fsutil::HashOutcome::Complete(hashes) =
        fsutil::hash_exact(&file, record.size, |_| Ok::<_, io::Error>(()))?
    else {
        return Ok(Target::Conflicting);
    };
    let after = fsutil::stat_identity(&file)?;
    let reopened = fsutil::open_regular_at(&directory, name)?;
    let current_parent = parent(
        root,
        naming,
        lookup,
        path.parent().ok_or(AdminError::Precondition)?,
        ParentOpen::Existing,
    )
    .map_err(|error| match error {
        ParentError::Finding(_) | ParentError::Missing => AdminError::Precondition,
        ParentError::Io(error) => error.into(),
    })?;
    if before != after
        || !same_inode(&file, &reopened)?
        || !same_inode(&directory, &current_parent)?
    {
        return Err(AdminError::Precondition);
    }
    Ok(
        if hashes.md5 == record.md5 && hashes.crc32c == record.crc32c {
            Target::Identical
        } else {
            Target::Conflicting
        },
    )
}
