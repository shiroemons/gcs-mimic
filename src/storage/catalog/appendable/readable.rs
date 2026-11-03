use super::{Catalog, Lookup, SlotHandle, authorized_bucket, check_conditions, record, select};
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord, ObjectTarget},
    storage::{
        BlobId, ObjectReader,
        catalog::{customer_keys, objects},
    },
};
use rusqlite::Transaction;

/// Whether the selected generation is immutable or still appendable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finality {
    Finalized,
    Unfinalized,
}

fn selected_appendable(
    transaction: &Transaction<'_>,
    catalog: &Catalog,
    target: &ObjectTarget,
    conditions: ObjectConditions,
    access: &BucketAccess,
) -> Result<(ObjectRecord, BlobId), ObjectError> {
    let (storage_class, _) = authorized_bucket(transaction, &target.bucket, access)?;
    let Lookup::Live(row) = select(transaction, &target.bucket, &target.name)? else {
        return Err(ObjectError::ObjectNotFound);
    };
    if target
        .generation
        .is_some_and(|generation| generation != row.generation)
    {
        return Err(ObjectError::ObjectNotFound);
    }
    let record = record(
        transaction,
        &target.bucket,
        &target.name,
        storage_class,
        &row,
        catalog.0.principals.get(),
    )?;
    check_conditions(Some(&record), conditions, objects::RequestKind::Read)?;
    Ok((record, row.blob_id))
}

pub(super) fn open_appendable_in(
    transaction: &Transaction<'_>,
    catalog: &Catalog,
    target: &ObjectTarget,
    conditions: ObjectConditions,
    access: &BucketAccess,
) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
    let (record, blob) = selected_appendable(transaction, catalog, target, conditions, access)?;
    // The same execution permit prevents finalization from unlinking before this open.
    let reader = ObjectReader::bounded_data(catalog.0.backend.open_appendable(&blob)?, record.size);
    Ok((record, reader))
}

impl Catalog {
    pub(crate) async fn open_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader, Finality), ObjectError> {
        let catalog = self.clone();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            match objects::open_selected_in(&transaction, &catalog, &target, conditions, &access) {
                Ok((record, reader)) => {
                    customer_keys::check(&record, None)?;
                    Ok((record, reader, Finality::Finalized))
                }
                Err(ObjectError::ObjectNotFound) => {
                    open_appendable_in(&transaction, &catalog, &target, conditions, &access)
                        .map(|(record, reader)| (record, reader, Finality::Unfinalized))
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

    /// Both states use one catalog snapshot; metadata requests never open a blob.
    pub(crate) async fn get_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, Finality), ObjectError> {
        let catalog = self.clone();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            match objects::selected(
                &transaction,
                &target.bucket,
                &target.name,
                target.generation,
                conditions,
                objects::RequestKind::Read,
                &access,
            ) {
                Ok(stored) => Ok((stored.record, Finality::Finalized)),
                Err(ObjectError::ObjectNotFound) => {
                    selected_appendable(&transaction, &catalog, &target, conditions, &access)
                        .map(|(record, _)| (record, Finality::Unfinalized))
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

    pub(crate) async fn delete_including_appendable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let slot = SlotHandle::new(self, &target.bucket, &target.name);
        let mut guard = slot.lock().await;
        // Finalization uses this slot before entering the catalog executor too.
        match self.delete_object(target.clone(), conditions, access).await {
            Err(ObjectError::ObjectNotFound) => {
                self.delete_appendable_locked(target, conditions, access, &mut guard)
                    .await
            }
            other => other,
        }
    }
}
