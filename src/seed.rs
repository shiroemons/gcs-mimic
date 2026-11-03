//! Local administrative fixture import, completed before the listener binds.

use crate::{
    clock::Clock,
    config::Config,
    json::validate_object_name,
    object::ExpectedHashes,
    source_tree::{WalkError, Walker, is_os_metadata},
    storage::{
        Catalog,
        admin::{
            Admin, BucketDefinition, BucketName, ObjectName, Origin, Publication, PublishMode,
            SeedBatch, SeedMode,
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

const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;
type Entry = Result<(PathBuf, std::fs::Metadata, usize), SeedError>;

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

#[derive(Default)]
struct Counts {
    buckets: usize,
    objects: usize,
    replaced: usize,
    unchanged: usize,
    blocked: usize,
    skipped_os_metadata: usize,
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
    let result = import_directory(&admin, config, clock, root, SeedMode::Merge).await;
    result.map_err(|error| SeedError::Import(Box::new(error)))
}

pub(crate) async fn reseed(
    admin: &Admin,
    config: &Config,
    clock: &dyn Clock,
) -> io::Result<SeedOutcome> {
    let root = config
        .seed_dir
        .as_ref()
        .ok_or(SeedError::MissingRoot)
        .map_err(io::Error::other)?;
    let mode = match config.backend {
        crate::config::Backend::Disk => SeedMode::Merge,
        crate::config::Backend::Memory => SeedMode::Replace,
    };
    let admin = admin
        .clone()
        .with_notification_mode(crate::notification::NotificationMode::Suppress);
    import_directory(&admin, config, clock, root, mode)
        .await
        .map_err(|error| match error {
            SeedError::Io(error) => error,
            SeedError::Admin(error) => io::Error::other(error),
            error => io::Error::other(error),
        })
}

fn walk_error(error: WalkError) -> SeedError {
    match error {
        WalkError::Io(error) => SeedError::Io(error),
        WalkError::EntryLimit => SeedError::EntryLimit,
        WalkError::DepthLimit => SeedError::DepthLimit,
        WalkError::NotDirectory => SeedError::NotDirectory,
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

fn entries(root: PathBuf) -> io::Result<mpsc::Receiver<Walked>> {
    let (send, receive) = mpsc::channel(2);
    crate::task::spawn_thread("seed-walk", move || {
        let walk = match Walker::new(&root, MAX_ENTRIES, MAX_DEPTH) {
            Ok(walk) => walk,
            Err(error) => {
                let _ = send.blocking_send(Walked::Entry(Box::new(Err(walk_error(error)))));
                return;
            }
        };
        #[cfg(test)]
        let walk = walk.enumerate().map(|(index, entry)| {
            tests::injected_walk_panic(&root, index);
            entry
        });
        for entry in walk {
            if send
                .blocking_send(Walked::Entry(Box::new(entry.map_err(walk_error))))
                .is_err()
            {
                return;
            }
        }
        let _ = send.blocking_send(Walked::Done);
    })?;
    Ok(receive)
}

async fn import_directory(
    catalog: &Admin,
    config: &Config,
    clock: &dyn Clock,
    root: &Path,
    mode: SeedMode,
) -> Result<SeedOutcome, SeedError> {
    let inspected = tokio::fs::symlink_metadata(root).await?;
    if !inspected.file_type().is_dir() {
        return Err(SeedError::NotDirectory);
    }
    #[cfg(test)]
    tests::injected_source_swap(root);
    let batch = catalog.seed_batch()?;
    #[cfg(unix)]
    let source_root = batch.open_root(root.to_owned(), inspected).await?;
    let root = tokio::fs::canonicalize(root).await?;
    if config.backend == crate::config::Backend::Disk {
        let data = tokio::fs::canonicalize(&config.data_dir).await?;
        if crate::source_tree::paths_overlap_canonical(&root, &data) {
            return Err(SeedError::Overlap);
        }
    }
    let mut entries = entries(root.clone())?;
    let mut seed = Seed {
        batch,
        #[cfg(unix)]
        source_root,
        config,
        clock,
    };
    let mut counts = Counts::default();
    loop {
        let entry = match entries.recv().await {
            Some(Walked::Entry(entry)) => *entry,
            Some(Walked::Done) => break,
            // The thread already reported its panic; failing here keeps the seed incomplete.
            None => {
                return Err(SeedError::WalkStopped);
            }
        };
        let (path, metadata, depth) = entry?;
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
        seed.entry(&fixture, &mut counts)
            .await
            .map_err(|error| SeedError::Entry {
                label,
                source: Box::new(error),
            })?;
    }
    let committed = seed.batch.commit(mode).await?;
    counts.buckets = committed.buckets;
    counts.objects = committed.objects;
    counts.replaced = committed.replaced;
    counts.unchanged = committed.unchanged;
    Ok(SeedOutcome::Imported {
        buckets: counts.buckets,
        objects: counts.objects,
        replaced: counts.replaced,
        unchanged: counts.unchanged,
        blocked: counts.blocked,
        skipped_os_metadata: counts.skipped_os_metadata,
    })
}

/// Shared inputs of one seed import.
struct Seed<'a> {
    batch: SeedBatch,
    #[cfg(unix)]
    source_root: std::sync::Arc<std::fs::File>,
    config: &'a Config,
    clock: &'a dyn Clock,
}

/// One walked fixture entry.
struct Fixture<'a> {
    path: &'a Path,
    relative: &'a Path,
    label: &'a str,
    metadata: &'a std::fs::Metadata,
    depth: usize,
}

impl Seed<'_> {
    async fn entry(&mut self, fixture: &Fixture<'_>, counts: &mut Counts) -> Result<(), SeedError> {
        let leaf = fixture
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(SeedError::NonUtf8Name)?;
        let kind = fixture.metadata.file_type();
        if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
            return Err(SeedError::UnsupportedFile);
        }
        if kind.is_file() && is_os_metadata(leaf) {
            counts.skipped_os_metadata += 1;
            return Ok(());
        }
        let mut components = fixture.relative.iter();
        let bucket = components
            .next()
            .and_then(|part| part.to_str())
            .ok_or(SeedError::NonUtf8Name)?;
        let (bucket, name) = if fixture.depth > 0 {
            let name = components
                .map(|part| part.to_str().ok_or(SeedError::NonUtf8Name))
                .collect::<Result<Vec<_>, _>>()?
                .join("/");
            validate_object_name(&name).map_err(SeedError::ObjectName)?;
            (bucket.to_owned(), name)
        } else {
            // fake-gcs-server ignores top-level files, and an implicit /data mount often holds
            // a README or .gitkeep, so skipping keeps such trees startable.
            if !kind.is_dir() {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "seed_skipped",
                    serde_json::json!({
                        "path": fixture.label,
                        "reason": "top-level entries must be bucket directories",
                    }),
                );
                return Ok(());
            }
            self.bucket(leaf)?;
            (leaf.to_owned(), String::new())
        };
        if kind.is_file() {
            let bucket = BucketName::new(bucket).map_err(SeedError::BucketName)?;
            let name = ObjectName::new(name)?;
            self.object(fixture, bucket, name, leaf).await?;
        }
        Ok(())
    }

    fn bucket(&mut self, leaf: &str) -> Result<(), SeedError> {
        let name = BucketName::new(leaf).map_err(SeedError::BucketName)?;
        let record = BucketDefinition::default_for(&name, &self.config.seed_project, self.clock)?;
        self.batch.bucket(record)?;
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
                .await?,
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
