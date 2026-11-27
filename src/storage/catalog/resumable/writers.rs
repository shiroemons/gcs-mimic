use super::{Catalog, CatalogError, Work, unavailable};
use crate::cause::BackendFailure;
use crate::storage::catalog::Activity;
use crate::storage::{blob_id::BlobId, catalog::session_limits::WriterWait};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::MutexGuard,
};

/// Typed namespaces keep unrelated transfer and upload IDs independent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::storage::catalog) enum WriterKey {
    Resumable(super::SessionId),
    /// A whole XML multipart upload, exclusive of every part of it.
    Multipart(String),
    /// A client abort of an XML multipart upload. It excludes the whole-upload holders and new
    /// parts, but not the parts already writing: Cloud Storage fails parts in flight when their
    /// upload is aborted, and here they fail at their commit, which finds the upload gone.
    MultipartAbort(String),
    /// One part of an XML multipart upload, exclusive of its own number and the whole upload.
    MultipartPart(String, u16),
    Rewrite(BlobId),
    Derived(BlobId),
}

/// A sweep that skips a busy writer must retry when its key becomes available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum KeyOrigin {
    Holder,
    Sweep(Deferred),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::storage::catalog) enum Deferred {
    Maintenance,
    Lifecycle,
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum ClaimError {
    Busy,
    Poisoned,
    Catalog(CatalogError),
}

// A catalog refusal displays and exposes the same CatalogError. Retain that identity
// until the diagnostic contract is revisited.
impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("session writer is busy"),
            Self::Poisoned => f.write_str("session writer registry is poisoned"),
            Self::Catalog(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ClaimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Catalog(error) => Some(error),
            Self::Busy | Self::Poisoned => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn claimed_catalog_source<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a dyn std::error::Error> {
    match error.downcast_ref() {
        Some(ClaimError::Catalog(cause)) => Some(cause),
        _ => None,
    }
}

impl From<ClaimError> for CatalogError {
    fn from(error: ClaimError) -> Self {
        match error {
            ClaimError::Busy => unavailable(BackendFailure::SessionBusy),
            ClaimError::Poisoned => unavailable("session writer registry is poisoned"),
            ClaimError::Catalog(error) => error,
        }
    }
}

/// Who writes one XML multipart upload right now.
#[derive(Debug)]
enum UploadHolders {
    /// Bodies of these part numbers are streaming. Each number has its own row and staging file,
    /// so different numbers write side by side, as Cloud Storage lets clients do.
    Parts(HashSet<u16>),
    /// Completion, and retirement by maintenance, lifecycle or a bucket purge, read or retire
    /// every part, so they need the upload to themselves.
    Whole,
    /// A client abort retires the upload while the bodies of these part numbers keep streaming.
    /// Each fails at its commit, which rechecks the upload in the same transaction.
    Aborting(HashSet<u16>),
}

impl Default for UploadHolders {
    fn default() -> Self {
        Self::Parts(HashSet::new())
    }
}

#[derive(Debug, Default)]
struct UploadWriters {
    holders: UploadHolders,
    /// Whole-upload requests in line. Any of them keeps new parts out, so steady part traffic
    /// cannot postpone a completion past its wait budget.
    whole_waiting: usize,
}

impl UploadWriters {
    fn claim_part(&mut self, number: u16) -> bool {
        match &mut self.holders {
            UploadHolders::Parts(writing) if self.whole_waiting == 0 => writing.insert(number),
            UploadHolders::Parts(_) | UploadHolders::Whole | UploadHolders::Aborting(_) => false,
        }
    }

    fn claim_whole(&mut self) -> bool {
        match &self.holders {
            UploadHolders::Parts(writing) if writing.is_empty() => {
                self.holders = UploadHolders::Whole;
                true
            }
            UploadHolders::Parts(_) | UploadHolders::Whole | UploadHolders::Aborting(_) => false,
        }
    }

    /// An abort takes the upload from the parts in flight without waiting for them, and hands
    /// them back when it ends.
    fn claim_abort(&mut self) -> bool {
        match &mut self.holders {
            UploadHolders::Parts(writing) => {
                self.holders = UploadHolders::Aborting(std::mem::take(writing));
                true
            }
            UploadHolders::Whole | UploadHolders::Aborting(_) => false,
        }
    }

    fn release_abort(&mut self) {
        if let UploadHolders::Aborting(writing) = &mut self.holders {
            self.holders = UploadHolders::Parts(std::mem::take(writing));
        }
    }

    fn release_part(&mut self, number: u16) {
        match &mut self.holders {
            UploadHolders::Parts(writing) | UploadHolders::Aborting(writing) => {
                writing.remove(&number);
            }
            UploadHolders::Whole => {}
        }
    }

    fn is_idle(&self) -> bool {
        self.whole_waiting == 0
            && matches!(&self.holders, UploadHolders::Parts(writing) if writing.is_empty())
    }
}

#[derive(Default)]
pub(in crate::storage::catalog) struct SessionWriters {
    held: HashSet<WriterKey>,
    deferred: HashMap<WriterKey, Deferred>,
    /// Only uploads someone holds or waits for, so its size follows the requests in flight.
    uploads: HashMap<String, UploadWriters>,
}

impl SessionWriters {
    #[cfg(test)]
    pub(in crate::storage::catalog) fn is_empty(&self) -> bool {
        self.held.is_empty() && self.uploads.is_empty() && self.deferred.is_empty()
    }

    fn upload(&mut self, id: &str) -> &mut UploadWriters {
        self.uploads.entry(id.to_owned()).or_default()
    }

    fn claim(&mut self, key: &WriterKey) -> Result<(), ClaimError> {
        let claimed = match key {
            WriterKey::Multipart(id) => self.upload(id).claim_whole(),
            WriterKey::MultipartAbort(id) => self.upload(id).claim_abort(),
            WriterKey::MultipartPart(id, number) => self.upload(id).claim_part(*number),
            WriterKey::Resumable(_) | WriterKey::Rewrite(_) | WriterKey::Derived(_) => {
                self.held.insert(key.clone())
            }
        };
        if claimed {
            Ok(())
        } else {
            Err(ClaimError::Busy)
        }
    }

    fn claim_from(&mut self, key: &WriterKey, origin: KeyOrigin) -> Result<(), ClaimError> {
        let result = self.claim(key);
        if let (Err(ClaimError::Busy), KeyOrigin::Sweep(retry)) = (&result, origin) {
            // Mark under the claim lock; the last holder cannot release between failure and arming.
            let key = match key {
                WriterKey::MultipartAbort(id) | WriterKey::MultipartPart(id, _) => {
                    WriterKey::Multipart(id.clone())
                }
                WriterKey::Multipart(_)
                | WriterKey::Derived(_)
                | WriterKey::Rewrite(_)
                | WriterKey::Resumable(_) => key.clone(),
            };
            let waiting = self.deferred.entry(key).or_insert(retry);
            *waiting = (*waiting).max(retry);
        }
        result
    }

    fn take_deferred(&mut self, key: &WriterKey) -> Option<Deferred> {
        if self.deferred.is_empty() {
            return None;
        }
        match key {
            WriterKey::Multipart(id)
            | WriterKey::MultipartAbort(id)
            | WriterKey::MultipartPart(id, _) => {
                if self.uploads.get(id).is_some_and(|upload| {
                    !matches!(&upload.holders, UploadHolders::Parts(parts) if parts.is_empty())
                }) {
                    return None;
                }
                self.deferred.remove(&WriterKey::Multipart(id.clone()))
            }
            WriterKey::Derived(_) | WriterKey::Resumable(_) | WriterKey::Rewrite(_) => {
                self.deferred.remove(key)
            }
        }
    }

    fn release(&mut self, key: &WriterKey) {
        match key {
            WriterKey::Multipart(id) => {
                self.settle_upload(id, |upload| upload.holders = UploadHolders::default());
            }
            WriterKey::MultipartAbort(id) => self.settle_upload(id, UploadWriters::release_abort),
            WriterKey::MultipartPart(id, number) => {
                self.settle_upload(id, |upload| upload.release_part(*number));
            }
            WriterKey::Resumable(_) | WriterKey::Rewrite(_) | WriterKey::Derived(_) => {
                self.held.remove(key);
            }
        }
    }

    /// Changes one upload's state and forgets the upload once nobody holds or waits for it.
    fn settle_upload(&mut self, id: &str, change: impl FnOnce(&mut UploadWriters)) {
        if let Some(upload) = self.uploads.get_mut(id) {
            change(upload);
            if upload.is_idle() {
                self.uploads.remove(id);
            }
        }
    }
}

/// A whole-upload request's place in line, given up when the request gets the upload, runs out of
/// budget or is cancelled. Parts it held back then try again, so it wakes them when it drops.
#[must_use = "dropping the guard gives up the whole-upload request's place in line"]
struct WholeUploadTurn<'a> {
    catalog: &'a Catalog,
    id: &'a str,
}

impl Drop for WholeUploadTurn<'_> {
    fn drop(&mut self) {
        self.catalog
            .0
            .writers
            .registry_for_release()
            .settle_upload(self.id, |upload| upload.whole_waiting -= 1);
        self.catalog.0.writers.released.notify_waiters();
    }
}

#[must_use = "dropping the guard releases the session writer"]
pub(in crate::storage::catalog) struct WriterGuard {
    pub(super) catalog: Catalog,
    key: WriterKey,
    _activity: Activity,
}

impl WriterGuard {
    pub(in crate::storage::catalog) fn multipart_id(&self) -> Option<&str> {
        match &self.key {
            WriterKey::Multipart(id) => Some(id),
            // Parts may still be writing under an abort, so it is not a whole-upload lease.
            WriterKey::Resumable(_)
            | WriterKey::MultipartAbort(_)
            | WriterKey::MultipartPart(..)
            | WriterKey::Rewrite(_)
            | WriterKey::Derived(_) => None,
        }
    }
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        let retry = {
            let mut writers = self.catalog.0.writers.registry_for_release();
            writers.release(&self.key);
            writers.take_deferred(&self.key)
        };
        self.catalog.0.writers.released.notify_waiters();
        if matches!(self.key, WriterKey::Derived(_)) {
            self.catalog.0.maintenance.derived_wake.notify_waiters();
        }
        if let Some(retry) = retry {
            let wake = &self.catalog.0.maintenance.wake;
            if retry == Deferred::Lifecycle {
                self.catalog.0.maintenance.lifecycle.mark_dirty();
            }
            wake.notify_one();
        }
    }
}

impl super::super::state::WriterRegistry {
    /// The registry for a new claim. A poisoned registry refuses claims, and `is_healthy` reports
    /// it.
    fn registry_for_claim(&self) -> Result<MutexGuard<'_, SessionWriters>, ClaimError> {
        crate::sync::lock_checked(&self.sessions).map_err(|_| ClaimError::Poisoned)
    }

    /// The registry for a release, which runs in `Drop` and cannot fail: the holder's own entry is
    /// still valid after another holder panicked.
    fn registry_for_release(&self) -> MutexGuard<'_, SessionWriters> {
        crate::sync::lock_for_cleanup(&self.sessions)
    }
}

impl Catalog {
    /// Whether any session or transfer writer is held right now.
    #[cfg(test)]
    pub(crate) fn has_session_writers(&self) -> bool {
        !self.0.writers.sessions.lock().unwrap().is_empty()
    }

    /// How many part numbers of an XML multipart upload are writing right now.
    #[cfg(test)]
    pub(crate) fn multipart_parts_writing(&self, id: &str) -> usize {
        match self.0.writers.sessions.lock().unwrap().uploads.get(id) {
            Some(UploadWriters {
                holders: UploadHolders::Parts(writing) | UploadHolders::Aborting(writing),
                ..
            }) => writing.len(),
            Some(_) | None => 0,
        }
    }

    /// How many whole-upload requests are in line for an XML multipart upload.
    #[cfg(test)]
    pub(crate) fn multipart_whole_waiters(&self, id: &str) -> usize {
        self.0
            .writers
            .sessions
            .lock()
            .unwrap()
            .uploads
            .get(id)
            .map_or(0, |upload| upload.whole_waiting)
    }

    /// Claims a writer now or fails with `Busy`. Maintenance and session trimming use this form:
    /// they skip a held session and come back later, so they never wait behind a client.
    pub(in crate::storage::catalog) fn claim_writer(
        &self,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        // Only a running exclusive job refuses this lease, and every such job wakes maintenance
        // when it ends or its next pass comes round, so this is one more thing to skip for now.
        let activity = self.activity().map_err(|_| ClaimError::Busy)?;
        self.claim_leased(activity, key, origin)
    }

    fn claim_leased(
        &self,
        activity: Activity,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        self.0
            .writers
            .registry_for_claim()?
            .claim_from(&key, origin)?;
        Ok(WriterGuard {
            catalog: self.clone(),
            key,
            _activity: activity,
        })
    }

    /// Only registry keys can be marked deferred. Wait out an exclusive job before trying the key.
    pub(in crate::storage::catalog) async fn claim_sweep_writer(
        &self,
        key: WriterKey,
    ) -> Result<WriterGuard, ClaimError> {
        let activity = self.client_activity(Work::UnderWay).await?;
        self.claim_leased(activity, key, KeyOrigin::Sweep(Deferred::Maintenance))
    }

    /// The lease for a client request's writer, once any exclusive job is done. `work` says
    /// whether the caller may line up behind a job still draining the catalog.
    async fn client_activity(&self, work: Work) -> Result<Activity, ClaimError> {
        self.await_activity(work)
            .await
            .map_err(|closed| ClaimError::Catalog(closed.into()))
    }

    /// Claims a writer for a client request that holds no lease yet, waiting up to the configured
    /// writer wait for the current holder to finish. A client that gave up on a stalled request
    /// retries while the old handler is still unwinding; the retry should take over rather than
    /// fail for a state that clears by itself. Past the budget the holder is really working, so
    /// it stays `Busy`.
    pub(in crate::storage::catalog) async fn wait_for_writer(
        &self,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        self.wait_to_claim(Work::New, key, origin).await
    }

    /// As [`Self::wait_for_writer`], for a request that already holds a lease. Lining up behind a
    /// job draining the catalog could then wait for that very lease.
    pub(in crate::storage::catalog) async fn wait_for_writer_under_way(
        &self,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        self.wait_to_claim(Work::UnderWay, key, origin).await
    }

    async fn wait_to_claim(
        &self,
        work: Work,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        match self.session_limits().writer_wait {
            WriterWait::Fail => self.claim_leased(self.client_activity(work).await?, key, origin),
            WriterWait::Within(limit) => {
                tokio::time::timeout_at(limit.deadline(), self.claim_released(work, key, origin))
                    .await
                    .unwrap_or(Err(ClaimError::Busy))
            }
        }
    }

    /// Claims a whole XML multipart upload for a client request. Parts already writing finish
    /// first, within the same budget as `wait_for_writer`, and parts arriving meanwhile wait
    /// behind this request.
    pub(in crate::storage::catalog) async fn wait_for_whole_upload(
        &self,
        id: &str,
    ) -> Result<WriterGuard, ClaimError> {
        let _turn = self.take_whole_upload_turn(id)?;
        self.wait_for_writer(WriterKey::Multipart(id.to_owned()), KeyOrigin::Holder)
            .await
    }

    fn take_whole_upload_turn<'a>(
        &'a self,
        id: &'a str,
    ) -> Result<WholeUploadTurn<'a>, ClaimError> {
        self.0
            .writers
            .registry_for_claim()?
            .upload(id)
            .whole_waiting += 1;
        Ok(WholeUploadTurn { catalog: self, id })
    }

    async fn claim_released(
        &self,
        work: Work,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        loop {
            let released = self.0.writers.released.notified();
            tokio::pin!(released);
            // Register before claiming so a concurrent release cannot be missed.
            released.as_mut().enable();
            let activity = self.client_activity(work).await?;
            match self.claim_leased(activity, key.clone(), origin) {
                Ok(guard) => return Ok(guard),
                Err(ClaimError::Busy) if self.is_closing() => {
                    return Err(ClaimError::Catalog(unavailable(
                        BackendFailure::CatalogClosed,
                    )));
                }
                Err(ClaimError::Busy) => tokio::select! {
                    () = released => {}
                    () = self.0.shutdown.token.cancelled() => {}
                },
                Err(error @ (ClaimError::Poisoned | ClaimError::Catalog(_))) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectKey;
    use crate::{
        bucket::BucketAccess,
        cause::BackendFailure,
        clock::SystemClock,
        storage::{
            ResumableOptions,
            admin::LocalAdmin,
            catalog::{ResumableError, resumable::SessionLease},
            test_fixtures::bucket,
        },
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn only_the_last_holder_of_the_deferred_upload_wakes_maintenance() {
        let (catalog, _) = open(0).await;
        let hold = |key| catalog.claim_writer(key, KeyOrigin::Holder).unwrap();
        let first = hold(WriterKey::MultipartPart("busy".into(), 1));
        let last = hold(WriterKey::MultipartPart("busy".into(), 2));
        let unrelated = hold(WriterKey::Multipart("other".into()));
        catalog.0.maintenance.wake.notified().await;
        let wake = catalog.0.maintenance.wake.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        assert!(matches!(
            catalog.claim_writer(
                WriterKey::Multipart("busy".into()),
                KeyOrigin::Sweep(Deferred::Maintenance)
            ),
            Err(ClaimError::Busy)
        ));
        drop(unrelated);
        drop(first);
        assert!(!wake.as_mut().enable());
        drop(last);
        assert!(wake.as_mut().enable());
        assert!(catalog.0.writers.sessions.lock().unwrap().is_empty());
        catalog.close().await.unwrap();
    }

    #[test]
    fn deferred_part_and_abort_retry_lifecycle_when_the_upload_is_released() {
        let mut writers = SessionWriters::default();
        let whole = WriterKey::Multipart("upload".into());
        writers.claim(&whole).unwrap();
        for (key, retry) in [
            (
                WriterKey::MultipartPart("upload".into(), 1),
                Deferred::Lifecycle,
            ),
            (
                WriterKey::MultipartAbort("upload".into()),
                Deferred::Maintenance,
            ),
        ] {
            assert_eq!(
                writers.claim_from(&key, KeyOrigin::Sweep(retry)),
                Err(ClaimError::Busy)
            );
        }
        writers.release(&whole);
        assert_eq!(writers.take_deferred(&whole), Some(Deferred::Lifecycle));
        assert!(writers.is_empty());
    }

    #[test]
    fn namespaces_keep_equal_ids_apart_and_a_held_key_is_busy_until_released() {
        let mut writers = SessionWriters::default();
        let id = BlobId::generate().unwrap();
        writers.claim(&WriterKey::Derived(id)).unwrap();
        writers.claim(&WriterKey::Rewrite(id)).unwrap();
        let session = WriterKey::Resumable(format!("{id}{id}").parse().unwrap());
        writers.claim(&session).unwrap();
        writers.claim(&WriterKey::Multipart(id.to_hex())).unwrap();
        assert_eq!(writers.claim(&session), Err(ClaimError::Busy));
        writers.release(&session);
        writers.claim(&session).unwrap();
    }

    #[test]
    fn an_abort_takes_an_upload_from_its_parts_in_flight_and_holds_back_everything_else() {
        let mut writers = SessionWriters::default();
        let id = "upload".to_owned();
        let (whole, abort) = (
            WriterKey::Multipart(id.clone()),
            WriterKey::MultipartAbort(id.clone()),
        );
        let part = |number| WriterKey::MultipartPart(id.clone(), number);
        writers.claim(&part(1)).unwrap();
        writers.claim(&abort).unwrap();
        for contender in [&abort, &whole, &part(1), &part(2)] {
            assert_eq!(
                writers.claim(contender),
                Err(ClaimError::Busy),
                "{contender:?}"
            );
        }
        // The part in flight keeps its number after the abort, and the upload until it ends.
        writers.release(&abort);
        assert_eq!(writers.claim(&part(1)), Err(ClaimError::Busy));
        assert_eq!(writers.claim(&whole), Err(ClaimError::Busy));
        writers.claim(&part(2)).unwrap();
        writers.release(&part(1));
        writers.release(&part(2));
        assert!(writers.is_empty());

        // A part that ends during the abort leaves nothing behind once the abort ends.
        writers.claim(&part(1)).unwrap();
        writers.claim(&abort).unwrap();
        writers.release(&part(1));
        writers.release(&abort);
        assert!(writers.is_empty());

        // Completion and catalog retirement hold an abort back.
        writers.claim(&whole).unwrap();
        assert_eq!(writers.claim(&abort), Err(ClaimError::Busy));
        writers.release(&whole);
        writers.claim(&abort).unwrap();
        writers.release(&abort);
        assert!(writers.is_empty());
    }

    async fn open(wait_ms: u64) -> (Catalog, String) {
        let config = crate::config::ConfigDraft {
            catalog_wait_timeout: crate::time_limit::TimeLimit::from_millis(wait_ms),
            ..Default::default()
        }
        .build();
        let catalog = Catalog::open_server_with_limits(
            crate::storage::CatalogBacking::Memory {
                blob_bytes: 1024,
                catalog_bytes: 1024 * 1024,
            },
            Arc::new(SystemClock),
            LocalAdmin::Disabled,
            Some(&config),
        )
        .await
        .unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        let session = catalog
            .create_resumable_session(
                ObjectKey::from_parts_unchecked("bucket", "object"),
                ResumableOptions::new(BucketAccess::Permissive),
                None,
            )
            .await
            .unwrap();
        (catalog, session.id.to_string())
    }

    fn spawn_lock(
        catalog: &Catalog,
        id: &str,
    ) -> tokio::task::JoinHandle<Result<SessionLease, ResumableError>> {
        let (catalog, id) = (catalog.clone(), id.to_owned());
        tokio::spawn(async move { catalog.lock_resumable_session(&id, "bucket").await })
    }

    #[tokio::test]
    async fn a_waiting_request_takes_over_when_the_holder_releases() {
        let (catalog, id) = open(60_000).await;
        let lease = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
        let waiting = spawn_lock(&catalog, &id);
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        assert!(!waiting.is_finished());
        drop(lease);
        drop(waiting.await.unwrap().unwrap());
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn every_waiter_gets_a_turn_when_holders_release_in_quick_succession() {
        let (catalog, id) = open(60_000).await;
        let mut waiters = tokio::task::JoinSet::new();
        for _ in 0..64 {
            let (catalog, id) = (catalog.clone(), id.clone());
            waiters.spawn(async move {
                let lease = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
                tokio::task::yield_now().await;
                drop(lease);
            });
        }
        while let Some(result) = waiters.join_next().await {
            result.unwrap();
        }
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_wait_past_its_budget_reports_a_busy_session() {
        let (catalog, id) = open(50).await;
        let lease = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
        let started = tokio::time::Instant::now();
        let Err(ResumableError::Object(crate::object::ObjectError::Unavailable(cause))) =
            catalog.lock_resumable_session(&id, "bucket").await
        else {
            panic!("the holder keeps the session busy");
        };
        assert_eq!(cause.failure(), Some(BackendFailure::SessionBusy));
        assert!(started.elapsed() >= std::time::Duration::from_millis(50));
        drop(lease);
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_dropped_wait_leaves_the_session_to_the_next_request() {
        let (catalog, id) = open(60_000).await;
        let lease = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
        let waiting = spawn_lock(&catalog, &id);
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        waiting.abort();
        assert!(waiting.await.is_err_and(|error| error.is_cancelled()));
        drop(lease);
        drop(catalog.lock_resumable_session(&id, "bucket").await.unwrap());
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn closing_wakes_a_waiting_request_with_the_closed_failure() {
        let (catalog, id) = open(60_000).await;
        let lease = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
        let waiting = spawn_lock(&catalog, &id);
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        assert!(!waiting.is_finished());
        catalog.close().await.unwrap();
        let Err(ResumableError::Object(crate::object::ObjectError::Unavailable(cause))) =
            waiting.await.unwrap()
        else {
            panic!("a closed catalog grants no session");
        };
        assert_eq!(cause.failure(), Some(BackendFailure::CatalogClosed));
        drop(lease);
    }

    #[tokio::test]
    async fn maintenance_style_claims_never_wait() {
        let (catalog, id) = open(60_000).await;
        let lease = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
        assert!(matches!(
            catalog.claim_writer(WriterKey::Resumable(id.parse().unwrap()), KeyOrigin::Holder),
            Err(ClaimError::Busy)
        ));
        drop(lease);
        catalog.close().await.unwrap();
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = ClaimError::Catalog(CatalogError::NotFound);
    assert_eq!(error.to_string(), "bucket not found");
    assert_eq!(
        crate::error::Report::chain(&error).to_string(),
        error.to_string()
    );
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<CatalogError>()
    );
}
