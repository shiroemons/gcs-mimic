//! Indexed XML multipart retirement under the shared lifecycle pass and action budget.

use super::{
    BucketRules, Catalog, CatalogError, LifecycleCounts, LifecycleSchedule, ObjectError,
    bucket_rules, emit_quarantines, sql, unavailable,
};
use crate::{
    bucket::lifecycle::{LifecycleAction, evaluate::multipart_abort_due},
    storage::catalog::{
        resumable::writers::{ClaimError, KeyOrigin, WriterGuard, WriterKey},
        system_time_millis,
    },
};
use rusqlite::{Connection, params};
use std::{sync::atomic::Ordering, time::SystemTime};

#[cfg(test)]
mod tests;

const UPLOAD_PAGE: usize = 128;
const UPLOAD_SCAN: &str = "SELECT name, id, created FROM xml_multipart_uploads WHERE bucket = ?1 AND (name, id) > (?2, ?3) AND incarnation = ?4 ORDER BY name, id LIMIT ?5";

type Cursor = (String, String);

struct Page {
    outcome: LifecycleCounts,
    after: Option<Cursor>,
    rules: BucketRules,
}

struct Bucket<'a> {
    name: &'a str,
    incarnation: &'a str,
}

impl LifecycleSchedule {
    pub(in crate::storage::catalog) fn multipart_released(&self, wake: &tokio::sync::Notify) {
        if self.enabled.load(Ordering::Acquire) && self.multipart_busy.swap(false, Ordering::AcqRel)
        {
            self.on_write(wake);
        }
    }
}

impl Catalog {
    pub(super) async fn sweep_lifecycle_abort(
        &self,
        bucket: &str,
        incarnation: &str,
        now: SystemTime,
        limit: u64,
    ) -> (LifecycleCounts, Option<CatalogError>) {
        let mut outcome = LifecycleCounts::default();
        let mut cursor = (String::new(), String::new());
        let mut rules = BucketRules::Absent;
        loop {
            let (bucket, incarnation) = (bucket.to_owned(), incarnation.to_owned());
            let catalog = self.clone();
            let remaining = limit - outcome.aborted;
            let page = self
                .run(move |connection| {
                    catalog
                        .abort_page(
                            connection,
                            &Bucket {
                                name: &bucket,
                                incarnation: &incarnation,
                            },
                            &cursor,
                            now,
                            remaining,
                            rules,
                        )
                        .map_err(unavailable)
                })
                .await;
            let page = match page {
                Ok(page) => page,
                Err(error) => return (outcome, Some(error)),
            };
            outcome.record(page.outcome);
            let Some(after) = page.after else {
                return (outcome, None);
            };
            if outcome.aborted == limit {
                return (outcome, None);
            }
            cursor = after;
            rules = page.rules;
        }
    }

    fn abort_page(
        &self,
        connection: &mut Connection,
        bucket: &Bucket<'_>,
        cursor: &Cursor,
        now: SystemTime,
        limit: u64,
        cached: BucketRules,
    ) -> Result<Page, ObjectError> {
        let transaction = sql::immediate(connection)?;
        let mut diagnostics = Vec::new();
        let rules = bucket_rules(
            &transaction,
            bucket.name,
            bucket.incarnation,
            cached,
            &mut diagnostics,
        )?;
        let mut outcome = LifecycleCounts::default();
        let mut after = None;
        let mut leases = Vec::new();
        if let BucketRules::Ready { rules: parsed, .. } = &rules
            && parsed
                .iter()
                .any(|rule| rule.action == LifecycleAction::AbortIncompleteMultipartUpload)
        {
            let rows = transaction
                .prepare_cached(UPLOAD_SCAN)?
                .query_map(
                    params![
                        bucket.name,
                        cursor.0,
                        cursor.1,
                        bucket.incarnation,
                        UPLOAD_PAGE
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            after = (rows.len() == UPLOAD_PAGE)
                .then(|| rows.last().map(|(name, id, _)| (name.clone(), id.clone())))
                .flatten();
            for (name, id, created) in rows {
                outcome.scanned += 1;
                let Some(due) =
                    multipart_abort_due(parsed, &name, system_time_millis(created)?, now)
                else {
                    continue;
                };
                if due > now {
                    outcome.next = outcome.next.into_iter().chain(Some(due)).min();
                    continue;
                }
                let Some(lease) = self.claim_abort_upload(&id)? else {
                    outcome.skipped += 1;
                    continue;
                };
                self.retire_lifecycle_multipart(&transaction, &id)?;
                leases.push(lease);
                outcome.aborted += 1;
                if outcome.aborted == limit {
                    break;
                }
            }
        }
        #[cfg(test)]
        let data_dir = super::super::fault::database_dir(&transaction);
        #[cfg(test)]
        if outcome.aborted > 0 {
            super::super::fault::hit(&data_dir, super::super::fault::Step::LifecycleCommit)
                .map_err(crate::storage::failure::object_unavailable)?;
        }
        transaction.commit()?;
        emit_quarantines(diagnostics);
        #[cfg(test)]
        if outcome.aborted > 0 {
            super::super::fault::hit(&data_dir, super::super::fault::Step::LifecycleCommitted)
                .map_err(crate::storage::failure::object_unavailable)?;
        }
        drop(leases);
        Ok(Page {
            outcome,
            after,
            rules,
        })
    }

    fn claim_abort_upload(&self, id: &str) -> Result<Option<WriterGuard>, ObjectError> {
        // Arm before claiming: a client releasing between Busy and arming cannot lose its wake.
        let was_busy = self.0.lifecycle.multipart_busy.swap(true, Ordering::AcqRel);
        match self.claim_writer(WriterKey::Multipart(id.to_owned()), KeyOrigin::Catalog) {
            Ok(lease) => {
                if !was_busy {
                    self.0
                        .lifecycle
                        .multipart_busy
                        .store(false, Ordering::Release);
                }
                Ok(Some(lease))
            }
            Err(ClaimError::Busy) => Ok(None),
            Err(error @ (ClaimError::Poisoned | ClaimError::Catalog(_))) => {
                Err(CatalogError::from(error).into())
            }
        }
    }
}
