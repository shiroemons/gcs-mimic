//! Group commit for single-request uploads.
//!
//! A commit holds the writer mostly for its WAL sync, while uploads that finish during that sync
//! wait for the writer anyway. They queue their commits instead, and the next holder of the
//! execution permit publishes the whole queue in one transaction, so one sync makes all of them
//! durable. Nothing waits for a batch to fill: a lone upload commits as soon as the writer is free.
//! An upload whose failure rolls back the whole transaction, as SQLite may after an I/O, full-disk
//! or out-of-memory error, would fail the others with it; since nothing was committed, each upload
//! of that batch is published again in its own transaction, and an upload fails only if its own
//! transaction does. A transient failure need not recur, and then no reply reports it, so the one
//! that rolled the batch back is logged as `upload_batch_retried` before the retry.

use super::*;
use crate::cause::BackendCause;
use crate::storage::failure::{StorageCause, wording::BackendFailureEvent};
use std::{
    collections::VecDeque,
    panic::AssertUnwindSafe,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::oneshot::error::TryRecvError;

/// The most uploads one transaction publishes, so a long queue cannot keep other catalog jobs
/// away from the writer for long.
const MAX_BATCH: usize = 64;

/// One upload's own result inside a batch that committed.
type UploadOutcome = Result<CommitOutcome, ObjectError>;

/// Why a batch did not answer each upload on its own.
enum BatchFailure {
    /// The transaction did not start, or its commit failed and may have published every upload.
    /// No upload caused it, and publishing one again could publish it twice, so every upload
    /// shares the failure and its cleanup checks for a reference.
    Shared(ObjectError),
    /// Publishing one upload rolled the transaction back before its commit, so nothing was
    /// published: SQLite rolled it back after the upload's error, or the upload's savepoint could
    /// not be ended and the transaction rolls back when it is dropped.
    RolledBack(ObjectError),
}

/// The fields of an `upload_batch_retried` event.
#[derive(serde::Serialize)]
struct BatchRetried {
    /// How many uploads are published again, each in its own transaction.
    uploads: usize,
    /// The failure that rolled the batch's transaction back.
    #[serde(flatten)]
    failure: BackendFailureEvent,
}

/// Uploads whose blobs are durable and whose commits wait for the writer.
#[derive(Default)]
pub(in crate::storage::catalog) struct CommitQueue {
    queued: Mutex<VecDeque<Queued>>,
    next_ticket: AtomicU64,
}

/// Identifies one queued commit, so its upload can withdraw it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Ticket(u64);

struct Queued {
    ticket: Ticket,
    commit: UploadCommit,
    reply: oneshot::Sender<UploadOutcome>,
}

impl CommitQueue {
    fn queued(&self) -> MutexGuard<'_, VecDeque<Queued>> {
        // Nothing under the lock can leave the queue half changed, so a poisoned one is valid.
        crate::sync::lock(&self.queued)
    }

    fn push(&self, commit: UploadCommit, reply: oneshot::Sender<UploadOutcome>) -> Ticket {
        let ticket = Ticket(self.next_ticket.fetch_add(1, Ordering::Relaxed));
        self.queued().push_back(Queued {
            ticket,
            commit,
            reply,
        });
        ticket
    }

    fn take_batch(&self) -> Vec<Queued> {
        let mut queued = self.queued();
        let batch = queued.len().min(MAX_BATCH);
        queued.drain(..batch).collect()
    }

    /// Removes the commit unless a batch already took it, so a failed upload cannot publish
    /// after its cleanup removed the blob.
    fn withdraw(&self, ticket: Ticket) -> bool {
        let mut queued = self.queued();
        queued
            .iter()
            .position(|queued| queued.ticket == ticket)
            .and_then(|position| queued.remove(position))
            .is_some()
    }
}

impl Catalog {
    /// Publishes `commit` with every upload queued by the time a writer permit comes up. Each
    /// upload still takes the permit in turn; one whose turn comes after a batch that published
    /// it only collects the answer.
    pub(super) async fn commit_upload(&self, commit: UploadCommit) -> UploadOutcome {
        let (reply, mut outcome) = oneshot::channel();
        let ticket = self.0.writer.upload_commits.push(commit, reply);
        match self.await_answer(&mut outcome).await {
            Ok(answer) => answer,
            Err(error) => self.abandon(ticket, outcome, error).await,
        }
    }

    /// Takes the writer in turn until a batch has answered. `Err` means the commit may still be
    /// queued.
    async fn await_answer(
        &self,
        outcome: &mut oneshot::Receiver<UploadOutcome>,
    ) -> Result<UploadOutcome, ObjectError> {
        loop {
            let execution = self
                .0
                .writer
                .execution
                .clone()
                .acquire_owned()
                .await
                .map_err(object_unavailable)?;
            // A batch answers before it releases the permit, so the commit is now either
            // answered or still queued.
            match outcome.try_recv() {
                Ok(answer) => return Ok(answer),
                // The batch that took the commit ended without answering, so it is no longer
                // queued; the upload's cleanup checks whether it was committed.
                Err(TryRecvError::Closed) => {
                    return Ok(Err(object_unavailable(
                        "upload commit ended without an answer",
                    )));
                }
                Err(TryRecvError::Empty) => self.commit_batch(execution).await?,
            }
        }
    }

    /// Fails the upload only once it can no longer publish: a commit still queued is withdrawn,
    /// and one that a batch took gets that batch's answer.
    async fn abandon(
        &self,
        ticket: Ticket,
        outcome: oneshot::Receiver<UploadOutcome>,
        error: ObjectError,
    ) -> UploadOutcome {
        if self.0.writer.upload_commits.withdraw(ticket) {
            return Err(error);
        }
        outcome.await.unwrap_or(Err(error))
    }

    /// Publishes the queued commits while `execution` keeps every other catalog job out, and
    /// answers them once the commit and its read cache invalidation are done.
    async fn commit_batch(&self, execution: OwnedSemaphorePermit) -> Result<(), ObjectError> {
        let batch = self.0.writer.upload_commits.take_batch();
        let inner = self.0.clone();
        self.0
            .writer
            .executor
            .run(move || {
                let _execution = execution;
                let (commits, replies): (Vec<_>, Vec<_>) = batch
                    .into_iter()
                    .map(|queued| (queued.commit, queued.reply))
                    .unzip();
                let outcomes =
                    match inner.with_connection(|connection| publish_batch(connection, &commits)) {
                        Ok(Ok(outcomes)) => outcomes,
                        Ok(Err(failure)) => shared(failure, commits.len()),
                        Err(connection) => shared(connection.into(), commits.len()),
                    };
                for (reply, outcome) in replies.into_iter().zip(outcomes) {
                    // An upload that stopped waiting cleans up by checking references instead.
                    let _ = reply.send(outcome);
                }
            })
            .await
            .map_err(object_unavailable)
    }
}

/// Answers every commit of the batch. `Err` is a failure they all share.
fn publish_batch(
    connection: &mut Connection,
    commits: &[UploadCommit],
) -> Result<Vec<UploadOutcome>, ObjectError> {
    match publish_all(connection, commits) {
        Ok(outcomes) => Ok(outcomes),
        Err(BatchFailure::Shared(failure)) => Err(failure),
        Err(BatchFailure::RolledBack(failure)) if commits.len() == 1 => Ok(vec![Err(failure)]),
        Err(BatchFailure::RolledBack(failure)) => {
            // Nothing was published. A transient failure need not recur when its upload is
            // published again, and then no reply reports it, so it is logged here.
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::UploadBatchRetried,
                BatchRetried {
                    uploads: commits.len(),
                    failure: BackendFailureEvent::from(&BackendCause::from_error(failure)),
                },
            );
            Ok(publish_each(connection, commits))
        }
    }
}

/// Publishes each commit in its own transaction, in queue order, so each precondition sees the
/// uploads published before it as it would have in the batch. A failure that no upload caused
/// ends the attempt, and the commits not yet tried share it.
fn publish_each(connection: &mut Connection, commits: &[UploadCommit]) -> Vec<UploadOutcome> {
    let mut outcomes = Vec::with_capacity(commits.len());
    for commit in commits {
        match publish_all(connection, std::slice::from_ref(commit)) {
            Ok(published) => outcomes.extend(published),
            Err(BatchFailure::RolledBack(failure)) => outcomes.push(Err(failure)),
            Err(BatchFailure::Shared(failure)) => {
                outcomes.extend(shared(failure, commits.len() - outcomes.len()));
                break;
            }
        }
    }
    outcomes
}

/// Publishes each commit under its own savepoint, so one upload's error or panic undoes only its
/// own changes, then commits them together.
fn publish_all(
    connection: &mut Connection,
    commits: &[UploadCommit],
) -> Result<Vec<UploadOutcome>, BatchFailure> {
    let transaction =
        sql::immediate(connection).map_err(|error| BatchFailure::Shared(error.into()))?;
    let outcomes = commits
        .iter()
        .map(|commit| publish_isolated(&transaction, commit))
        .collect::<Result<Vec<_>, _>>()
        .map_err(BatchFailure::RolledBack)?;
    commit_publication(transaction).map_err(BatchFailure::Shared)?;
    Ok(outcomes)
}

/// Returns the upload's own outcome, or the error that rolls the transaction back: its error made
/// SQLite roll back the whole transaction, or its savepoint could not be ended and the transaction
/// rolls back when it is dropped.
fn publish_isolated(
    transaction: &sql::WriteTx<'_>,
    commit: &UploadCommit,
) -> Result<UploadOutcome, ObjectError> {
    sql::exec(transaction, "SAVEPOINT upload", [])?;
    // A panicking upload's changes are rolled back to its savepoint below, so nothing it left
    // half done is committed or read afterwards.
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| commit.apply(transaction)))
        .unwrap_or_else(|_| Err(super::super::super::ConnectionUnavailable::Panicked.into()));
    match outcome {
        Ok(published) => {
            sql::exec(transaction, "RELEASE upload", [])?;
            Ok(Ok(published))
        }
        // Some errors, such as a full disk, roll back the whole transaction in SQLite.
        Err(error) if transaction.is_autocommit() => Err(error),
        Err(error) => {
            sql::exec(transaction, "ROLLBACK TO upload", [])?;
            sql::exec(transaction, "RELEASE upload", [])?;
            Ok(Err(error))
        }
    }
}

/// Gives every upload of a failed batch the batch's failure.
fn shared(failure: ObjectError, uploads: usize) -> Vec<UploadOutcome> {
    // Storage failures keep their own cause; anything else is wrapped as unavailable.
    std::iter::repeat_n(failure.classify(), uploads)
        .map(|failure| Err(failure.into()))
        .collect()
}

#[cfg(test)]
mod tests;
