use super::cleanup::DiscardedTransfer;
use super::source::PublicationFile;
use super::*;
use crate::object::ObjectKey;

pub(crate) struct DerivedBody {
    checkpoint: Checkpoint,
    pin: Option<PublicationPin>,
}

impl DerivedTransfer {
    pub(crate) fn publication_body(&self, pin: Option<PublicationPin>) -> DerivedBody {
        DerivedBody {
            checkpoint: Checkpoint::Stored(self.id),
            pin,
        }
    }
}

impl RequestTransfer {
    pub(crate) fn publication_body(self, pin: Option<PublicationPin>) -> DerivedBody {
        DerivedBody {
            checkpoint: Checkpoint::Request(self),
            pin,
        }
    }
}

impl From<&BlobId> for DerivedBody {
    fn from(id: &BlobId) -> Self {
        Self {
            checkpoint: Checkpoint::Stored(*id),
            pin: None,
        }
    }
}

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
    /// Digests the client declared for the new object; a mismatch is the client's error.
    pub declared: ExpectedHashes,
    /// Digests recorded for the bytes the transfer copies; a mismatch means stored data is
    /// damaged, which is no fault of the request.
    pub recorded: ExpectedHashes,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
    pub component_count: Option<i64>,
}

impl DerivedDestination {
    pub(crate) fn key(&self) -> ObjectKey<'_> {
        ObjectKey::from_parts_unchecked(&self.bucket, &self.name)
    }
}

struct TransferPublication<P, C> {
    checkpoint: Checkpoint,
    pinned: Option<(BlobId, Hashes)>,
    destination: DerivedDestination,
    backend: super::super::BlobBackend,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    principals: Option<super::super::principals::Profile>,
    replay: Option<super::super::upload_replay::BoundCopyReplay>,
    prepare: P,
    complete: C,
}

struct Published<T> {
    journaled: objects::Journaled,
    record: ObjectRecord,
    pins: Vec<BlobId>,
    /// What completion produced; `None` when a replay returned the stored record instead.
    completion: Option<T>,
}

/// A published derived object, and what completion produced unless a replay returned the
/// stored record instead.
pub(crate) struct DerivedPublication<T> {
    pub record: ObjectRecord,
    pub completion: Option<T>,
}

fn publish_transfer<P, C, T>(
    connection: &mut Connection,
    request: TransferPublication<P, C>,
    pins: Vec<BlobId>,
) -> Result<Published<T>, ObjectError>
where
    P: FnOnce(&sql::WriteTx<'_>, &mut ObjectRecord) -> Result<(), ObjectError>,
    C: FnOnce(&sql::WriteTx<'_>, &ObjectRecord) -> Result<T, ObjectError>,
{
    let backend = request.backend.clone();
    let now = request.clock.now();
    let (progress, hasher) = request
        .checkpoint
        .read(connection, UnixMillis::try_from(now)?)?;
    let id = progress.id;
    if request.pinned.is_none() && progress.offset != progress.total {
        return Err(ObjectError::DerivedTransferIncomplete);
    }
    let (blob, hashes) = request.pinned.unwrap_or_else(|| (id, hasher.finish()));
    if backend.is_memory() && request.pinned.is_none() {
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
    let (record, completion, journaled) = if let Some(record) = cached {
        objects::journal_new_deletion(&transaction, &blob)?;
        (record, None, objects::Journaled::SOME)
    } else {
        let (record, completion, journaled) =
            publish_destination(&transaction, request, &progress, hashes, now, micros)?;
        (record, Some(completion), journaled)
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
        journaled,
        record,
        pins: if backend.is_memory() {
            Vec::new()
        } else {
            pins
        },
        completion,
    })
}

fn publish_destination<P, C, T>(
    transaction: &sql::WriteTx<'_>,
    request: TransferPublication<P, C>,
    progress: &DerivedTransfer,
    hashes: Hashes,
    now: std::time::SystemTime,
    micros: i64,
) -> Result<(ObjectRecord, T, objects::Journaled), ObjectError>
where
    P: FnOnce(&sql::WriteTx<'_>, &mut ObjectRecord) -> Result<(), ObjectError>,
    C: FnOnce(&sql::WriteTx<'_>, &ObjectRecord) -> Result<T, ObjectError>,
{
    let TransferPublication {
        pinned,
        destination,
        principals,
        replay,
        prepare,
        complete,
        ..
    } = request;
    let id = progress.id;
    let authority =
        objects::BucketAuthority::load(transaction, &destination.bucket, &destination.access)?;
    if replay.is_some() {
        super::super::upload_replay::make_room(transaction, micros)?;
    }
    let bucket = authority.publication?;
    let storage_class = bucket.storage_class(destination.storage_class)?;
    let previous = bucket.lookup(transaction, destination.key())?;
    objects::check_publication(
        previous.as_ref(),
        destination.conditions,
        authority.overwrite,
        now,
    )?;
    destination.metadata.reject_past_retention(now)?;
    destination.recorded.verify(&hashes).map_err(|mismatch| {
        ObjectError::Corrupt(crate::cause::BackendCause::with_source(
            "copied bytes do not reproduce their source's recorded digest",
            mismatch,
        ))
    })?;
    destination.declared.verify(&hashes)?;
    let time = system_time_micros(micros)?;
    let version = objects::NewVersion {
        blob_id: pinned.map_or(id, |(blob, _)| blob),
        assignment: objects::GenerationAssignment::Automatic,
        micros,
        notification_mode: crate::notification::NotificationMode::Emit,
        owner_profile: principals.as_ref(),
        acl: super::super::acl::PublicationAcl::requested(&destination.metadata.acl),
    };
    // Publishing a transfer always wakes the maintenance scheduler once it settles.
    let objects::PublishedVersion {
        record, retired, ..
    } = objects::publish_version(
        transaction,
        previous.as_ref(),
        &bucket,
        version,
        |generation| {
            let defaults = objects::PublicationDefaults {
                owner: Some(bucket.owner(&destination.access, principals.as_ref())?),
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
            Ok(record)
        },
    )?;
    let completion = complete(transaction, &record)?;
    if let Some(replay) = replay {
        replay.store(transaction, micros, &record)?;
    }
    // Compose completion can hard-delete sources as part of this same publication.
    let journaled = if retired == objects::Retired::Journaled {
        objects::Journaled::SOME
    } else {
        objects::Journaled::from(sql::scalar::<bool>(
            transaction,
            "SELECT EXISTS(SELECT 1 FROM derived_deletions WHERE transfer_id = ?1)",
            [id],
        )?)
    };
    Ok((record, completion, journaled))
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
            declared: Default::default(),
            recorded: Default::default(),
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
        complete: impl FnOnce(&sql::WriteTx<'_>, &ObjectRecord) -> Result<(), ObjectError>
        + Send
        + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_with_validation(id.into(), destination, |_, _| Ok(()), complete)
            .await
    }

    /// Validation precedes retirement so same-name sources remain visible in unversioned buckets.
    pub(crate) async fn publish_derived_with_validation(
        &self,
        body: DerivedBody,
        destination: DerivedDestination,
        validate: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&sql::WriteTx<'_>, &ObjectRecord) -> Result<(), ObjectError>
        + Send
        + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_prepared(
            body,
            destination,
            move |connection, record| validate(connection, record),
            complete,
            None,
        )
        .await
        .map(|(record, ())| record)
    }

    /// Adapter-derived fields must use the destination defaults selected by this transaction.
    /// Completion's value, computed in the same transaction, returns with the record.
    pub(crate) async fn publish_derived_prepared<T: Send + 'static>(
        &self,
        body: DerivedBody,
        destination: DerivedDestination,
        prepare: impl FnOnce(&sql::WriteTx<'_>, &mut ObjectRecord) -> Result<(), ObjectError>
        + Send
        + 'static,
        complete: impl FnOnce(&sql::WriteTx<'_>, &ObjectRecord) -> Result<T, ObjectError>
        + Send
        + 'static,
        admission_deadline: Option<tokio::time::Instant>,
    ) -> Result<(ObjectRecord, T), ObjectError> {
        let published = self
            .publish_derived_replayed(
                body,
                destination,
                prepare,
                complete,
                admission_deadline,
                None,
            )
            .await?;
        // Without a replay binding the publication always runs completion.
        let completion = published
            .completion
            .ok_or_else(|| object_unavailable("missing derived completion"))?;
        Ok((published.record, completion))
    }

    pub(crate) async fn publish_derived_replayed<T: Send + 'static>(
        &self,
        body: DerivedBody,
        destination: DerivedDestination,
        prepare: impl FnOnce(&sql::WriteTx<'_>, &mut ObjectRecord) -> Result<(), ObjectError>
        + Send
        + 'static,
        complete: impl FnOnce(&sql::WriteTx<'_>, &ObjectRecord) -> Result<T, ObjectError>
        + Send
        + 'static,
        admission_deadline: Option<tokio::time::Instant>,
        replay: Option<super::super::upload_replay::BoundCopyReplay>,
    ) -> Result<DerivedPublication<T>, ObjectError> {
        let DerivedBody {
            checkpoint,
            mut pin,
        } = body;
        let id = checkpoint.id();
        let admission = async {
            let permit = self.continue_object_upload().await?;
            let guard = self.derived_guard(&id)?;
            let execution = self
                .0
                .writer
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
        crate::task::spawn_supervised("derived-publication", async move {
            let (_permit, _guard) = (permit, guard);
            let query = id;
            let backend = catalog.0.storage.backend.clone();
            let clock = catalog.0.policy.clock.clone();
            let principals = catalog.0.policy.principals.clone();
            let input = pin.as_ref().map(|p| p.input);
            let snapshot = pin.as_ref().is_some_and(|p| matches!(p.file, PublicationFile::Snapshot));
            let pinned = pin.as_mut().map(|p| {
                let blob = match &mut p.file {
                    PublicationFile::Link(link) => { link.backend = None; link.id }
                    PublicationFile::Snapshot => p.input,
                };
                (blob, p.hashes)
            });
            let blob = pinned.map_or(id, |(blob, _)| blob);
            let result = async {
            let (pins, checkpoint) = catalog.run_upload_admitted(execution, move |connection| {
                let (progress, _) = checkpoint.read(connection, UnixMillis::try_from(clock.now())?)?;
                let sources = inputs(connection, &query)?;
                let ready = match input {
                    Some(input) => matches!(sources.as_slice(), [s] if s.id == input && s.start == 0 && s.length == s.size && s.size == progress.total && progress.offset == 0),
                    None => progress.offset == progress.total,
                };
                if !ready { return Err(ObjectError::DerivedTransferIncomplete); }
                if snapshot {
                    // This first and only call has issued no token. Once its sole input
                    // moves to blobs, a crash must abandon the transfer and sweep the orphan.
                    sql::exec_on_writer(connection, "UPDATE derived_transfers SET ephemeral=1 WHERE id=?1 AND ephemeral=0", [query])?;
                    return Ok((Vec::new(), checkpoint));
                }
                Ok((sources.into_iter().map(|source| source.id).collect(), checkpoint))
            }).await?;
            if !backend.is_memory() {
                if pinned.is_none() {
                    let files = backend.clone();
                    crate::task::run_blocking("derived-restore", move || files.restore_draft(&id)).await??;
                }
                catalog.publish_staged(blob.staging_path(&catalog.0.storage.data_dir), blob.blob_path(&catalog.0.storage.data_dir)).await?;
            }
            let publication = TransferPublication { checkpoint, pinned, destination, backend, clock: catalog.0.policy.clock.clone(), principals, replay, prepare, complete };
            let published = catalog.run_upload(move |connection| publish_transfer(connection, publication, pins)).await?;
            catalog.release_transfer(DiscardedTransfer { id, pins: published.pins }).await;
            catalog.reclaim_if(published.journaled).await;
            Ok::<_, ObjectError>(DerivedPublication { record: published.record, completion: published.completion })
            }.await;
            if result.is_err() && pinned.is_some() {
                catalog.discard_staged(objects::StagedBlob::pin(&catalog.0.storage.backend, &catalog.0.storage.data_dir, blob)).await;
            }
            catalog.0.maintenance.wake.notify_one();
            // Check SQL references even after an ambiguous commit error before removing bytes.
            if matches!(&result, Err(error) if !error.is_backend_failure() && !matches!(error, ObjectError::DerivedTransferBusy | ObjectError::DerivedTransferIncomplete | ObjectError::DerivedTransferNotFound))
                && let Err(error) = catalog.cleanup_derived(&id).await {
                deferred_cleanup(&error);
            }
            result
        }).await?
    }
}
