use super::{Catalog, Lookup, SlotHandle, authorized_bucket, check_conditions, record, select};
use crate::{
    bucket::BucketAccess,
    csek::{CustomerKey, StoredHashes, check_metadata},
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

/// An object's metadata in whichever state the selected generation is in.
pub(crate) struct ReadableMetadata {
    pub record: ObjectRecord,
    pub finality: Finality,
    pub hashes: StoredHashes,
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
    /// Opens the selected generation's data after verifying `key` against it. The check runs in
    /// the same read as the selection and after authorization, so an unauthorized caller cannot
    /// learn whether a key is right. An appendable object is never encrypted, so any key fails
    /// there with `ResourceNotEncrypted`.
    pub(crate) async fn open_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<(ObjectRecord, ObjectReader, Finality), ObjectError> {
        let catalog = self.clone();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let (record, reader, finality) = match objects::open_selected_in(
                &transaction,
                &catalog,
                &target,
                conditions,
                &access,
            ) {
                Ok((record, reader)) => (record, reader, Finality::Finalized),
                Err(ObjectError::ObjectNotFound) => {
                    let (record, reader) =
                        open_appendable_in(&transaction, &catalog, &target, conditions, &access)?;
                    (record, reader, Finality::Unfinalized)
                }
                Err(error) => return Err(error),
            };
            customer_keys::check(&record, key)?;
            Ok((record, reader, finality))
        })
        .await
    }

    /// Both states use one catalog snapshot; metadata requests never open a blob. A supplied
    /// key must be the right one; the result says whether the stored bytes' hashes may be shown.
    pub(crate) async fn get_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<ReadableMetadata, ObjectError> {
        let catalog = self.clone();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let (record, finality) = match objects::selected(
                &transaction,
                &target.bucket,
                &target.name,
                target.generation,
                conditions,
                objects::RequestKind::Read,
                &access,
            ) {
                Ok(stored) => (stored.record, Finality::Finalized),
                Err(ObjectError::ObjectNotFound) => {
                    let (record, _) =
                        selected_appendable(&transaction, &catalog, &target, conditions, &access)?;
                    (record, Finality::Unfinalized)
                }
                Err(error) => return Err(error),
            };
            let hashes = check_metadata(record.customer_key_sha256, key)
                .map_err(ObjectError::CustomerEncryption)?;
            Ok(ReadableMetadata {
                record,
                finality,
                hashes,
            })
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
