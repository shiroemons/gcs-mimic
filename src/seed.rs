//! Local administrative fixture import, completed before the listener binds.

use crate::{
    clock::Clock,
    config::Config,
    object::ExpectedHashes,
    source_tree::{
        LARGE_TREE_ENTRIES, TreeLimits, WalkEntry, WalkError, Walker, is_os_metadata,
        warn_large_tree,
    },
    storage::{
        Catalog,
        admin::{
            Admin, BucketDefinition, BucketName, ObjectName, Origin, Publication, PublishMode,
            Removed, SeedBatch, SeedMode, initialization::Initialization,
        },
    },
};
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;

mod error;
pub use error::SeedError;

type Entry = Result<WalkEntry, SeedError>;

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
pub async fn import(
    catalog: &Catalog,
    config: &Config,
    clock: &dyn Clock,
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

/// A wait for in-flight catalog work that outlasts its budget, or `request_deadline`, ends with a
/// no-changes `Busy`. The work itself may take as long as the tree needs.
pub(crate) async fn reseed(
    admin: &Admin,
    config: &Config,
    clock: &dyn Clock,
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

/// What a reset to the startup state removed, and what it published in its place.
#[derive(Debug, Serialize)]
pub(crate) struct Restored {
    #[serde(flatten)]
    removed: Removed,
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
    clock: &dyn Clock,
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
        restored: Republished {
            buckets: counts.buckets,
            objects: counts.objects,
        },
    })
}

impl From<WalkError> for SeedError {
    fn from(error: WalkError) -> Self {
        match error {
            WalkError::Io(error) => Self::Io(error),
            WalkError::EntryLimit { limit } => Self::EntryLimit { limit: limit.get() },
            WalkError::DepthLimit { limit } => Self::DepthLimit { limit: limit.get() },
            WalkError::NotDirectory => Self::NotDirectory,
        }
    }
}

/// Opening the fixture tree is reading it. Reporting its I/O failure as [`SeedError::Io`], as
/// the walk and the portable open path do, lets an unreadable fixture classify alike on every
/// platform instead of passing for a storage failure.
#[cfg(unix)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other administrative failure is carried unchanged as SeedError::Admin"
)]
fn source_failure(error: crate::storage::admin::AdminError) -> SeedError {
    use crate::storage::admin::AdminError;
    match error {
        AdminError::Io(cause) => SeedError::Io(io::Error::new(cause.kind(), cause)),
        error => SeedError::Admin(error),
    }
}

/// One message from the walk thread. The walk ends only with [`Walked::Done`]: a thread that
/// panicked or stopped early drops its sender without one, and a closed channel alone would
/// otherwise look like the end of a complete walk and mark a partial seed complete. The entry is
/// boxed because Linux `Metadata` carries statx fields that make an unboxed variant over 200 bytes.
enum Walked {
    Entry(Box<Entry>),
    Done,
}

fn entries(root: PathBuf, limits: TreeLimits) -> io::Result<mpsc::Receiver<Walked>> {
    let (send, receive) = mpsc::channel(2);
    crate::task::spawn_thread("seed-walk", move || {
        let walk = match Walker::new(&root, limits) {
            Ok(walk) => walk,
            Err(error) => {
                let _ = send.blocking_send(Walked::Entry(Box::new(Err(error.into()))));
                return;
            }
        };
        #[cfg(test)]
        let walk = walk.enumerate().map(|(index, entry)| {
            tests::injected_walk_panic(&root, index);
            entry
        });
        for entry in walk {
            let entry = entry.map_err(SeedError::from);
            if send.blocking_send(Walked::Entry(Box::new(entry))).is_err() {
                return;
            }
        }
        let _ = send.blocking_send(Walked::Done);
    })?;
    Ok(receive)
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
            // Atomic replay never skips fixtures; retain the field for output compatibility.
            blocked: 0,
            skipped_os_metadata: self.skipped_os_metadata,
        })
    }
}

async fn stage_directory(
    catalog: &Admin,
    config: &Config,
    clock: &dyn Clock,
    root: &Path,
    request_deadline: Option<tokio::time::Instant>,
) -> Result<Staged, SeedError> {
    let inspected = tokio::fs::symlink_metadata(root).await?;
    if !inspected.file_type().is_dir() {
        return Err(SeedError::NotDirectory);
    }
    #[cfg(test)]
    tests::injected_source_swap(root);
    let batch = catalog
        .seed_batch_until(request_deadline, config.max_seed_entries)
        .await?;
    #[cfg(unix)]
    let source_root = batch
        .open_root(root.to_owned(), inspected)
        .await
        .map_err(source_failure)?;
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
    let mut entries = entries(root.clone(), limits)?;
    let mut seed = Seed {
        batch,
        #[cfg(unix)]
        source_root,
        config,
        clock,
        buckets: 0,
        skipped_os_metadata: 0,
    };
    let mut walked = 0;
    loop {
        let entry = match entries.recv().await {
            Some(Walked::Entry(entry)) => *entry,
            Some(Walked::Done) => break,
            // The thread already reported its panic; failing here keeps the seed incomplete.
            None => {
                return Err(SeedError::WalkStopped);
            }
        };
        let WalkEntry {
            path,
            metadata,
            depth,
        } = entry?;
        walked += 1;
        // A chosen limit already says how large a tree may be.
        if walked == LARGE_TREE_ENTRIES + 1 && limits.entries.is_none() {
            warn_large_tree("seed_tree_large", "seed_dir", "max_seed_entries");
        }
        // The walker only yields paths below the root it was given.
        let relative = path.strip_prefix(&root).map_err(SeedError::RelativePath)?;
        let label = relative.display().to_string();
        let fixture = Fixture {
            path: &path,
            relative,
            label: &label,
            metadata: &metadata,
            depth,
        };
        seed.entry(&fixture)
            .await
            .map_err(|error| SeedError::Entry {
                label,
                source: Box::new(error),
            })?;
    }
    Ok(Staged {
        batch: seed.batch,
        skipped_os_metadata: seed.skipped_os_metadata,
    })
}

/// Shared inputs of one seed import.
struct Seed<'a> {
    batch: SeedBatch,
    #[cfg(unix)]
    source_root: std::sync::Arc<std::fs::File>,
    config: &'a Config,
    clock: &'a dyn Clock,
    buckets: usize,
    /// OS metadata files such as `.DS_Store` that the seed skipped.
    skipped_os_metadata: usize,
}

/// One walked fixture entry.
struct Fixture<'a> {
    path: &'a Path,
    relative: &'a Path,
    label: &'a str,
    metadata: &'a std::fs::Metadata,
    depth: usize,
}

/// What one walked fixture entry is, decided once from its depth and type.
enum FixtureKind<'a> {
    /// A file macOS or Windows leaves beside fixtures; it is skipped and counted.
    OsMetadata,
    /// A top-level file, which fake-gcs-server ignores; it is skipped with a warning.
    TopLevelFile,
    /// A top-level directory names a bucket.
    Bucket(&'a str),
    /// A directory below a bucket. Its path must still be a valid object name.
    Directory,
    /// A regular file below a bucket names an object; `leaf` selects its content type.
    Object {
        bucket: BucketName,
        name: ObjectName,
        leaf: &'a str,
    },
}

impl<'a> Fixture<'a> {
    /// Classifies the entry. Its checks fail in a fixed order: the leaf name, the entry type, the
    /// bucket component, the object name, then the bucket name of an object.
    fn kind(&self) -> Result<FixtureKind<'a>, SeedError> {
        let leaf = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(SeedError::NonUtf8Name)?;
        let kind = self.metadata.file_type();
        if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
            return Err(SeedError::UnsupportedFile);
        }
        if kind.is_file() && is_os_metadata(leaf) {
            return Ok(FixtureKind::OsMetadata);
        }
        let mut components = self.relative.iter();
        let bucket = components
            .next()
            .and_then(|part| part.to_str())
            .ok_or(SeedError::NonUtf8Name)?;
        if self.depth == 0 {
            return Ok(if kind.is_dir() {
                FixtureKind::Bucket(leaf)
            } else {
                FixtureKind::TopLevelFile
            });
        }
        let name = components
            .map(|part| part.to_str().ok_or(SeedError::NonUtf8Name))
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        // Startup and the reseed route report a fixed message, so the rule it broke is dropped.
        let name = ObjectName::try_from(name).map_err(|_| {
            SeedError::ObjectName(crate::json::JsonError::invalid_parameter(
                "Invalid object name",
            ))
        })?;
        if kind.is_dir() {
            return Ok(FixtureKind::Directory);
        }
        let bucket = BucketName::new(bucket).map_err(SeedError::BucketName)?;
        Ok(FixtureKind::Object { bucket, name, leaf })
    }
}

impl Seed<'_> {
    async fn entry(&mut self, fixture: &Fixture<'_>) -> Result<(), SeedError> {
        match fixture.kind()? {
            FixtureKind::OsMetadata => self.skipped_os_metadata += 1,
            // fake-gcs-server ignores top-level files, and an implicit /data mount often holds
            // a README or .gitkeep, so skipping keeps such trees startable.
            FixtureKind::TopLevelFile => crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "seed_skipped",
                serde_json::json!({
                    "path": fixture.label,
                    "reason": "top-level entries must be bucket directories",
                }),
            ),
            FixtureKind::Bucket(leaf) => self.bucket(leaf)?,
            FixtureKind::Directory => {}
            FixtureKind::Object { bucket, name, leaf } => {
                self.object(fixture, bucket, name, leaf).await?;
            }
        }
        Ok(())
    }

    fn bucket(&mut self, leaf: &str) -> Result<(), SeedError> {
        if let Some(limit) = self.config.max_buckets_per_batch
            && self.buckets >= limit.get()
        {
            return Err(SeedError::BucketLimit { limit: limit.get() });
        }
        let name = BucketName::new(leaf).map_err(SeedError::BucketName)?;
        let record = BucketDefinition::default_for(&name, &self.config.seed_project, self.clock)?;
        self.batch.bucket(record)?;
        self.buckets += 1;
        Ok(())
    }

    async fn object(
        &mut self,
        fixture: &Fixture<'_>,
        bucket: BucketName,
        name: ObjectName,
        leaf: &str,
    ) -> Result<(), SeedError> {
        let config = self.config;
        #[cfg(test)]
        tests::injected_source_swap(fixture.path);
        #[cfg(unix)]
        let file = tokio::fs::File::from_std(
            self.batch
                .open_source(self.source_root.clone(), fixture.relative.to_owned())
                .await
                .map_err(source_failure)?,
        );
        #[cfg(not(unix))]
        let file = tokio::fs::File::open(fixture.path).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() || metadata.len() > config.max_object_bytes {
            return Err(SeedError::FileLimit);
        }
        let content_type = config.seed_content_type.for_name(leaf);
        let file = file.into_std().await;
        let request = Publication {
            content_type: content_type.into(),
            expected: ExpectedHashes::default(),
            condition: None,
            metadata: Default::default(),
            max_bytes: config.max_object_bytes,
            mode: PublishMode::Replace,
        };
        self.batch.object(bucket, name, request, file).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
