//! The commit of a pass while the server runs: one transaction beside the requests the server
//! serves. The batch holds its admission and its lease until the commit ends, so no reseed, no
//! reset to the startup state and no other pass runs meanwhile, and no reset clears what the pass
//! staged.

use super::*;
use crate::storage::catalog::admin::{
    replay::{Confirmation, Reread},
    seed_watch::{LiveCounts, Problems},
};
use std::collections::HashSet;

const PROTECTED_FILE: &str = "the file changed, but a hold or retention period protects the object; the object stays until the file changes again";
const PROTECTED_DELETION: &str =
    "the file is gone, but a hold or retention period protects the object seeding published";
const OTHER_PROJECT: &str =
    "a bucket of another project has the directory's name; the directory is left out";

/// What a pass while the server runs found beside the files it staged and the names it proved
/// absent.
pub(crate) struct LiveWalk {
    /// Whether the walk listed the whole tree, met no failure and ended with its final check of
    /// the root. Only then do its proofs of absence count, and only then does the pass forget the
    /// mark of a directory it did not list.
    pub(crate) complete: bool,
    /// The top-level directories the walk listed, by bucket name.
    pub(crate) directories: HashSet<String>,
    /// What the walk left as it was.
    pub(crate) problems: Problems,
    /// Directories whose bucket was deleted after seeding took it, which the pass left out.
    pub(crate) buckets_kept_deleted: usize,
}

/// What a pass committed, and what it left as it was.
pub(crate) struct LiveOutcome {
    pub(crate) counts: LiveCounts,
    pub(crate) problems: Problems,
}

impl SeedBatch {
    /// Commits pass number `pass` while the server runs. A pass that would change nothing opens
    /// no transaction and wakes nothing.
    pub(crate) async fn commit_live(
        mut self,
        walk: LiveWalk,
        pass: u64,
    ) -> Result<LiveOutcome, AdminError> {
        let mut counts = LiveCounts {
            buckets_kept_deleted: walk.buckets_kept_deleted,
            ..LiveCounts::default()
        };
        let found = self.found_names(&walk);
        self.keep_owned_absences(&walk, &mut counts);
        if self.entries.is_empty()
            && self.buckets.is_empty()
            && self.absences.is_empty()
            && found.is_empty()
            && !self.marks_to_forget(&walk)
        {
            return Ok(LiveOutcome {
                counts,
                problems: walk.problems,
            });
        }
        let catalog = self.admin.catalog.clone();
        self.prepare_entries().await?;
        let deadline = wait_deadline(&catalog, None);
        let execution =
            tokio::time::timeout_at(deadline, catalog.0.execution.clone().acquire_owned())
                .await
                .map_err(|_| AdminError::busy())?
                .map_err(|_| AdminError::Closed)?;
        let (outcome, mut batch) = catalog
            .run_upload_admitted::<_, AdminError>(execution, move |connection| {
                let outcome = commit(connection, &self, walk, &found, (pass, counts));
                Ok((outcome, self))
            })
            .await?;
        batch.clean_up_staged().await?;
        if let Ok(outcome) = &outcome
            && outcome.counts.published + outcome.counts.deleted > 0
        {
            // A replaced or deleted version may wait in a bucket that retains deletions.
            catalog.0.maintenance_wake.notify_one();
            catalog.reclaim_after_commit().await;
        }
        outcome
    }

    /// The recorded names whose proof of absence a pass clears ([`SeedRecord::found`]): every
    /// name that kept one, except where a complete walk proved it absent again.
    fn found_names(&self, walk: &LiveWalk) -> Vec<RecordKey> {
        let proven: HashSet<&RecordKey> = if walk.complete {
            self.absences.iter().map(|(key, _)| key).collect()
        } else {
            HashSet::new()
        };
        self.records
            .iter()
            .flatten()
            .filter(|(key, record)| record.absent.is_some() && !proven.contains(key))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Keeps the proofs of absence that may delete: only those of a complete walk, and only at
    /// names where seeding owns a version. Every other name proven absent stays a tombstone,
    /// which changes nothing, and is counted.
    fn keep_owned_absences(&mut self, walk: &LiveWalk, counts: &mut LiveCounts) {
        let absences = std::mem::take(&mut self.absences);
        if !walk.complete {
            return;
        }
        let (owned, tombstones): (Vec<_>, Vec<_>) =
            absences.into_iter().partition(|((bucket, name), _)| {
                self.record(bucket, name)
                    .is_some_and(|record| record.owned.is_some())
            });
        counts.tombstones += tombstones.len();
        self.absences = owned;
    }

    /// Whether a complete walk no longer listed a directory whose bucket seeding took.
    fn marks_to_forget(&self, walk: &LiveWalk) -> bool {
        walk.complete
            && self
                .marks
                .keys()
                .any(|marked| !walk.directories.contains(marked))
    }
}

/// The commit of pass number `pass`, which starts from `counts`.
fn commit(
    connection: &mut Connection,
    batch: &SeedBatch,
    walk: LiveWalk,
    found: &[RecordKey],
    (pass, mut counts): (u64, LiveCounts),
) -> Result<LiveOutcome, AdminError> {
    let transaction = sql::immediate(connection).map_err(objects_error)?;
    let now = super::super::recorded_instant(batch.admin.catalog.0.clock.now())?;
    check_guards(&transaction, SeedMode::Merge, now)?;
    let LiveWalk {
        complete,
        directories,
        mut problems,
        ..
    } = walk;
    take_buckets(&transaction, batch, &mut counts, &mut problems)?;
    replay_live_files(&transaction, batch, now, &mut counts, &mut problems)?;
    replay_live_absences(&transaction, batch, (pass, now), &mut counts, &mut problems)?;
    for (bucket, name) in found {
        seed_records::found(&transaction, bucket, name)?;
    }
    if complete {
        forget_marks(&transaction, |bucket| directories.contains(bucket))?;
    }
    objects::commit_publication(transaction)?;
    Ok(LiveOutcome { counts, problems })
}

/// Creates the bucket of each directory the walk found without a mark, or takes over a bucket of
/// the seed project an API request created under the directory's name, and marks it. A directory
/// marked since the walk read the marks is left as it is, and so is one whose name a bucket of
/// another project has, which the pass reports.
fn take_buckets(
    transaction: &rusqlite::Transaction<'_>,
    batch: &SeedBatch,
    counts: &mut LiveCounts,
    problems: &mut Problems,
) -> Result<(), AdminError> {
    for definition in &batch.buckets {
        let record = &definition.0;
        if seed_records::mark_of(transaction, &record.name)? != Mark::Unmarked {
            continue;
        }
        let existing = crate::storage::catalog::select_bucket(transaction, &record.name)
            .map_err(objects_error)?;
        if existing.is_some_and(|existing| existing.project != record.project) {
            problems.report(Some(&record.name), &OTHER_PROJECT);
            continue;
        }
        if create_bucket(transaction, &batch.admin.catalog, record)? == Bucket::Created {
            counts.buckets_created += 1;
        }
        seed_records::mark(transaction, &record.name)?;
    }
    Ok(())
}

/// Whether the pass replays files into `bucket`: only while the bucket seeding took for its
/// directory is the live one. `current` keeps the answers of earlier files.
fn replays_into(
    transaction: &rusqlite::Transaction<'_>,
    current: &mut HashMap<String, bool>,
    bucket: &str,
) -> Result<bool, AdminError> {
    if let Some(replays) = current.get(bucket) {
        return Ok(*replays);
    }
    let replays = seed_records::mark_of(transaction, bucket)? == Mark::Current;
    current.insert(bucket.to_owned(), replays);
    Ok(replays)
}

/// Decides each file the pass read again with [`replay::live_read`], and keeps the record that
/// decision leaves, writing it only where it changed.
fn replay_live_files(
    transaction: &rusqlite::Transaction<'_>,
    batch: &SeedBatch,
    now: SystemTime,
    counts: &mut LiveCounts,
    problems: &mut Problems,
) -> Result<(), AdminError> {
    let mut current = HashMap::new();
    for SeedEntry {
        publication: entry,
        fingerprint,
    } in &batch.entries
    {
        let (bucket, name) = (entry.bucket.as_str(), entry.name.as_str());
        if !replays_into(transaction, &mut current, bucket)? {
            continue;
        }
        validate_staged_entry(entry)?;
        let live = objects::lookup(transaction, bucket, name)?
            .map(|stored| live_object(transaction, &stored.record, entry, now))
            .transpose()?;
        let file = staged_body(entry);
        let record = seed_records::get(transaction, bucket, name)?;
        let mut published = false;
        let kept = match replay::live_read(&file, record.as_ref(), live.as_ref()) {
            Reread::Refresh => record.map(|record| record.refreshed(*fingerprint)),
            Reread::Changed(Replay::Publish) => {
                let object = publish_entry(transaction, entry, &batch.admin.grant, now)?;
                published = true;
                counts.published += 1;
                Some(SeedRecord::new(
                    Some(version_of(&object)),
                    file,
                    *fingerprint,
                ))
            }
            Reread::Changed(Replay::Record { owned }) => {
                Some(SeedRecord::new(owned, file, *fingerprint))
            }
            Reread::Changed(Replay::Protected) => {
                problems.report(Some(&format!("{bucket}/{name}")), &PROTECTED_FILE);
                let owned = record.and_then(|record| record.owned);
                Some(SeedRecord::new(owned, file, *fingerprint))
            }
        };
        if let Some(kept) = kept
            && record != Some(kept)
        {
            seed_records::save(transaction, bucket, name, &kept)?;
            if !published {
                counts.recorded += 1;
            }
        }
    }
    Ok(())
}

/// Decides each name a complete walk, that of pass number `pass` at `now`, proved absent where
/// seeding owns a version: deletes it once a later proof confirms the first, keeps a tombstone
/// where an API write took the name, and reports a protected object.
fn replay_live_absences(
    transaction: &rusqlite::Transaction<'_>,
    batch: &SeedBatch,
    (pass, now): (u64, SystemTime),
    counts: &mut LiveCounts,
    problems: &mut Problems,
) -> Result<(), AdminError> {
    let access = batch.admin.grant.access();
    for ((bucket, name), proof) in &batch.absences {
        let Some(record) = seed_records::get(transaction, bucket, name)? else {
            continue;
        };
        let live = absent_object(transaction, bucket, name, now)?;
        let kept = match replay::absent(Phase::Live, proof, &record, live.as_ref()) {
            Vanished::Delete { owned } => match replay::confirm_absence(record.absent, pass, now) {
                // A refused deletion keeps the record, and the next pass decides again.
                Confirmation::Confirmed
                    if delete_owned(transaction, (bucket, name), owned, now, &access)? =>
                {
                    counts.deleted += 1;
                    deleted(bucket, name);
                    Some(record.after_deleting(owned))
                }
                Confirmation::Confirmed => None,
                Confirmation::Pending(absence) => {
                    counts.pending_deletions += 1;
                    Some(SeedRecord {
                        absent: Some(absence),
                        ..record
                    })
                }
            },
            Vanished::Tombstone => {
                counts.tombstones += 1;
                Some(record.tombstone())
            }
            Vanished::Protected => {
                problems.report(Some(&format!("{bucket}/{name}")), &PROTECTED_DELETION);
                None
            }
            Vanished::Forget => {
                seed_records::forget(transaction, bucket, name)?;
                None
            }
        };
        if let Some(kept) = kept
            && kept != record
        {
            seed_records::save(transaction, bucket, name, &kept)?;
        }
    }
    Ok(())
}
