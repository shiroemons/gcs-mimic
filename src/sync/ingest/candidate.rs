//! Selecting the changed files of a scan page, and what hashing a tracked file shows.

use super::stage::{open_source, recheck_source};
use super::{AliasOutcome, Candidate, Ingest};
use crate::{
    fsutil::{self, FileIdentity},
    storage::{
        admin::{AdminError, ObjectName},
        sync_state::{
            SyncBinding, SyncFingerprint, SyncPath, SyncRecord, SyncUpdate, root::RootError,
        },
    },
    sync::{
        SyncError,
        entries::{EntryProblem, MappedFile},
    },
};
use std::{io, path::PathBuf};

/// What a file's stat observation shows about the body its tracked fingerprint recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackedCheck {
    /// Size, inode, mtime and ctime match a stable fingerprint: the body is unchanged.
    Unchanged,
    /// Only hashing the file can tell: the fingerprint is racy, or the inode number alone
    /// differs. Docker Desktop's shared folders can report a new number for an unchanged
    /// file, so that difference alone does not show an edit.
    Hash,
    /// Size, mtime or ctime differ: the file is an ingestion candidate.
    Changed,
}

impl TrackedCheck {
    fn new(fingerprint: &SyncFingerprint, identity: FileIdentity) -> Self {
        let changes = fingerprint.changed_fields(identity);
        if changes.is_empty() && !fingerprint.racy {
            Self::Unchanged
        } else if changes.is_empty() || changes.is_inode_only() {
            Self::Hash
        } else {
            Self::Changed
        }
    }
}

/// What hashing a file showed about its tracked body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HashCheck {
    /// The file holds the tracked body, and its observed identity is now recorded.
    Recorded,
    /// The file holds the tracked body, but the API generation moved on or the object is gone,
    /// so the observed identity is not recorded. The file is no external edit, and the pass
    /// handles it like an unchanged file: the export or removal of the API change replaces it.
    /// Ingesting it would report that change as a conflict. Counting it as deferred would keep
    /// a wait reporting the source as changing, because only that export records a new
    /// fingerprint, and a finding can hold the export back indefinitely.
    Superseded,
    /// The bytes differ, or the file changed while it was read.
    Differs,
    /// The file holds the tracked body, but it could not be made durable, so nothing is recorded
    /// and it counts as deferred, as an ingested file would.
    Deferred,
}

impl Ingest<'_> {
    /// Selects a regular file whose observed state differs from its tracked fingerprint;
    /// `None` means it is unchanged or was counted as unrepresentable or deferred.
    pub(super) async fn candidate(
        &mut self,
        path: PathBuf,
        file: MappedFile,
    ) -> Result<Option<Candidate>, SyncError> {
        let MappedFile {
            bucket,
            name,
            identity,
        } = file;
        let state = self.catalog.sync_state();
        let binding = match self.root.sync_binding(&bucket).await {
            Ok(binding) => binding,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                self.summary.count_deferred(&path);
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let text = path
            .to_str()
            .ok_or(SyncError::Invariant("invalid sync path"))?
            .to_owned();
        // The tracked state's lookup checks the same spelling, so this fails exactly where it would.
        let key = SyncPath::try_from(text.clone())?;
        let tracked = match state.get_path(binding.clone(), text).await {
            Ok(tracked) => tracked,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                self.summary.count_deferred(&path);
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(tracked) = &tracked {
            let unchanged = match TrackedCheck::new(&tracked.fingerprint, identity) {
                TrackedCheck::Unchanged => true,
                TrackedCheck::Hash => match self
                    .hash_tracked(&key, identity, tracked, &binding, &name)
                    .await?
                {
                    HashCheck::Recorded => {
                        self.summary.identical += 1;
                        true
                    }
                    HashCheck::Superseded => true,
                    HashCheck::Differs => false,
                    HashCheck::Deferred => return Ok(None),
                },
                TrackedCheck::Changed => false,
            };
            if unchanged {
                self.clear_observation(binding, name).await?;
                return Ok(None);
            }
            match self.alias_edit(&binding, &name, &path, &key).await? {
                AliasOutcome::Free => {}
                AliasOutcome::Kept => return Ok(None),
                AliasOutcome::HandOver(heir) => {
                    let holder = Candidate {
                        path,
                        key,
                        identity,
                        bucket,
                        name,
                        binding,
                        tracked: Some(tracked.clone()),
                        live: None,
                    };
                    self.hand_over(holder, heir).await?;
                    return Ok(None);
                }
            }
        } else if self
            .root
            .frozen_elsewhere(binding.clone(), name.clone(), key.clone())
            .await?
        {
            // The frozen file of an alias, renamed on the host to another object's spelling:
            // taking it in for that object would end its alias finding while the file still
            // stands for the others, so it stays frozen until an API change ends the alias. It
            // is not deferred, since no later pass decides otherwise and a deferral would hold
            // every wait; the wait names it among the frozen files instead.
            return Ok(None);
        }
        let live = match self.admin.get_object(&bucket, &name, None).await {
            Ok(record) => Some(record),
            Err(AdminError::NotFound) => None,
            Err(error) => return Err(error.into()),
        };
        if crate::sync::export::reconcile::hierarchy_collision(&self.admin, &bucket, &name).await? {
            self.summary
                .count_unrepresentable_entry(&path, EntryProblem::PathCollision);
            return Ok(None);
        }
        Ok(Some(Candidate {
            path,
            key,
            identity,
            bucket,
            name,
            binding,
            tracked,
            live,
        }))
    }

    /// Hashes the file at `path`, observed as `expected`, and compares it with the tracked body.
    /// A match records the observed identity, so a later stable pass skips the hash. The file
    /// may be a new one with the same bytes, such as an editor's save through a rename, whose
    /// entry is as volatile as an ingested file's, so it is made durable first like one.
    async fn hash_tracked(
        &mut self,
        path: &SyncPath,
        expected: FileIdentity,
        tracked: &SyncRecord,
        binding: &SyncBinding,
        name: &ObjectName,
    ) -> Result<HashCheck, SyncError> {
        let selected = PathBuf::from(path.as_str());
        let naming = self.naming;
        let lookup = self.lookup.for_job();
        let observed = self
            .root
            .file_job(move |root| {
                let (file, parent) = open_source(root, naming, &lookup, &selected, expected)?;
                let hashes = fsutil::hash_exact(&file, expected.size, |_| Ok::<_, io::Error>(()))?;
                recheck_source(root, naming, &lookup, &selected, expected, parent)?;
                Ok(hashes)
            })
            .await;
        let hashes = match observed {
            Ok(fsutil::HashOutcome::Complete(hashes)) => hashes,
            Ok(_) | Err(RootError::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                return Ok(HashCheck::Differs);
            }
            Err(error) => return Err(error.into()),
        };
        if hashes.md5 != tracked.fingerprint.md5 || hashes.crc32c != tracked.fingerprint.crc32c {
            return Ok(HashCheck::Differs);
        }
        let fingerprint =
            SyncFingerprint::observed(expected, hashes, self.root.observation_time_ns());
        let Some(durable) = self.make_durable(path, expected, binding, name).await? else {
            return Ok(HashCheck::Deferred);
        };
        let update = SyncUpdate::new(binding.clone(), durable, fingerprint)?;
        match self
            .catalog
            .sync_state()
            .acknowledge(name, tracked.generation, update)
            .await
        {
            Ok(()) => Ok(HashCheck::Recorded),
            Err(AdminError::Precondition | AdminError::NotFound) => Ok(HashCheck::Superseded),
            Err(error) => Err(error.into()),
        }
    }
}
