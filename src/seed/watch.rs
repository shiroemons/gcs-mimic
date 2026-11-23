//! The pass that replays the seed directory while the server runs.
//!
//! Each pass walks the tree as a startup replay does and decides each name with the same
//! functions in [`Phase::Live`](replay::Phase::Live): it reads a file again only where its
//! fingerprint differs from the record or the record could have missed a change, deletes an
//! object seeding owns only once two proofs of absence at least a second apart showed its file
//! gone, and never fails on an entry it cannot replay: it reports the entry and leaves the name as
//! it is. A pass that cannot run at all, such as one whose seed directory went away, changes
//! nothing and reports why. No pass writes to the seed directory.

#[cfg(unix)]
use super::pin_root;
use super::{
    SeedError, split_label,
    walk::{Fixture, Object, Opened, Seen, Source, Tree, Unlisted, Walked},
};
use crate::{
    clock::Clock,
    config::Config,
    object::ExpectedHashes,
    source_tree::TreeLimits,
    storage::{
        Catalog,
        admin::{
            AdminError, BucketDefinition, BucketName, LiveOutcome, LiveWalk, Origin, Publication,
            PublishMode, SeedBatch,
            replay::{self, Look},
            seed_records::Mark,
            seed_watch::{LiveCounts, PassReport, Problems},
        },
    },
};
use std::{collections::HashSet, sync::Arc};

/// Starts the passes over the seed directory every [`Config::seed_watch_interval`] on `clock`,
/// unless no seed directory or no interval is configured. The passes keep a copy of the
/// configuration without the buckets document, which only a reset to the startup state reads.
pub fn watch(catalog: &Catalog, config: &Config, clock: Arc<dyn Clock>) {
    let (Some(_), Some(interval)) = (&config.seed_dir, config.seed_watch_interval) else {
        return;
    };
    let mut copy = config.clone();
    copy.buckets = None;
    let config = Arc::new(copy);
    catalog.start_seed_watch(interval, move |catalog| {
        let (config, clock) = (Arc::clone(&config), Arc::clone(&clock));
        async move { pass(&catalog, &config, &clock).await }
    });
}

/// Runs one pass and reports it.
async fn pass(catalog: &Catalog, config: &Config, clock: &Arc<dyn Clock>) -> PassReport {
    let started_at = clock.now();
    let (complete, counts, problems, error) = match walk_and_commit(catalog, config, clock).await {
        Ok((complete, outcome)) => (complete, outcome.counts, outcome.problems, None),
        Err(error) => (
            false,
            LiveCounts::default(),
            Problems::default(),
            Some(error.to_string()),
        ),
    };
    PassReport::new((started_at, clock.now()), complete, counts, problems, error)
}

/// Walks the tree, stages each file to read again, and commits what the pass found. Returns
/// whether the walk was complete.
async fn walk_and_commit(
    catalog: &Catalog,
    config: &Config,
    clock: &Arc<dyn Clock>,
) -> Result<(bool, LiveOutcome), SeedError> {
    let root = config.seed_dir.as_ref().ok_or(SeedError::MissingRoot)?;
    let inspected = tokio::fs::symlink_metadata(root).await?;
    if !inspected.file_type().is_dir() {
        return Err(SeedError::NotDirectory);
    }
    let admin = catalog.admin(Origin::Seed);
    let mut batch = admin
        .seed_batch_until(None, config.max_seed_entries)
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
    let mut entries = tree.with_recorded(recorded).walk(limits)?;
    let mut pass = Pass {
        batch,
        config,
        clock: clock.as_ref(),
        buckets: 0,
        skipped: HashSet::new(),
        walk: LiveWalk {
            complete: true,
            directories: HashSet::new(),
            problems: Problems::default(),
            buckets_kept_deleted: 0,
        },
    };
    loop {
        match entries.recv().await {
            Some(Walked::Entry(seen)) => match *seen {
                Ok(Seen { label, fixture }) => pass.entry(&label, fixture).await?,
                // The walk listed only part of the tree, so its proofs count for nothing.
                Err(error) => pass.incomplete(&error),
            },
            Some(Walked::Unlisted(unlisted)) => pass.unlisted(*unlisted),
            Some(Walked::Done) => break,
            None => {
                pass.incomplete(&SeedError::WalkStopped);
                break;
            }
        }
    }
    let Pass { batch, walk, .. } = pass;
    let complete = walk.complete;
    Ok((complete, batch.commit_live(walk).await?))
}

/// One pass while the server runs.
struct Pass<'a> {
    batch: SeedBatch,
    config: &'a Config,
    clock: &'a dyn Clock,
    /// Top-level directories counted against `max_buckets_per_batch`.
    buckets: usize,
    /// Top-level directories the pass leaves out, with every name below them.
    skipped: HashSet<String>,
    walk: LiveWalk,
}

impl Pass<'_> {
    fn incomplete(&mut self, error: &SeedError) {
        self.walk.complete = false;
        self.walk.problems.report(None, error);
    }

    /// Takes one walked entry. Only a failure of the catalog or of the pass itself is an error;
    /// an entry the pass cannot replay is reported and left as it is.
    async fn entry(&mut self, label: &str, fixture: Fixture) -> Result<(), SeedError> {
        match fixture {
            Fixture::Unknown(unknown) => self.report(label, &SeedError::from(unknown)),
            Fixture::Absent(proof) => {
                if let Some((bucket, name)) = split_label(label)
                    && !self.skipped.contains(bucket)
                {
                    self.batch.absent(bucket, name, proof);
                }
            }
            Fixture::Unusable(rejected) => self.report(label, &SeedError::from(rejected)),
            // Skipped as at startup, without a report on every pass: fake-gcs-server ignores a
            // top-level file, and a mounted directory often holds a README.
            Fixture::OsMetadata | Fixture::TopLevelFile | Fixture::Directory => {}
            Fixture::Bucket(name) => self.bucket(&name)?,
            Fixture::Object(object) => self.object(label, *object).await?,
        }
        Ok(())
    }

    fn report(&mut self, label: &str, reason: &SeedError) {
        self.walk.problems.report(Some(label), reason);
    }

    /// Takes what a lookup found at a recorded name the walk did not list. Only a proof of
    /// absence counts; a name the lookup could not tell is reported.
    fn unlisted(&mut self, unlisted: Unlisted) {
        let Unlisted {
            bucket,
            name,
            source,
        } = unlisted;
        if self.skipped.contains(&bucket) {
            return;
        }
        match source {
            Source::Absent(proof) => {
                self.batch.absent(&bucket, &name, proof);
            }
            Source::File(_) => {}
            Source::Unknown(unknown) => {
                self.report(&format!("{bucket}/{name}"), &SeedError::from(unknown));
            }
        }
    }

    /// Takes a top-level directory: a directory past `max_buckets_per_batch` is left out with
    /// everything below it, as is one whose bucket was deleted after seeding took it. A
    /// directory without a mark gets its bucket when the pass commits.
    fn bucket(&mut self, name: &BucketName) -> Result<(), SeedError> {
        self.walk.directories.insert(name.as_str().to_owned());
        if let Some(limit) = self.config.max_buckets_per_batch
            && self.buckets >= limit.get()
        {
            self.report(
                name.as_str(),
                &SeedError::BucketLimit { limit: limit.get() },
            );
            self.skipped.insert(name.as_str().to_owned());
            return Ok(());
        }
        self.buckets += 1;
        match self.batch.mark(name.as_str()) {
            Mark::Current => {}
            Mark::Gone => {
                self.walk.buckets_kept_deleted += 1;
                self.skipped.insert(name.as_str().to_owned());
            }
            Mark::Unmarked => {
                let definition =
                    BucketDefinition::default_for(name, &self.config.seed_project, self.clock)?;
                if let Err(error) = self.batch.bucket(definition) {
                    self.report(name.as_str(), &staging_problem(error)?);
                    self.skipped.insert(name.as_str().to_owned());
                }
            }
        }
        Ok(())
    }

    /// Takes a fixture file: stages it to be read again unless its fingerprint shows the file
    /// the record read.
    async fn object(&mut self, label: &str, object: Object) -> Result<(), SeedError> {
        let Object {
            bucket,
            name,
            leaf,
            source,
        } = object;
        if self.skipped.contains(bucket.as_str()) {
            return Ok(());
        }
        let Opened {
            file,
            metadata,
            fingerprint,
        } = match source {
            Source::File(opened) => *opened,
            Source::Absent(proof) => {
                self.batch.absent(bucket.as_str(), name.as_str(), proof);
                return Ok(());
            }
            Source::Unknown(unknown) => {
                self.report(label, &SeedError::from(unknown));
                return Ok(());
            }
        };
        let config = self.config;
        if !metadata.is_file() || metadata.len() > config.max_object_bytes {
            self.report(label, &SeedError::FileLimit);
            return Ok(());
        }
        let record = self.batch.record(bucket.as_str(), name.as_str());
        if replay::live_file(record, &fingerprint) == Look::Unchanged {
            return Ok(());
        }
        let request = Publication {
            content_type: config.seed_content_type.for_name(&leaf).into(),
            expected: ExpectedHashes::default(),
            condition: None,
            metadata: Default::default(),
            max_bytes: config.max_object_bytes,
            mode: PublishMode::Replace,
        };
        if let Err(error) = self.batch.fixture(bucket, name, request, file).await {
            self.report(label, &staging_problem(error)?);
        }
        Ok(())
    }
}

/// Why staging an entry failed, where the entry alone failed: the file changed while it was read,
/// could not be read, or does not fit. Any other failure, such as a closed catalog or a full
/// disk, fails the pass.
fn staging_problem(error: AdminError) -> Result<SeedError, SeedError> {
    match error {
        AdminError::SourceChanged { .. }
        | AdminError::Io(_)
        | AdminError::TooLarge
        | AdminError::Invalid(_)
        | AdminError::Checksum
        | AdminError::SeedMetadataBudget { .. } => Ok(SeedError::Admin(error)),
        AdminError::Busy { .. }
        | AdminError::DiskFull(_)
        | AdminError::Closed
        | AdminError::Precondition
        | AdminError::Hold
        | AdminError::SeedProjectMismatch
        | AdminError::Corrupt(_)
        | AdminError::NotFound
        | AdminError::Forbidden
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => Err(SeedError::Admin(error)),
    }
}

#[cfg(test)]
mod tests;
