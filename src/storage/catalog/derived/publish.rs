use super::cleanup::DiscardedTransfer;
use super::*;

/// Adapters supply inherited or replacement metadata explicitly; bytes and destination defaults
/// are transport independent. Adapters pass the inherited source class for an absent/null body;
/// `None` selects the destination bucket default for an explicit resource without storageClass.
#[derive(Clone, Debug)]
pub(crate) struct DerivedDestination {
    pub bucket: String,
    pub name: String,
    pub content_type: String,
    pub metadata: ObjectMetadataUpdate,
    pub storage_class: Option<crate::bucket::StorageClass>,
    pub expected: ExpectedHashes,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
    pub component_count: Option<i64>,
}

struct TransferPublication<P, C> {
    id: BlobId,
    destination: DerivedDestination,
    backend: super::super::BlobBackend,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    principals: Option<super::super::principals::Profile>,
    replay: Option<super::super::upload_replay::CopyReplay>,
    prepare: P,
    complete: C,
}

struct Published {
    record: ObjectRecord,
    pins: Vec<BlobId>,
}

fn publish_transfer<P, C>(
    connection: &mut Connection,
    request: TransferPublication<P, C>,
) -> Result<Published, ObjectError>
where
    P: FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError>,
    C: FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError>,
{
    let id = request.id;
    let backend = request.backend.clone();
    let now = request.clock.now();
    let (progress, hasher) = transfer(connection, &id, unix_millis(now)?)?;
    if progress.offset != progress.total {
        return Err(ObjectError::DerivedTransferIncomplete);
    }
    let pins = inputs(connection, &id)?
        .into_iter()
        .map(|source| source.id)
        .collect::<Vec<_>>();
    if backend.is_memory() {
        backend.publish_draft(&id)?;
    }
    let transaction = sql::immediate(connection)?;
    now.duration_since(std::time::UNIX_EPOCH)
        .map_err(object_unavailable)?;
    let micros = unix_micros(now)?;
    objects::authorized_bucket(
        &transaction,
        &request.destination.bucket,
        &request.destination.access,
    )?;
    let cached = request
        .replay
        .as_ref()
        .map(|replay| replay.cached(&transaction, micros))
        .transpose()?
        .flatten();
    let record = if let Some(record) = cached {
        sql::exec(
            &transaction,
            "INSERT INTO pending_blob_deletions(blob_id) VALUES (?1)",
            [id],
        )?;
        record
    } else {
        publish_destination(
            &transaction,
            request,
            &progress,
            hasher.finish(),
            now,
            micros,
        )?
    };
    sql::exec(
        &transaction,
        "DELETE FROM derived_inputs WHERE transfer_id=?1",
        [id],
    )?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_transfers WHERE id=?1",
        [id],
    )?;
    let committed = objects::commit_publication(transaction);
    if backend.is_memory() {
        // A failed commit can still have published: release pins only after checking references.
        let retained: bool = sql::scalar(
            connection,
            "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id=?1)",
            [id],
        )?;
        if !retained {
            for pin in &pins {
                if let Err(error) = backend.remove_pin(pin) {
                    deferred_cleanup(&error);
                }
            }
            if let Err(error) = backend.finish_draft(&id) {
                deferred_cleanup(&error);
            }
        }
    }
    committed?;
    Ok(Published {
        record,
        pins: if backend.is_memory() {
            Vec::new()
        } else {
            pins
        },
    })
}

fn publish_destination<P, C>(
    transaction: &rusqlite::Transaction<'_>,
    request: TransferPublication<P, C>,
    progress: &DerivedTransfer,
    hashes: Hashes,
    now: std::time::SystemTime,
    micros: i64,
) -> Result<ObjectRecord, ObjectError>
where
    P: FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError>,
    C: FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError>,
{
    let TransferPublication {
        id,
        destination,
        principals,
        replay,
        prepare,
        complete,
        ..
    } = request;
    let (_, delete_permission) =
        objects::authorized_bucket(transaction, &destination.bucket, &destination.access)?;
    if replay.is_some() {
        super::super::upload_replay::make_room(transaction, micros)?;
    }
    let bucket = objects::BucketPublication::load(transaction, &destination.bucket)?;
    let storage_class = bucket.storage_class(destination.storage_class);
    let previous = objects::lookup(transaction, &destination.bucket, &destination.name)?;
    objects::check_publication(
        previous.as_ref(),
        destination.conditions,
        delete_permission,
        now,
    )?;
    if destination
        .expected
        .md5
        .is_some_and(|hash| hash != hashes.md5)
        || destination
            .expected
            .crc32c
            .is_some_and(|hash| hash != hashes.crc32c)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    let time = system_time_micros(micros)?;
    let version = objects::NewVersion {
        blob_id: id,
        assignment: objects::GenerationAssignment::Automatic,
        micros,
        origin: super::super::change_journal::origin(&destination.access),
        notification_mode: crate::notification::NotificationMode::Emit,
        owner_profile: principals.as_ref(),
        acl: super::super::acl::PublicationAcl::InheritDefault,
    };
    // Publishing a transfer always wakes the maintenance scheduler once it settles.
    let (record, _retired, ()) = objects::publish_in_transaction(
        transaction,
        previous.as_ref(),
        &bucket,
        version,
        |generation| {
            let defaults = objects::PublicationDefaults {
                owner: Some(super::super::object_owner::for_publication(
                    transaction,
                    &destination.bucket,
                    &destination.access,
                    principals.as_ref(),
                )?),
                storage_class,
                time,
                event_based_hold: bucket.default_event_hold,
            };
            let mut record = defaults.record(
                ObjectTarget::new(destination.bucket, destination.name, generation),
                destination.content_type,
                progress.total,
                hashes,
            );
            record.component_count = destination.component_count;
            destination.metadata.apply(&mut record)?;
            sql::exec(
                transaction,
                "UPDATE derived_transfers SET publishing=1 WHERE id=?1",
                [id],
            )?;
            prepare(transaction, &mut record)?;
            Ok((record, ()))
        },
    )?;
    complete(transaction, &record)?;
    if let Some(replay) = replay {
        replay.store(transaction, micros, &record)?;
    }
    Ok(record)
}

impl DerivedDestination {
    #[cfg(test)]
    pub(crate) fn new(access: BucketAccess) -> Self {
        Self {
            bucket: String::new(),
            name: String::new(),
            content_type: String::new(),
            metadata: Default::default(),
            storage_class: None,
            expected: Default::default(),
            conditions: Default::default(),
            access,
            component_count: None,
        }
    }
}

impl Catalog {
    /// The callback joins protocol-specific completion state to the same destination commit.
    #[cfg(test)]
    pub(crate) async fn publish_derived(
        &self,
        id: &BlobId,
        destination: DerivedDestination,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_with_validation(id, destination, |_, _| Ok(()), complete)
            .await
    }

    /// Validation precedes retirement so same-name sources remain visible in unversioned buckets.
    pub(crate) async fn publish_derived_with_validation(
        &self,
        id: &BlobId,
        destination: DerivedDestination,
        validate: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_prepared(
            id,
            destination,
            move |connection, record| validate(connection, record),
            complete,
            None,
        )
        .await
    }

    /// Adapter-derived fields must use the destination defaults selected by this transaction.
    pub(crate) async fn publish_derived_prepared(
        &self,
        id: &BlobId,
        destination: DerivedDestination,
        prepare: impl FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        admission_deadline: Option<tokio::time::Instant>,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_replayed(id, destination, prepare, complete, admission_deadline, None)
            .await
    }

    pub(crate) async fn publish_derived_replayed(
        &self,
        id: &BlobId,
        destination: DerivedDestination,
        prepare: impl FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        admission_deadline: Option<tokio::time::Instant>,
        replay: Option<super::super::upload_replay::CopyReplay>,
    ) -> Result<ObjectRecord, ObjectError> {
        let admission = async {
            let permit = self.continue_object_upload().await?;
            let guard = self.derived_guard(id)?;
            let execution = self
                .0
                .execution
                .clone()
                .acquire_owned()
                .await
                .map_err(object_unavailable)?;
            Ok::<_, ObjectError>((permit, guard, execution))
        };
        let (permit, guard, execution) = match admission_deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, admission)
                .await
                .map_err(|_| {
                    object_unavailable("derived publication catalog admission timed out")
                })??,
            None => admission.await?,
        };
        let catalog = self.clone();
        let id = *id;
        crate::task::spawn_supervised("derived-publication", async move {
            let (_permit, _guard) = (permit, guard);
            let query = id;
            let backend = catalog.0.backend.clone();
            let clock = catalog.0.clock.clone();
            let principals = catalog.0.principals.get().cloned();
            let result = async {
            catalog.run_upload_admitted(execution, move |connection| {
                let (progress, hasher) = transfer(connection, &query, unix_millis(clock.now())?)?;
                let sources = inputs(connection, &query)?;
                if progress.offset != progress.total { return Err(ObjectError::DerivedTransferIncomplete); }
                Ok((hasher, sources))
            }).await?;
            if !backend.is_memory() {
                let files = backend.clone();
                crate::task::run_blocking("derived-restore", move || files.restore_draft(&id)).await??;
                catalog.publish_staged(id.staging_path(&catalog.0.data_dir), id.blob_path(&catalog.0.data_dir)).await?;
            }
            let publication = TransferPublication { id, destination, backend, clock: catalog.0.clock.clone(), principals, replay, prepare, complete };
            let published = catalog.run_upload(move |connection| publish_transfer(connection, publication)).await?;
            catalog.release_transfer(DiscardedTransfer { id, pins: published.pins }).await;
            Ok::<_, ObjectError>(published.record)
            }.await;
            if result.is_ok() {
                catalog.reclaim_after_commit().await;
            }
            catalog.0.maintenance_wake.notify_one();
            // Check SQL references even after an ambiguous commit error before removing bytes.
            if matches!(&result, Err(error) if !error.is_backend_failure() && !matches!(error, ObjectError::DerivedTransferBusy | ObjectError::DerivedTransferIncomplete | ObjectError::DerivedTransferNotFound))
                && let Err(error) = catalog.cleanup_derived(&id).await {
                deferred_cleanup(&error);
            }
            result
        }).await?
    }
}
