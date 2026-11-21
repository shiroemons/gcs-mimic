//! Staging a file's bytes and making the file durable before the catalog records it.

#[cfg(test)]
use super::checkpoint;
use super::{Candidate, Ingest};
use crate::{
    diagnostics::{self, LogLevel},
    fsutil::{self, FileIdentity},
    storage::{
        admin::{AdminError, ObjectName, StagedObject},
        sync_state::{
            SyncBinding, SyncFingerprint, SyncPath,
            durable::{self, Durable, NotDurable},
            observation::PathStop,
            root::{RootError, RootLease},
        },
    },
    sync::{
        SyncError,
        correspond::{PathEntry, observe_path},
        naming::RootNaming,
    },
};
use std::{fs::File, io, path::PathBuf};

struct PinnedSource {
    file: File,
    _lease: RootLease,
}
impl std::borrow::Borrow<File> for PinnedSource {
    fn borrow(&self) -> &File {
        &self.file
    }
}

pub(super) fn open_source(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    path: &std::path::Path,
    expected: FileIdentity,
) -> Result<(File, FileIdentity), AdminError> {
    let (directory, file) = match observe_path(root, naming, lookup, path) {
        PathEntry::File { parent, file } => (parent, file),
        PathEntry::Directory(error)
        | PathEntry::Special(error)
        | PathEntry::NameRejected(error)
        | PathEntry::Unreadable(error) => return Err(error.into()),
        PathEntry::Absent { .. }
        | PathEntry::Unplaced(_)
        | PathEntry::Respelled(_)
        | PathEntry::Transient(_) => return Err(AdminError::Precondition),
    };
    if fsutil::stat_identity(&file)? != expected {
        return Err(AdminError::Precondition);
    }
    Ok((file, fsutil::stat_identity(&directory)?))
}

/// The second half of reading a source: re-resolves `path` below `root` and fails with
/// `Precondition` unless it still names the unchanged file `expected` in the directory `parent`.
/// `open_source` took both identities before the read, so a file or any of its ancestors
/// replaced or renamed while it was read is detected.
pub(super) fn recheck_source(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    path: &std::path::Path,
    expected: FileIdentity,
    parent: FileIdentity,
) -> Result<(), AdminError> {
    let (_, current) = open_source(root, naming, lookup, path, expected)?;
    if current.node() == parent.node() {
        Ok(())
    } else {
        Err(AdminError::Precondition)
    }
}

impl Ingest<'_> {
    /// Stages the file's current bytes, verifying that its identity and parent did not change
    /// while it was read.
    pub(super) async fn stage(
        &self,
        selected: PathBuf,
        identity: FileIdentity,
    ) -> Result<StagedObject, RootError> {
        let path = selected.clone();
        let naming = self.naming;
        let lookup = self.lookup.clone();
        let opening = self.lookup.clone();
        let (file, directory) = self
            .root
            .file_job(move |root| open_source(root, naming, &opening, &path, identity))
            .await?;
        #[cfg(test)]
        checkpoint("stage").await;
        let source = PinnedSource {
            file,
            _lease: self.root.clone(),
        };
        let staged = self
            .admin
            .stage_from_file(source, Default::default(), self.config.max_object_bytes)
            .await?;
        #[cfg(test)]
        checkpoint("staged").await;
        let checked = self
            .root
            .file_job(move |root| {
                recheck_source(root, naming, &lookup, &selected, identity, directory)
            })
            .await;
        if let Err(error) = checked {
            if let Err(cleanup) = staged.discard().await {
                diagnostics::emit(
                    LogLevel::Error,
                    "sync_staging_cleanup_failed",
                    serde_json::json!({"reason":crate::error::Report(&cleanup).to_string()}),
                );
            }
            return Err(error);
        }
        Ok(staged)
    }

    /// Stages a candidate's bytes with the fingerprint that tracks them; `None` means the
    /// file changed or became unreadable and was deferred.
    pub(super) async fn stage_update(
        &mut self,
        candidate: &Candidate,
    ) -> Result<Option<(StagedObject, SyncFingerprint)>, SyncError> {
        let staged = match self.stage(candidate.path.clone(), candidate.identity).await {
            Ok(staged) => staged,
            Err(RootError::Admin(error))
                if matches!(
                    &error,
                    AdminError::Precondition
                        | AdminError::NotFound
                        | AdminError::SourceChanged { .. }
                ) || matches!(&error, AdminError::Io(cause) if cause.kind() == io::ErrorKind::NotFound) =>
            {
                self.summary.count_deferred(&candidate.path);
                self.record_observation(
                    &candidate.binding,
                    &candidate.name,
                    PathStop::SourceChanging,
                )
                .await?;
                return Ok(None);
            }
            Err(RootError::Admin(AdminError::Io(error))) => {
                self.defer(&candidate.path, error.kind());
                self.record_observation(&candidate.binding, &candidate.name, PathStop::IoFailure)
                    .await?;
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let hashes = staged.hashes();
        self.clear_observation(candidate.binding.clone(), candidate.name.clone())
            .await?;
        let fingerprint =
            SyncFingerprint::observed(candidate.identity, hashes, self.root.observation_time_ns());
        Ok(Some((staged, fingerprint)))
    }

    async fn record_observation(
        &self,
        binding: &SyncBinding,
        name: &ObjectName,
        reason: PathStop,
    ) -> Result<(), SyncError> {
        crate::sync::error::unless_superseded(
            self.root
                .record_path_stop(binding.clone(), name.clone(), reason)
                .await,
        )?;
        Ok(())
    }

    /// Makes the file at `path`, observed as `identity`, durable together with the directory
    /// entries above it, before the catalog records it. `None` means it could not: the file is
    /// counted as deferred with the path stop that names why, nothing is recorded for it, and the
    /// next scan tries again.
    pub(super) async fn make_durable(
        &mut self,
        path: &SyncPath,
        identity: FileIdentity,
        binding: &SyncBinding,
        name: &ObjectName,
    ) -> Result<Option<Durable>, SyncError> {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        crate::storage::fault::hit_io(
            &self.config.data_dir,
            crate::storage::fault::Step::SyncSourceSync,
        )?;
        let selected = path.clone();
        let lookup = self.lookup.clone();
        let mut synced = std::mem::take(&mut self.synced);
        let (synced, outcome) = self
            .root
            .file_job(move |root| {
                let outcome =
                    durable::make_durable(root, &lookup, &selected, identity, &mut synced);
                Ok((synced, outcome))
            })
            .await?;
        self.synced = synced;
        match outcome {
            Ok(durable) => Ok(Some(durable)),
            Err(NotDurable::Changed) => {
                self.summary.count_deferred(path.as_ref());
                self.record_observation(binding, name, PathStop::SourceChanging)
                    .await?;
                Ok(None)
            }
            Err(NotDurable::Io(error)) => {
                self.defer(path.as_ref(), error.kind());
                self.record_observation(binding, name, PathStop::IoFailure)
                    .await?;
                Ok(None)
            }
        }
    }
}
