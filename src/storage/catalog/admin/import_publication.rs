//! Atomic create-only publication of trusted import records and observations.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::import_journal::{self, ImportBinding, ImportElement};
use super::*;
use crate::storage::catalog::sql;

impl Admin {
    #[cfg(test)]
    pub(crate) async fn publish_import_bucket(
        &self,
        binding: ImportBinding,
        expected_cursor: i64,
        element: ImportElement,
        definition: BucketDefinition,
    ) -> Result<EnsureBucket, AdminError> {
        self.publish_import_bucket_with_acl(
            binding,
            expected_cursor,
            element,
            definition,
            None,
            None,
        )
        .await
    }

    pub(crate) async fn publish_import_bucket_with_acl(
        &self,
        binding: ImportBinding,
        expected_cursor: i64,
        element: ImportElement,
        definition: BucketDefinition,
        acl: Option<Vec<crate::acl::AclEntry>>,
        source_created: Option<std::time::SystemTime>,
    ) -> Result<EnsureBucket, AdminError> {
        let origin = self.origin();
        let principals = self.catalog.0.policy.principals.clone();
        self.catalog
            .run_admin(move |connection| {
                let transaction = sql::immediate(connection)?;
                import_journal::check_publication(&transaction, &binding, expected_cursor)?;
                let mut record = definition.0;
                let exists: bool = sql::scalar(
                    &transaction,
                    "SELECT EXISTS(SELECT 1 FROM buckets WHERE name = ?1)",
                    [&record.name],
                )?;
                let result = if exists {
                    Err(AdminError::Precondition)
                } else {
                    // An import reproduces its source, so it invents no default soft delete policy.
                    super::super::insert_bucket_in_transaction(&transaction, &mut record, None)?;
                    if let Some(created) = source_created {
                        super::super::bucket_timestamps::store_imported(
                            &transaction,
                            &mut record,
                            created,
                        )?;
                    }
                    if let Some(entries) = &acl {
                        super::super::acl::replace_resource_acl(
                            &transaction,
                            super::super::acl::ResolvedAclTarget::Bucket {
                                bucket: &record.name,
                                scope: super::super::acl::BucketScope::Bucket,
                            },
                            entries,
                            &mint(origin, NotificationMode::Suppress).access(),
                            principals.as_ref(),
                        )?;
                    }
                    Ok(EnsureBucket::Created(record))
                };
                import_journal::record_outcomes(
                    &transaction,
                    &binding,
                    expected_cursor,
                    &[import_journal::ElementResult {
                        element: &element,
                        outcome: import_journal::ElementOutcome::of(&result),
                    }],
                )?;
                transaction.commit()?;
                Ok(result)
            })
            .await?
    }
}

#[cfg(test)]
mod tests;
