//! The pass that replays the seed directory while the server runs.
//!
//! Each pass walks the tree as a startup replay does and decides each name with the same
//! functions in [`Phase::Live`](replay::Phase::Live): it reads a file again only where its
//! fingerprint differs from the record or the record could have missed a change, deletes an
//! object seeding owns only once two proofs of absence at least a second apart showed its file
//! gone, and never fails on an entry it cannot replay: it reports the entry and leaves the name as
//! it is. A pass that cannot run at all, such as one whose seed directory went away, changes
//! nothing and reports why. No pass writes to the seed directory.
//!
//! No pass forgets the bucket it took for a top-level directory, even once that directory is gone:
//! a bucket an API request deleted or created again stays as that request left it until a reseed
//! or a restart, however the directory comes and goes.

use super::{
    SeedError, open_tree, split_label,
    walk::{Fixture, Object, Opened, Seen, Source, Unlisted, Walked},
};
use crate::{
    clock::Clock,
    config::Config,
    object::ExpectedHashes,
    storage::{
        Catalog,
        admin::{
            AdminError, BucketDefinition, BucketName, LiveOutcome, LiveWalk, Origin, Publication,
            PublishMode, SeedBatch,
            replay::{self, Look, Phase, Unread},
            seed_records::Mark,
            seed_watch::{PassReport, Problems},
        },
    },
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

/// How long a wait for a settled pass lasts unless its request names another bound: the
/// administration route and `gcs-mimic seed wait` both default to it.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(60);
/// The longest wait for a settled pass a request may name; `gcs-mimic seed wait` refuses a longer
/// `--timeout` before it asks. `request_timeout_ms` ends a longer request sooner.
pub const MAX_WAIT: Duration = Duration::from_secs(600);

/// Starts the passes over the seed directory every [`Config::seed_watch_interval`] on `clock`,
/// unless no seed directory or no interval is configured. The passes keep a copy of the
/// configuration without the buckets document, which only a reset to the startup state reads.
pub fn watch(catalog: &Catalog, config: &Config, clock: Arc<dyn Clock>) {
    let (Some(_), Some(interval)) = (config.seed_dir(), config.seed_watch_interval()) else {
        return;
    };
    let mut copy = config.clone();
    copy.set_startup_buckets(None);
    let config = Arc::new(copy);
    let failures = Arc::new(FailedPasses::default());
    catalog.start_seed_watch(interval, move |catalog, number| {
        let (config, clock, failures) = (
            Arc::clone(&config),
            Arc::clone(&clock),
            Arc::clone(&failures),
        );
        async move { pass(&catalog, &config, &clock, &failures, number).await }
    });
}

/// Runs pass number `number` and reports it. Every report says whether its pass could run, while
/// the diagnostics leave out a failure the last warning already gave.
async fn pass(
    catalog: &Catalog,
    config: &Config,
    clock: &Arc<dyn Clock>,
    failures: &FailedPasses,
    number: u64,
) -> PassReport {
    let started_at = clock.now();
    let outcome = match walk_and_commit(catalog, config, clock, number).await {
        Ok(outcome) => {
            if let Some(failed) = failures.ran() {
                tell_recovered(number, failed);
            }
            Ok(outcome)
        }
        // Shutdown closing the catalog ends a pass that was running. The server is stopping, and
        // nothing failed, so the pass neither warns nor counts toward a run of failed passes.
        Err(error) if catalog.is_closing() => Err(error.reason().to_string()),
        Err(error) => {
            let reason = error.reason().to_string();
            if failures.failed(&reason) {
                warn_failed(number, &error);
            }
            Err(reason)
        }
    };
    PassReport::new((started_at, clock.now()), outcome)
}

/// The passes in a row that could not run since the last pass that ran or since the server
/// started. A failure that lasts, such as a missing seed directory or a full disk, would otherwise
/// warn on every pass, every interval, and bury the other events.
#[derive(Debug, Default)]
struct FailedPasses(Mutex<Option<FailedRun>>);

#[derive(Debug)]
struct FailedRun {
    /// The reason the last warning gave.
    warned: String,
    passes: u64,
}

impl FailedPasses {
    /// Counts a pass that could not run for `reason` and tells whether to warn about it: the
    /// first pass of a run warns, and a later one only for another reason than the last warning
    /// gave.
    fn failed(&self, reason: &str) -> bool {
        let mut current = crate::sync::lock(&self.0);
        let Some(run) = current.as_mut() else {
            *current = Some(FailedRun {
                warned: reason.to_owned(),
                passes: 1,
            });
            return true;
        };
        run.passes += 1;
        if run.warned == reason {
            return false;
        }
        reason.clone_into(&mut run.warned);
        true
    }

    /// Ends the run with a pass that ran, and tells how many passes it held, if any.
    fn ran(&self) -> Option<u64> {
        let mut current = crate::sync::lock(&self.0);
        current.take().map(|run| run.passes)
    }
}

/// Warns that pass number `number` could not run, with the whole chain of causes, which its
/// report leaves out to state the failure in words.
fn warn_failed(number: u64, error: &SeedError) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "seed_pass_failed",
        serde_json::json!({"pass": number, "cause": crate::error::Report(error).to_string()}),
    );
}

/// Tells that pass number `number` ran after `failed` passes in a row could not.
fn tell_recovered(number: u64, failed: u64) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Info,
        "seed_pass_recovered",
        serde_json::json!({"pass": number, "failed_passes": failed}),
    );
}

/// Walks the tree, stages each file to read again, and commits what pass number `number` found.
async fn walk_and_commit(
    catalog: &Catalog,
    config: &Config,
    clock: &Arc<dyn Clock>,
    number: u64,
) -> Result<LiveOutcome, SeedError> {
    let root = config.seed_dir().ok_or(SeedError::MissingRoot)?;
    let admin = catalog.admin(Origin::Seed);
    let (batch, mut entries) = open_tree(&admin, config, clock, root, None).await?;
    let mut pass = Pass {
        batch,
        config,
        clock: clock.as_ref(),
        buckets: 0,
        skipped: HashSet::new(),
        walk: LiveWalk {
            complete: true,
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
    Ok(batch.commit_live(walk, number).await?)
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
        self.walk.problems.report(None, &error.reason());
    }

    /// Takes one walked entry. Only a failure of the catalog or of the pass itself is an error;
    /// an entry the pass cannot replay is reported and left as it is.
    async fn entry(&mut self, label: &str, fixture: Fixture) -> Result<(), SeedError> {
        match fixture {
            Fixture::Unknown(unknown) => self.unread(label, unknown)?,
            Fixture::Absent(proof) => match split_label(label) {
                Some((bucket, name)) if !self.skipped.contains(bucket) => {
                    self.batch.absent(bucket, name, proof);
                }
                // A name below a bucket the pass leaves out, or a top-level name the walk listed
                // and then found empty, which keeps its mark as every directory that went away
                // does.
                Some(_) | None => {}
            },
            Fixture::Unusable(rejected) => self.unread(label, rejected)?,
            // Skipped as at startup, without a report on every pass: fake-gcs-server ignores a
            // top-level file, and a mounted directory often holds a README.
            Fixture::OsMetadata | Fixture::TopLevelFile | Fixture::Directory => {}
            Fixture::Bucket(name) => self.bucket(&name)?,
            Fixture::Object(object) => self.object(label, *object).await?,
        }
        Ok(())
    }

    fn report(&mut self, label: &str, reason: &SeedError) {
        self.walk.problems.report(Some(label), &reason.reason());
    }

    /// Applies a pass's decision at the entry `label`, which the walk could not read or seeding
    /// cannot use for `reason`.
    fn unread(&mut self, label: &str, reason: impl Into<SeedError>) -> Result<(), SeedError> {
        match replay::unread(Phase::Live, reason.into()) {
            Unread::Report(reason) => {
                self.report(label, &reason);
                Ok(())
            }
            Unread::Fail(reason) => Err(reason),
        }
    }

    /// Takes what a lookup found at a recorded name the walk did not list. Only a proof of
    /// absence counts toward a deletion; a name the lookup could not tell about is reported while
    /// seeding owns its object, as the commit decides.
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
                self.batch
                    .unproven(&bucket, &name, &SeedError::from(unknown).reason());
            }
        }
    }

    /// Takes a top-level directory: a directory past `max_buckets_per_batch` is left out with
    /// everything below it, as is one whose bucket was deleted, or created again, after seeding
    /// took it. A directory without a mark gets its bucket when the pass commits.
    fn bucket(&mut self, name: &BucketName) -> Result<(), SeedError> {
        if let Some(limit) = self.config.max_buckets_per_batch()
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
                    BucketDefinition::default_for(name, self.config.seed_project(), self.clock)?;
                if let Err(error) = self.batch.bucket(definition) {
                    self.unread(name.as_str(), staging_problem(error)?)?;
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
            Source::Unknown(unknown) => return self.unread(label, unknown),
        };
        let config = self.config;
        if !metadata.is_file() || metadata.len() > config.max_object_bytes() {
            return self.unread(label, SeedError::FileLimit);
        }
        let record = self.batch.record(bucket.as_str(), name.as_str());
        if replay::live_file(record, &fingerprint) == Look::Unchanged {
            return Ok(());
        }
        let request = Publication {
            content_type: config.seed_content_type().for_name(&leaf).into(),
            expected: ExpectedHashes::default(),
            condition: None,
            metadata: Default::default(),
            max_bytes: config.max_object_bytes(),
            mode: PublishMode::Replace,
        };
        match self.batch.fixture(bucket, name, request, file).await {
            Ok(()) => Ok(()),
            Err(error) => self.unread(label, staging_problem(error)?),
        }
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
