//! Atomic create-only publication of trusted import records and observations.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::import_journal::{self, ImportBinding, ImportElement};
use super::*;

impl Admin {
    #[expect(
        clippy::disallowed_methods,
        reason = "import publishes each bucket once"
    )]
    pub(crate) async fn publish_import_bucket(
        &self,
        binding: ImportBinding,
        expected_cursor: i64,
        element: ImportElement,
        definition: BucketDefinition,
    ) -> Result<EnsureBucket, AdminError> {
        let origin = self.origin();
        self.catalog
            .run_admin(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(unavailable)?;
                import_journal::check_publication(&transaction, &binding, expected_cursor)?;
                let mut record = definition.0;
                let exists: bool = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM buckets WHERE name = ?1)",
                        [&record.name],
                        |row| row.get(0),
                    )
                    .map_err(unavailable)?;
                let result = if exists {
                    Err(AdminError::Precondition)
                } else {
                    super::super::insert_bucket_in_transaction(&transaction, &mut record)?;
                    super::super::change_journal::append(
                        &transaction,
                        origin,
                        super::super::change_journal::ChangeKind::BucketCreate,
                        &record.name,
                        None,
                    )?;
                    Ok(EnsureBucket::Created(record))
                };
                import_journal::record_outcomes(
                    &transaction,
                    &binding,
                    expected_cursor,
                    &[element],
                    &[result.is_ok()],
                )?;
                transaction.commit().map_err(unavailable)?;
                Ok(result)
            })
            .await?
    }
}

#[cfg(test)]
mod tests;
