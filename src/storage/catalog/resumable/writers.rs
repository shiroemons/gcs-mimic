use super::{Catalog, CatalogError, MAX_SESSIONS, unavailable};
use crate::storage::{blob_id::BlobId, failure::BackendFailure};
use std::{collections::HashSet, fmt};

/// Typed namespaces keep unrelated transfer and upload IDs independent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::storage::catalog) enum WriterKey {
    Resumable(String),
    Multipart(String),
    Rewrite(BlobId),
    Derived(BlobId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum KeyOrigin {
    Client,
    Catalog,
}

impl WriterKey {
    fn capped(&self, origin: KeyOrigin) -> bool {
        matches!((self, origin), (Self::Resumable(_), KeyOrigin::Client))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum ClaimError {
    Busy,
    AtCapacity,
    Poisoned,
    Catalog(CatalogError),
}

impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("session writer is busy"),
            Self::AtCapacity => f.write_str("session writer capacity reached"),
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
            ClaimError::Busy | ClaimError::AtCapacity => unavailable(BackendFailure::SessionBusy),
            ClaimError::Poisoned => unavailable("session writer registry is poisoned"),
            ClaimError::Catalog(error) => error,
        }
    }
}

#[derive(Default)]
pub(in crate::storage::catalog) struct SessionWriters {
    held: HashSet<WriterKey>,
    client_resumable: usize,
}

impl SessionWriters {
    #[cfg(test)]
    pub(in crate::storage::catalog) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    fn claim(&mut self, key: &WriterKey, origin: KeyOrigin) -> Result<(), ClaimError> {
        if self.held.contains(key) {
            return Err(ClaimError::Busy);
        }
        let capped = key.capped(origin);
        if capped && self.client_resumable >= MAX_SESSIONS {
            return Err(ClaimError::AtCapacity);
        }
        self.held.insert(key.clone());
        self.client_resumable += usize::from(capped);
        Ok(())
    }

    fn release(&mut self, key: &WriterKey, origin: KeyOrigin) {
        if self.held.remove(key) && key.capped(origin) {
            self.client_resumable = self.client_resumable.saturating_sub(1);
        }
    }
}

pub(in crate::storage::catalog) struct WriterGuard {
    pub(super) catalog: Catalog,
    key: WriterKey,
    origin: KeyOrigin,
    _activity: super::super::Activity,
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        self.catalog
            .0
            .session_writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release(&self.key, self.origin);
        match &self.key {
            WriterKey::Derived(_) => {
                self.catalog.0.derived_wake.notify_waiters();
                self.catalog.0.maintenance_wake.notify_one();
            }
            WriterKey::Rewrite(_) | WriterKey::Multipart(_) => {
                self.catalog.0.maintenance_wake.notify_one()
            }
            WriterKey::Resumable(_) => {
                // Maintenance failures already schedule a retry; only requests rearm skipped work.
                if self.origin == KeyOrigin::Client {
                    self.catalog.0.maintenance_wake.notify_one();
                }
            }
        }
    }
}

impl Catalog {
    pub(in crate::storage::catalog) fn claim_writer(
        &self,
        key: WriterKey,
        origin: KeyOrigin,
    ) -> Result<WriterGuard, ClaimError> {
        let activity = self.activity().map_err(ClaimError::Catalog)?;
        self.0
            .session_writers
            .lock()
            .map_err(|_| ClaimError::Poisoned)?
            .claim(&key, origin)?;
        Ok(WriterGuard {
            catalog: self.clone(),
            key,
            origin,
            _activity: activity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_and_catalog_claims_do_not_consume_client_resumable_capacity() {
        let mut writers = SessionWriters::default();
        for index in 0..MAX_SESSIONS {
            let id = BlobId::generate().unwrap();
            writers
                .claim(&WriterKey::Derived(id), KeyOrigin::Catalog)
                .unwrap();
            writers
                .claim(&WriterKey::Rewrite(id), KeyOrigin::Catalog)
                .unwrap();
            writers
                .claim(&WriterKey::Resumable(index.to_string()), KeyOrigin::Client)
                .unwrap();
        }
        let extra = WriterKey::Resumable("extra".into());
        assert_eq!(
            writers.claim(&extra, KeyOrigin::Client),
            Err(ClaimError::AtCapacity)
        );
        writers.claim(&extra, KeyOrigin::Catalog).unwrap();
        writers.release(&extra, KeyOrigin::Catalog);
        assert_eq!(
            writers.claim(&extra, KeyOrigin::Client),
            Err(ClaimError::AtCapacity)
        );
        writers.release(&WriterKey::Resumable("0".into()), KeyOrigin::Client);
        writers.claim(&extra, KeyOrigin::Client).unwrap();
        assert_eq!(
            writers.claim(&extra, KeyOrigin::Catalog),
            Err(ClaimError::Busy)
        );
    }

    #[tokio::test]
    async fn admission_trims_terminal_sessions_at_client_writer_capacity() {
        let catalog = Catalog::open_memory_for_server(
            1024,
            1024 * 1024,
            std::sync::Arc::new(crate::clock::SystemClock),
            crate::storage::admin::LocalAdmin::Disabled,
        )
        .await
        .unwrap();
        catalog
            .insert_bucket(crate::storage::test_fixtures::bucket("bucket", "project"))
            .await
            .unwrap();
        let options = super::super::ResumableOptions::new(crate::bucket::BucketAccess::Permissive);
        catalog
            .create_resumable_session("bucket", "source", options.clone(), None)
            .await
            .unwrap();
        catalog.run::<_, CatalogError>(|connection| {
            connection.execute_batch("WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<129) INSERT INTO resumable_sessions SELECT printf('%064x',n),printf('%032x',n),bucket,name,incarnation,options,total,0,created,deadline,'CANCELLED',NULL FROM ids CROSS JOIN (SELECT * FROM resumable_sessions LIMIT 1);")?;
            Ok(())
        }).await.unwrap();
        let guards = (0..MAX_SESSIONS)
            .map(|index| {
                catalog
                    .try_resumable_writer(&format!("client-{index}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        catalog
            .create_resumable_session("bucket", "new", options, None)
            .await
            .unwrap();
        let terminal: i64 = catalog
            .run(|connection| {
                crate::storage::catalog::sql::scalar(
                    connection,
                    "SELECT count(*) FROM resumable_sessions WHERE state='CANCELLED'",
                    [],
                )
                .map_err(unavailable)
            })
            .await
            .unwrap();
        assert_eq!(terminal, 128);
        drop(guards);
        catalog.close().await.unwrap();
    }
}
