use super::{Catalog, CatalogError, Work, unavailable};
use crate::storage::catalog::Activity;
use crate::storage::{
    blob_id::BlobId, catalog::session_limits::WriterWait, failure::BackendFailure,
};
use std::{collections::HashSet, fmt};

/// Typed namespaces keep unrelated transfer and upload IDs independent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::storage::catalog) enum WriterKey {
    Resumable(String),
    Multipart(String),
    Rewrite(BlobId),
    Derived(BlobId),
}

/// Who holds a writer. Releasing one a request held rearms the maintenance work it made skip;
/// maintenance releasing its own would wake itself in a loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum KeyOrigin {
    Client,
    Catalog,
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum ClaimError {
    Busy,
    Poisoned,
    Catalog(CatalogError),
}

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
            _ => None,
        }
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

#[derive(Default)]
pub(in crate::storage::catalog) struct SessionWriters {
    held: HashSet<WriterKey>,
}

impl SessionWriters {
    #[cfg(test)]
    pub(in crate::storage::catalog) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    fn claim(&mut self, key: &WriterKey) -> Result<(), ClaimError> {
        if self.held.insert(key.clone()) {
            Ok(())
        } else {
            Err(ClaimError::Busy)
        }
    }

    fn release(&mut self, key: &WriterKey) {
        self.held.remove(key);
    }
}

pub(in crate::storage::catalog) struct WriterGuard {
    pub(super) catalog: Catalog,
    key: WriterKey,
    origin: KeyOrigin,
    _activity: Activity,
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        self.catalog
            .0
            .session_writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release(&self.key);
        self.catalog.0.writer_released.notify_waiters();
        match (&self.key, self.origin) {
            (WriterKey::Derived(_), _) => {
                self.catalog.0.derived_wake.notify_waiters();
                self.catalog.0.maintenance_wake.notify_one();
            }
            (WriterKey::Rewrite(_) | WriterKey::Multipart(_), _)
            | (WriterKey::Resumable(_), KeyOrigin::Client) => {
                self.catalog.0.maintenance_wake.notify_one()
            }
            // Maintenance failures already schedule a retry; only requests rearm skipped work.
            (WriterKey::Resumable(_), KeyOrigin::Catalog) => {}
        }
    }
}

impl Catalog {
    /// Whether any session or transfer writer is held right now.
    #[cfg(test)]
    pub(crate) fn has_session_writers(&self) -> bool {
        !self.0.session_writers.lock().unwrap().is_empty()
    }

    /// Claims a writer now or fails with `Busy`. Maintenance and session trimming use this form:
    /// they skip a held session and come back later, so they never wait behind a client.
    pub(in crate::storage::catalog) fn claim_writer(
        &self,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        let activity = self.activity().map_err(ClaimError::Catalog)?;
        self.claim_leased(activity, key, origin)
    }

    fn claim_leased(
        &self,
        activity: Activity,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        self.0
            .session_writers
            .lock()
            .map_err(|_| ClaimError::Poisoned)?
            .claim(&key)?;
        Ok(WriterGuard {
            catalog: self.clone(),
            key,
            origin,
            _activity: activity,
        })
    }

    /// The lease for a client request's writer, once any exclusive job is done. No caller holds a
    /// lease here, so lining up behind a job still draining the catalog cannot wait for itself.
    async fn client_activity(&self) -> Result<Activity, ClaimError> {
        self.await_activity(Work::New)
            .await
            .map_err(|closed| ClaimError::Catalog(closed.into()))
    }

    /// Claims a writer for a client request, waiting up to the configured writer wait for the
    /// current holder to finish. A client that gave up on a stalled request retries while the
    /// old handler is still unwinding; the retry should take over rather than fail for a state
    /// that clears by itself. Past the budget the holder is really working, so it stays `Busy`.
    pub(in crate::storage::catalog) async fn wait_for_writer(
        &self,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        match self.session_limits().writer_wait {
            WriterWait::Fail => self.claim_leased(self.client_activity().await?, key, origin),
            WriterWait::Within(limit) => {
                tokio::time::timeout_at(limit.deadline(), self.claim_released(key, origin))
                    .await
                    .unwrap_or(Err(ClaimError::Busy))
            }
        }
    }

    async fn claim_released(
        &self,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        loop {
            let released = self.0.writer_released.notified();
            tokio::pin!(released);
            // Registered before the claim and the closed check, so a release or a close right
            // after them still wakes this waiter.
            released.as_mut().enable();
            let activity = self.client_activity().await?;
            match self.claim_leased(activity, key.clone(), origin) {
                Ok(guard) => return Ok(guard),
                Err(ClaimError::Busy) if self.0.admission.is_closed() => {
                    return Err(ClaimError::Catalog(unavailable(
                        BackendFailure::CatalogClosed,
                    )));
                }
                Err(ClaimError::Busy) => released.await,
                Err(error @ (ClaimError::Poisoned | ClaimError::Catalog(_))) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bucket::BucketAccess,
        clock::SystemClock,
        config::Config,
        storage::{
            ResumableOptions,
            admin::LocalAdmin,
            catalog::{ResumableError, resumable::SessionLease},
            failure::BackendFailure,
            test_fixtures::bucket,
        },
    };
    use std::sync::Arc;

    #[test]
    fn namespaces_keep_equal_ids_apart_and_a_held_key_is_busy_until_released() {
        let mut writers = SessionWriters::default();
        let id = BlobId::generate().unwrap();
        writers.claim(&WriterKey::Derived(id)).unwrap();
        writers.claim(&WriterKey::Rewrite(id)).unwrap();
        let session = WriterKey::Resumable(id.to_hex());
        writers.claim(&session).unwrap();
        writers.claim(&WriterKey::Multipart(id.to_hex())).unwrap();
        assert_eq!(writers.claim(&session), Err(ClaimError::Busy));
        writers.release(&session);
        writers.claim(&session).unwrap();
    }

    async fn open(wait_ms: u64) -> (Catalog, String) {
        let catalog = Catalog::open_memory_for_server(
            1024,
            1024 * 1024,
            Arc::new(SystemClock),
            LocalAdmin::Disabled,
        )
        .await
        .unwrap();
        catalog.apply_session_limits(&Config {
            catalog_wait_timeout: crate::time_limit::TimeLimit::from_millis(wait_ms),
            ..Default::default()
        });
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        let session = catalog
            .create_resumable_session(
                "bucket",
                "object",
                ResumableOptions::new(BucketAccess::Permissive),
                None,
            )
            .await
            .unwrap();
        (catalog, session.id)
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
            catalog.claim_writer(WriterKey::Resumable(id.clone()), KeyOrigin::Catalog),
            Err(ClaimError::Busy)
        ));
        drop(lease);
        catalog.close().await.unwrap();
    }
}
