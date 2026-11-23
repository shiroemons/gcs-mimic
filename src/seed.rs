//! Local administrative fixture import, completed before the listener binds.

use crate::{
    clock::Clock,
    config::Config,
    diagnostics::{Diagnostic, DiagnosticSource},
    object::ExpectedHashes,
    source_tree::{LARGE_TREE_ENTRIES, TreeLimits, WalkError, warn_large_tree},
    storage::{
        Catalog,
        admin::{
            Admin, BucketDefinition, BucketName, Origin, Publication, PublishMode, Removed,
            SeedBatch, SeedMode,
            initialization::Initialization,
            replay::{self, Phase, Unread},
        },
    },
};
use serde::Serialize;
use std::{path::Path, sync::Arc};
use tokio::sync::mpsc;
use walk::{Fixture, Object, Opened, Seen, Source, Tree, Unlisted, Walked};

mod error;
mod walk;
mod watch;
pub use error::SeedError;
pub use watch::watch;

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum SeedOutcome {
    Disabled,
    Imported {
        buckets: usize,
        objects: usize,
        replaced: usize,
        unchanged: usize,
        blocked: usize,
        skipped_os_metadata: usize,
    },
}

/// Replays the seed tree on every startup, like fake-gcs-server's `-data`: fixture files win
/// over API changes at the same name, while unrelated objects are never deleted.
///
/// `clock` dates the buckets the tree creates and paces the bounded wait for a fixture that
/// fails to open for a moment. A fixture's fingerprint is timed by the system wall clock
/// instead, because it is compared with the timestamps the filesystem wrote.
pub async fn import(
    catalog: &Catalog,
    config: &Config,
    clock: &Arc<dyn Clock>,
) -> Result<SeedOutcome, SeedError> {
    let admin = catalog.admin(Origin::Seed);
    let Some(root) = &config.seed_dir else {
        admin.finish_seed().await?;
        return Ok(SeedOutcome::Disabled);
    };
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Info,
        "seeding",
        serde_json::json!({}),
    );
    let result = match stage_directory(&admin, config, clock, root, None).await {
        Ok(staged) => staged.commit(SeedMode::Merge).await,
        Err(error) => Err(error),
    };
    result.map_err(|error| SeedError::Import(Box::new(error)))
}

/// Refuses a seed directory that directory synchronization used while the marker or the private
/// workspace it kept there remains. Seeding never writes to the folder, so it cannot tidy them,
/// and the workspace may hold the only copy of a file a write-back displaced: the operator
/// inspects and removes them before the server starts.
pub fn refuse_retired_sync(config: &Config) -> Result<(), Diagnostic> {
    let Some(root) = &config.seed_dir else {
        return Ok(());
    };
    let left = crate::storage::retired_sync_left_in_folder(root);
    if left.is_empty() {
        return Ok(());
    }
    Err(Diagnostic::new(
        DiagnosticSource::Seed,
        "seed_dir",
        format!(
            "seed_dir {} still holds {}, which directory synchronization kept in every folder it synchronized. gcs-mimic only reads seed_dir and never changes them. Inspect them first: .gcs-mimic-sync-work may hold the only copy of a file a write-back displaced, and sync-retired.json in the data directory of the server that synchronized the folder lists those files. Delete them yourself once nothing in them is needed, then restart.",
            root.display(),
            left.join(" and ")
        ),
    ))
}

/// A wait for in-flight catalog work that outlasts its budget, or `request_deadline`, ends with a
/// no-changes `Busy`. The work itself may take as long as the tree needs.
pub(crate) async fn reseed(
    admin: &Admin,
    config: &Config,
    clock: &Arc<dyn Clock>,
    request_deadline: tokio::time::Instant,
) -> Result<SeedOutcome, SeedError> {
    let root = config.seed_dir.as_ref().ok_or(SeedError::MissingRoot)?;
    let mode = match config.backend {
        crate::config::Backend::Disk => SeedMode::Merge,
        crate::config::Backend::Memory => SeedMode::Replace,
    };
    let admin = admin
        .clone()
        .with_notification_mode(crate::notification::NotificationMode::Suppress);
    stage_directory(&admin, config, clock, root, Some(request_deadline))
        .await?
        .commit(mode)
        .await
}

/// What a reset to the startup state removed, and what it published in its place. The first
/// three members are those of an empty reset.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Restored {
    #[serde(flatten)]
    removed: Removed,
    cleanup_pending: bool,
    restored: Republished,
}

#[derive(Debug, Serialize)]
struct Republished {
    buckets: usize,
    objects: usize,
}

/// Clears the instance and publishes its startup state in the same transaction, in startup
/// order: the buckets document's buckets, then the seed tree when one is configured. No
/// protection refuses it, as none refuses a reset; waits end as [`reseed`]'s do.
pub(crate) async fn restore(
    admin: &Admin,
    config: &Config,
    clock: &Arc<dyn Clock>,
    request_deadline: tokio::time::Instant,
    initialization: Option<Initialization>,
) -> Result<Restored, SeedError> {
    let admin = admin
        .clone()
        .with_notification_mode(crate::notification::NotificationMode::Suppress);
    let batch = match &config.seed_dir {
        Some(root) => {
            stage_directory(&admin, config, clock, root, Some(request_deadline))
                .await?
                .batch
        }
        None => {
            admin
                .seed_batch_until(Some(request_deadline), config.max_seed_entries)
                .await?
        }
    };
    let counts = batch.restore(initialization).await?;
    Ok(Restored {
        removed: counts.removed,
        cleanup_pending: counts.cleanup_pending,
        restored: Republished {
            buckets: counts.buckets,
            objects: counts.objects,
        },
    })
}

impl From<WalkError> for SeedError {
    fn from(error: WalkError) -> Self {
        match error {
            WalkError::Io(error) | WalkError::Entry { error, .. } => Self::Io(error),
            WalkError::EntryLimit { limit } => Self::EntryLimit { limit: limit.get() },
            WalkError::DepthLimit { limit } => Self::DepthLimit { limit: limit.get() },
            WalkError::NotDirectory => Self::NotDirectory,
        }
    }
}

/// Pinning the fixture root is reading the tree. Reporting its I/O failure as [`SeedError::Io`],
/// as the walk and the fixture opens do, lets an unreadable tree classify alike on every
/// platform instead of passing for a storage failure.
#[cfg(unix)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other administrative failure is carried unchanged as SeedError::Admin"
)]
fn source_failure(error: crate::storage::admin::AdminError) -> SeedError {
    use crate::storage::admin::AdminError;
    match error {
        AdminError::Io(cause) => SeedError::Io(std::io::Error::new(cause.kind(), cause)),
        error => SeedError::Admin(error),
    }
}

/// Pins the fixture root `root`, which `inspected` described as a directory. A root replaced
/// since then fails as [`SeedError::TreeChanged`], as a directory replaced during the walk does.
#[cfg(unix)]
async fn pin_root(
    batch: &SeedBatch,
    root: &Path,
    inspected: &std::fs::Metadata,
) -> Result<Arc<std::fs::File>, SeedError> {
    let (pinned, node) = batch
        .open_root(root.to_owned())
        .await
        .map_err(source_failure)?;
    if node == crate::fsutil::NodeId::from(inspected) {
        Ok(pinned)
    } else {
        Err(SeedError::TreeChanged)
    }
}

/// Applies a startup replay's decision at the entry `label`, which the walk could not read or
/// seeding cannot use for `reason`.
fn unread(label: &str, reason: impl Into<SeedError>) -> Result<(), SeedError> {
    match replay::unread(Phase::Startup, reason.into()) {
        Unread::Fail(error) => Err(error),
        Unread::Report(error) => {
            skipped(label, &error.to_string());
            Ok(())
        }
    }
}

/// Reports the entry `label`, which the walk listed and a lookup then proved absent, at a name
/// without a record: the name owns nothing a replay could delete, and the replay leaves it as it
/// is.
fn vanished(label: &str) {
    skipped(label, "removed after its directory was listed");
}

/// The bucket and object name of `label`, a path below the root that names a recorded object.
/// Only a Unix lookup proves a name absent, so the path separator is `/`.
fn split_label(label: &str) -> Option<(&str, &str)> {
    label.split_once('/')
}

/// Reports the entry `label`, which a replay left as it was for `reason`.
fn skipped(label: &str, reason: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "seed_skipped",
        serde_json::json!({
            "path": label,
            "reason": reason,
        }),
    );
}

/// A fixture tree staged in a batch that has yet to commit.
struct Staged {
    batch: SeedBatch,
    skipped_os_metadata: usize,
}

impl Staged {
    async fn commit(self, mode: SeedMode) -> Result<SeedOutcome, SeedError> {
        let committed = self.batch.commit(mode).await?;
        Ok(SeedOutcome::Imported {
            buckets: committed.buckets,
            objects: committed.objects,
            replaced: committed.replaced,
            unchanged: committed.unchanged,
            // 0.7.0 and earlier left a changed protected fixture as it was and counted it here.
            // A protected fixture now rejects the whole replay, so the count stays 0; the field
            // remains for output compatibility.
            blocked: 0,
            skipped_os_metadata: self.skipped_os_metadata,
        })
    }
}

/// Opens a batch on `catalog`, reads what seeding recorded in it, and starts the walk of the
/// fixture tree at `root`, a directory that is not the data directory or below or above it. On
/// Unix the walk reads the tree through the root pinned in the batch. Startup, a reseed and a
/// pass while the server runs all open the tree this way, so they check and walk it alike.
async fn open_tree(
    catalog: &Admin,
    config: &Config,
    clock: &Arc<dyn Clock>,
    root: &Path,
    request_deadline: Option<tokio::time::Instant>,
) -> Result<(SeedBatch, mpsc::Receiver<Walked>), SeedError> {
    let inspected = tokio::fs::symlink_metadata(root).await?;
    if !inspected.file_type().is_dir() {
        return Err(SeedError::NotDirectory);
    }
    #[cfg(test)]
    tests::injected_source_swap(root);
    let mut batch = catalog
        .seed_batch_until(request_deadline, config.max_seed_entries)
        .await?;
    let recorded = batch.read_records().await?;
    #[cfg(unix)]
    let source_root = pin_root(&batch, root, &inspected).await?;
    let root = tokio::fs::canonicalize(root).await?;
    if config.backend == crate::config::Backend::Disk {
        let data = tokio::fs::canonicalize(&config.data_dir).await?;
        if crate::source_tree::paths_overlap_canonical(&root, &data) {
            return Err(SeedError::Overlap);
        }
    }
    let limits = TreeLimits {
        entries: config.max_seed_entries,
        depth: config.max_seed_depth,
    };
    #[cfg(unix)]
    let tree = Tree::new(root, source_root, Arc::clone(clock));
    #[cfg(not(unix))]
    let tree = Tree::new(root);
    let entries = tree.with_recorded(recorded).walk(limits)?;
    Ok((batch, entries))
}

async fn stage_directory(
    catalog: &Admin,
    config: &Config,
    clock: &Arc<dyn Clock>,
    root: &Path,
    request_deadline: Option<tokio::time::Instant>,
) -> Result<Staged, SeedError> {
    let (batch, mut entries) = open_tree(catalog, config, clock, root, request_deadline).await?;
    let mut seed = Seed {
        batch,
        config,
        clock: clock.as_ref(),
        buckets: 0,
        skipped_os_metadata: 0,
    };
    let mut walked = 0;
    loop {
        let seen = match entries.recv().await {
            Some(Walked::Entry(seen)) => *seen,
            Some(Walked::Unlisted(unlisted)) => {
                seed.unlisted(*unlisted);
                continue;
            }
            Some(Walked::Done) => break,
            // The thread already reported its panic; failing here keeps the seed incomplete.
            None => {
                return Err(SeedError::WalkStopped);
            }
        };
        let Seen { label, fixture } = seen?;
        walked += 1;
        // A chosen limit already says how large a tree may be.
        if walked == LARGE_TREE_ENTRIES + 1 && config.max_seed_entries.is_none() {
            warn_large_tree("seed_tree_large", "seed_dir", "max_seed_entries");
        }
        seed.entry(label, fixture).await?;
    }
    Ok(Staged {
        batch: seed.batch,
        skipped_os_metadata: seed.skipped_os_metadata,
    })
}

/// Shared inputs of one seed import.
struct Seed<'a> {
    batch: SeedBatch,
    config: &'a Config,
    clock: &'a dyn Clock,
    buckets: usize,
    /// OS metadata files such as `.DS_Store` that the seed skipped.
    skipped_os_metadata: usize,
}

impl Seed<'_> {
    /// Replays one walked entry. A failure met before the walk could tell what the entry is
    /// names no entry, as when the walk itself failed there; any other names it by `label`.
    async fn entry(&mut self, label: String, fixture: Fixture) -> Result<(), SeedError> {
        let replayed = match fixture {
            Fixture::Unknown(unknown) => return unread(&label, unknown),
            Fixture::Absent(proof) => {
                let recorded = split_label(&label)
                    .is_some_and(|(bucket, name)| self.batch.absent(bucket, name, proof));
                if !recorded {
                    vanished(&label);
                }
                return Ok(());
            }
            Fixture::Unusable(rejected) => unread(&label, rejected),
            Fixture::OsMetadata => {
                self.skipped_os_metadata += 1;
                Ok(())
            }
            // fake-gcs-server ignores top-level files, and an implicit /data mount often holds
            // a README or .gitkeep, so skipping keeps such trees startable.
            Fixture::TopLevelFile => {
                skipped(&label, "top-level entries must be bucket directories");
                Ok(())
            }
            Fixture::Bucket(name) => self.bucket(&name),
            Fixture::Directory => Ok(()),
            Fixture::Object(object) => self.object(&label, *object).await,
        };
        replayed.map_err(|error| SeedError::Entry {
            label,
            source: Box::new(error),
        })
    }

    /// Takes what a lookup found at a recorded name the walk did not list. Only a proof of
    /// absence changes anything; a file that appeared after its directory was listed, or a name
    /// the lookup could not tell, keeps its record and its object.
    fn unlisted(&mut self, unlisted: Unlisted) {
        let Unlisted {
            bucket,
            name,
            source,
        } = unlisted;
        let label = format!("{bucket}/{name}");
        match source {
            Source::Absent(proof) => {
                self.batch.absent(&bucket, &name, proof);
            }
            Source::File(_) => {}
            Source::Unknown(unknown) => {
                skipped(&label, &SeedError::from(unknown).to_string());
            }
        }
    }

    fn bucket(&mut self, name: &BucketName) -> Result<(), SeedError> {
        if let Some(limit) = self.config.max_buckets_per_batch
            && self.buckets >= limit.get()
        {
            return Err(SeedError::BucketLimit { limit: limit.get() });
        }
        let record = BucketDefinition::default_for(name, &self.config.seed_project, self.clock)?;
        self.batch.bucket(record)?;
        self.buckets += 1;
        Ok(())
    }

    /// Stages a fixture file. Whether it replaces the live object is decided when the batch
    /// commits, where the live object is known.
    async fn object(&mut self, label: &str, object: Object) -> Result<(), SeedError> {
        let Object {
            bucket,
            name,
            leaf,
            source,
        } = object;
        let Opened { file, metadata, .. } = match source {
            Source::File(opened) => *opened,
            Source::Absent(proof) => {
                if !self.batch.absent(bucket.as_str(), name.as_str(), proof) {
                    vanished(label);
                }
                return Ok(());
            }
            Source::Unknown(unknown) => return unread(label, unknown),
        };
        let config = self.config;
        if !metadata.is_file() || metadata.len() > config.max_object_bytes {
            return Err(SeedError::FileLimit);
        }
        let request = Publication {
            content_type: config.seed_content_type.for_name(&leaf).into(),
            expected: ExpectedHashes::default(),
            condition: None,
            metadata: Default::default(),
            max_bytes: config.max_object_bytes,
            mode: PublishMode::Replace,
        };
        self.batch.fixture(bucket, name, request, file).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
