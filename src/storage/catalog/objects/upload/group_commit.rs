//! Group commit for single-request uploads.
//!
//! A commit holds the writer mostly for its WAL sync, while uploads that finish during that sync
//! wait for the writer anyway. They queue their commits instead, and the next holder of the
//! execution permit publishes the whole queue in one transaction, so one sync makes all of them
//! durable. Nothing waits for a batch to fill: a lone upload commits as soon as the writer is free.

use super::*;
use crate::storage::failure::{Classified, StorageCause};
use std::{
    collections::VecDeque,
    panic::AssertUnwindSafe,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::oneshot::error::TryRecvError;

/// The most uploads one transaction publishes, so a long queue cannot keep other catalog jobs
/// away from the writer for long.
const MAX_BATCH: usize = 64;

type Outcome = Result<CommitOutcome, ObjectError>;

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
    reply: oneshot::Sender<Outcome>,
}

impl CommitQueue {
    fn queued(&self) -> MutexGuard<'_, VecDeque<Queued>> {
        // Nothing under the lock can leave the queue half changed, so a poisoned one is valid.
        self.queued.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, commit: UploadCommit, reply: oneshot::Sender<Outcome>) -> Ticket {
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
    pub(super) async fn commit_upload(&self, commit: UploadCommit) -> Outcome {
        let (reply, mut outcome) = oneshot::channel();
        let ticket = self.0.upload_commits.push(commit, reply);
        match self.await_answer(&mut outcome).await {
            Ok(answer) => answer,
            Err(error) => self.abandon(ticket, outcome, error).await,
        }
    }

    /// Takes the writer in turn until a batch has answered. `Err` means the commit may still be
    /// queued.
    async fn await_answer(
        &self,
        outcome: &mut oneshot::Receiver<Outcome>,
    ) -> Result<Outcome, ObjectError> {
        loop {
            let execution = self
                .0
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
        outcome: oneshot::Receiver<Outcome>,
        error: ObjectError,
    ) -> Outcome {
        if self.0.upload_commits.withdraw(ticket) {
            return Err(error);
        }
        outcome.await.unwrap_or(Err(error))
    }

    /// Publishes the queued commits while `execution` keeps every other catalog job out, and
    /// answers them once the commit and its read cache invalidation are done.
    async fn commit_batch(&self, execution: OwnedSemaphorePermit) -> Result<(), ObjectError> {
        let batch = self.0.upload_commits.take_batch();
        let inner = self.0.clone();
        self.0
            .executor
            .run(move || {
                let _execution = execution;
                let (commits, replies): (Vec<_>, Vec<_>) = batch
                    .into_iter()
                    .map(|queued| (queued.commit, queued.reply))
                    .unzip();
                let outcomes =
                    match inner.with_connection(|connection| publish_all(connection, &commits)) {
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

/// Publishes each commit under its own savepoint, so one upload's error or panic undoes only its
/// own changes, then commits them together. `Err` fails the whole batch: either nothing was
/// committed, or the outcome is unknown and each upload's cleanup checks for a reference.
fn publish_all(
    connection: &mut Connection,
    commits: &[UploadCommit],
) -> Result<Vec<Outcome>, ObjectError> {
    let transaction = sql::immediate(connection)?;
    let mut outcomes = Vec::with_capacity(commits.len());
    for commit in commits {
        outcomes.push(publish_isolated(&transaction, commit)?);
    }
    commit_publication(transaction)?;
    Ok(outcomes)
}

/// Returns the upload's own outcome, or `Err` when the transaction cannot go on: its savepoint
/// could not be ended, or its error made SQLite roll back the whole transaction.
fn publish_isolated(
    transaction: &Transaction<'_>,
    commit: &UploadCommit,
) -> Result<Outcome, ObjectError> {
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
fn shared(failure: ObjectError, uploads: usize) -> Vec<Outcome> {
    // Storage failures keep their own cause; anything else is wrapped as unavailable.
    let failure = match failure {
        ObjectError::Corrupt(cause) => Classified::Corrupt(cause),
        ObjectError::Io(cause) => Classified::Io(cause),
        ObjectError::Unavailable(cause) => Classified::Unavailable(cause),
        failure => failure.classify(),
    };
    std::iter::repeat_n(failure, uploads)
        .map(|failure| Err(failure.into()))
        .collect()
}

#[cfg(test)]
mod tests;
