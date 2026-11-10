use super::*;

enum CommitOutcome {
    Published {
        record: ObjectRecord,
        retired: Retired,
        journaled: Journaled,
    },
    Replayed(ObjectRecord),
    Unchanged(ObjectRecord),
}

impl CommitOutcome {
    fn into_record(self) -> ObjectRecord {
        match self {
            Self::Published { record, .. } | Self::Replayed(record) | Self::Unchanged(record) => {
                record
            }
        }
    }
}

/// The streamed body's size and digests.
struct UploadedBody {
    size: u64,
    md5: [u8; 16],
    crc32c: u32,
    sha256: [u8; 32],
}

/// Everything a single-request upload commits once its blob is durable.
struct UploadCommit {
    backend: crate::config::Backend,
    bucket: String,
    name: String,
    content_type: String,
    metadata: ObjectMetadataUpdate,
    conditions: ObjectConditions,
    access: BucketAccess,
    principals: Option<super::super::principals::Profile>,
    mode: super::super::admin::PublishMode,
    identity: PublishIdentity,
    owner: OwnerAssignment,
    replay: Option<super::super::upload_replay::UploadReplay>,
    clock: Arc<dyn Clock>,
    blob_id: BlobId,
    body: UploadedBody,
}

pub(in crate::storage::catalog) enum OwnerAssignment {
    Request,
    Preserve(Option<crate::object::owner::ObjectOwner>),
    Unknown,
}

/// The authority that fixes a publication's generation and timestamps.
#[derive(Clone, Copy)]
pub(in crate::storage::catalog) enum PublishIdentity {
    Assigned,
    /// Admin publication validates imported identities against their request before admission.
    Imported(super::super::admin::ImportedIdentity),
    Reserved(super::super::appendable::ReservedGeneration),
}

impl PublishIdentity {
    fn assignment(self) -> GenerationAssignment {
        match self {
            Self::Assigned => GenerationAssignment::Automatic,
            Self::Imported(identity) => identity.generation().map_or(
                GenerationAssignment::Automatic,
                GenerationAssignment::Imported,
            ),
            Self::Reserved(reserved) => GenerationAssignment::Imported(reserved.generation()),
        }
    }

    fn timestamps(self) -> Option<(std::time::SystemTime, std::time::SystemTime)> {
        match self {
            Self::Assigned => None,
            Self::Imported(identity) => Some((identity.time_created(), identity.updated())),
            Self::Reserved(reserved) => Some((reserved.time_created(), reserved.updated())),
        }
    }

    fn checks_past_retention(self) -> bool {
        matches!(self, Self::Assigned)
    }

    fn retires_appendable(self) -> bool {
        !matches!(self, Self::Assigned)
    }
}

impl UploadCommit {
    /// Identifies the request for upload replay: target, metadata, conditions and body.
    fn fingerprint(&self) -> Vec<u8> {
        super::super::upload_replay::UploadFingerprint {
            bucket: &self.bucket,
            name: &self.name,
            content_type: &self.content_type,
            metadata: &self.metadata,
            conditions: self.conditions,
            size: self.body.size,
            sha256: self.body.sha256,
        }
        .digest()
    }

    fn record(
        &self,
        generation: i64,
        defaults: PublicationDefaults,
    ) -> Result<ObjectRecord, ObjectError> {
        let time = defaults.time;
        let mut record = defaults.record(
            ObjectTarget::new(&self.bucket, &self.name, generation),
            self.content_type.clone(),
            self.body.size,
            Hashes {
                md5: self.body.md5,
                crc32c: self.body.crc32c,
            },
        );
        if self.identity.checks_past_retention() {
            self.metadata.reject_past_retention(time)?;
        }
        self.metadata.clone().apply(&mut record)?;
        if let Some((created, updated)) = self.identity.timestamps() {
            record.time_created = created;
            record.updated = updated;
        }
        Ok(record)
    }

    /// Publishes the upload in one immediate transaction, or returns the replayed or
    /// unchanged record.
    fn commit(self, connection: &mut Connection) -> Result<CommitOutcome, ObjectError> {
        let transaction = sql::immediate(connection)?;
        let now = PublicationTime::read(self.clock.as_ref())?;
        let (_, delete_permission) = authorized_bucket(&transaction, &self.bucket, &self.access)?;
        let micros = now.micros;
        let fingerprint = self.replay.as_ref().map(|_| self.fingerprint());
        if let (Some(replay), Some(fingerprint)) = (&self.replay, &fingerprint) {
            replay.check_incarnation(&transaction, &self.bucket)?;
            if let Some(record) = replay.replay(&transaction, micros, fingerprint)? {
                return Ok(CommitOutcome::Replayed(record));
            }
            super::super::upload_replay::make_room(&transaction, micros)?;
        }
        let bucket = BucketPublication::load(&transaction, &self.bucket)?;
        let storage_class = bucket.storage_class(self.metadata.storage_class);
        let previous = lookup(&transaction, &self.bucket, &self.name)?;
        // Rechecked under the write lock so a concurrent create cannot be overwritten.
        check_publication(
            previous.as_ref(),
            self.conditions,
            delete_permission,
            now.time,
        )?;
        let body = &self.body;
        if skip_same(
            self.mode,
            previous.as_ref(),
            body.size,
            body.md5,
            body.crc32c,
        ) {
            return previous
                .map(|stored| CommitOutcome::Unchanged(stored.record))
                .ok_or(ObjectError::ObjectNotFound);
        }
        let defaults = PublicationDefaults {
            storage_class,
            event_based_hold: bucket.default_event_hold,
            time: now.time,
            owner: match &self.owner {
                OwnerAssignment::Request => Some(super::super::object_owner::for_publication(
                    &transaction,
                    &self.bucket,
                    &self.access,
                    self.principals.as_ref(),
                )?),
                OwnerAssignment::Preserve(owner) => owner.clone(),
                OwnerAssignment::Unknown => None,
            },
        };
        let (record, retired, ()) = publish_in_transaction(
            &transaction,
            previous.as_ref(),
            &bucket,
            NewVersion {
                blob_id: self.blob_id,
                assignment: self.identity.assignment(),
                micros,
                origin: super::super::change_journal::origin(&self.access),
                notification_mode: NotificationMode::for_access(&self.access),
                owner_profile: self.principals.as_ref(),
                acl: super::super::acl::PublicationAcl::requested(self.metadata.predefined_acl),
            },
            |generation| {
                let mut record = self.record(generation, defaults)?;
                inherit_metadata(self.mode, previous.as_ref(), &mut record);
                Ok((record, ()))
            },
        )?;
        let appendable = self.finish_metadata(&transaction, &record)?;
        if let (Some(replay), Some(fingerprint)) = (&self.replay, &fingerprint) {
            replay.store(
                &transaction,
                micros,
                fingerprint,
                &record,
                &self.access,
                None,
            )?;
        }
        commit_publication(transaction)?;
        Ok(CommitOutcome::Published {
            record,
            retired,
            journaled: Journaled::from(retired) | appendable,
        })
    }

    fn finish_metadata(
        &self,
        transaction: &Transaction<'_>,
        record: &ObjectRecord,
    ) -> Result<Journaled, ObjectError> {
        super::super::admin::apply_acl(
            transaction,
            self.metadata.acl.as_deref(),
            &self.access,
            record,
        )?;
        // Only a reserved generation can match an appendable row.
        if self.identity.retires_appendable() {
            return super::super::appendable::retire_published(transaction, record, self.backend);
        }
        Ok(Journaled::NOTHING)
    }
}

impl Catalog {
    pub(in crate::storage::catalog) async fn run_upload<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        self.run_upload_admitted(execution, operation).await
    }

    pub(in crate::storage::catalog) async fn run_upload_admitted<T: Send + 'static>(
        &self,
        execution: OwnedSemaphorePermit,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        let inner = self.0.clone();
        self.0
            .executor
            .run(move || {
                let _execution = execution;
                match inner.with_connection(operation) {
                    Ok(result) => result,
                    Err(super::super::ConnectionUnavailable::Closed) => Err(object_unavailable(
                        crate::storage::failure::BackendFailure::CatalogClosed,
                    )),
                    Err(error) => Err(object_unavailable(error)),
                }
            })
            .await
            .map_err(object_unavailable)?
    }

    /// Advisory check that also rejects a bucket or an overwrite the caller is not authorized
    /// for. Publication repeats every part of it inside its own transaction.
    pub(crate) async fn check_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let (_, delete_permission) = authorized_bucket(&transaction, &bucket, &access)?;
            let previous = lookup(&transaction, &bucket, &name)?;
            check_publication(
                previous.as_ref(),
                conditions,
                delete_permission,
                clock.now(),
            )
        })
        .await
    }

    /// HTTP connection admission bounds streaming uploads.
    /// File I/O uses short Tokio filesystem jobs, never a blocking worker awaiting a body.
    #[cfg(test)]
    pub(crate) async fn put_object(
        &self,
        upload: ObjectUpload<'_>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<ObjectRecord, ObjectError> {
        let permit = self
            .prepare_object_upload(
                upload.bucket,
                upload.name,
                upload.conditions,
                &upload.access,
            )
            .await?;
        self.put_object_prechecked(upload, body, permit).await
    }

    pub(crate) async fn prepare_object_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<UploadGuard, ObjectError> {
        let permit = self.wait_object_upload().await?;
        self.check_upload(bucket, name, conditions, access).await?;
        Ok(permit)
    }

    /// Admits an upload a client just started, once any exclusive job waiting or running is done.
    pub(crate) async fn wait_object_upload(&self) -> Result<UploadGuard, ObjectError> {
        let activity = self.await_activity(Work::New).await?;
        self.counted_upload(activity)
    }

    /// Admits the next step of work already under way, once any exclusive job running is done.
    /// It need not hold a lease: a copy holds none between its steps, while a rewrite does.
    pub(in crate::storage::catalog) async fn continue_object_upload(
        &self,
    ) -> Result<UploadGuard, ObjectError> {
        let activity = self.await_activity(Work::UnderWay).await?;
        self.counted_upload(activity)
    }

    /// Admits work whose caller already holds a lease, such as a session writer or a derived
    /// source. No exclusive job can run under that lease, so this never fails on one and needs no
    /// wait, which lets synchronous paths use it.
    pub(crate) fn admit_object_upload(&self) -> Result<UploadGuard, ObjectError> {
        self.counted_upload(self.activity()?)
    }

    fn counted_upload(&self, activity: Activity) -> Result<UploadGuard, ObjectError> {
        let mut uploads = self
            .0
            .uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if uploads.closed || !self.is_healthy() {
            return Err(object_unavailable(
                crate::storage::failure::BackendFailure::CatalogClosed,
            ));
        }
        uploads.active += 1;
        Ok(UploadGuard {
            slot: super::super::UploadSlot::Counted(self.clone()),
            replay: None,
            _activity: activity,
        })
    }

    pub(crate) async fn put_object_prechecked(
        &self,
        upload: ObjectUpload<'_>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        permit: UploadGuard,
    ) -> Result<ObjectRecord, ObjectError> {
        self.put_object_with_mode(
            upload,
            body,
            permit,
            super::super::admin::PublishMode::Replace,
            PublishIdentity::Assigned,
            OwnerAssignment::Request,
        )
        .await
    }

    pub(in crate::storage::catalog) async fn put_object_with_mode(
        &self,
        upload: ObjectUpload<'_>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        mut permit: UploadGuard,
        mode: super::super::admin::PublishMode,
        identity: PublishIdentity,
        owner: OwnerAssignment,
    ) -> Result<ObjectRecord, ObjectError> {
        let ObjectUpload {
            bucket,
            name,
            content_type,
            expected,
            conditions,
            max_bytes,
            metadata,
            access,
        } = upload;
        crate::object::validate_fixed_metadata("contentType", &content_type)?;
        metadata.validate()?;
        let catalog = self.clone();
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let (_request, cancellation) = oneshot::channel::<()>();
        let replay = permit.replay.take();
        // Cancellation must not release directory ownership during publication or cleanup.
        crate::task::spawn_supervised("object-upload", async move {
            let _permit = permit;
            let blob =
                super::super::super::blob_id::BlobId::generate().map_err(object_unavailable)?;
            let id = blob;
            let stage = blob.staging_path(&catalog.0.data_dir);
            let published = blob.blob_path(&catalog.0.data_dir);
            let mut file = catalog.0.backend.stage(&id).await?;
            let result = async {
                let (size, md5, crc32c, sha256) =
                    write_stream(&mut file, expected, max_bytes, body, cancellation, &catalog)
                        .await?;
                match file {
                    UploadSink::Memory(memory) => memory.publish(id.to_hex())?,
                    UploadSink::Disk(file) => {
                        drop(file);
                        catalog
                            .publish_staged(stage.clone(), published.clone())
                            .await?;
                    }
                }
                let commit = UploadCommit {
                    backend: catalog.0.backend.kind(),
                    bucket,
                    name,
                    content_type,
                    metadata,
                    conditions,
                    access,
                    principals: catalog.0.principals.get().cloned(),
                    mode,
                    identity,
                    owner,
                    replay,
                    clock: catalog.0.clock.clone(),
                    blob_id: id,
                    body: UploadedBody {
                        size,
                        md5,
                        crc32c,
                        sha256,
                    },
                };
                catalog
                    .run_upload(move |connection| commit.commit(connection))
                    .await
            }
            .await;
            match &result {
                Ok(CommitOutcome::Published {
                    retired, journaled, ..
                }) => {
                    catalog.schedule_expiry(*retired);
                    catalog.reclaim_if(*journaled).await
                }
                Ok(CommitOutcome::Replayed(_) | CommitOutcome::Unchanged(_)) | Err(_) => {
                    catalog
                        .discard_staged(StagedBlob {
                            id,
                            stage: (!catalog.0.backend.is_memory()).then_some(stage),
                        })
                        .await;
                }
            }
            result.map(CommitOutcome::into_record)
        })
        .await?
    }
}

pub(in crate::storage::catalog) enum UploadSink {
    Disk(StagedFile),
    Memory(super::super::super::memory::Stage),
}

pub(in crate::storage::catalog) async fn write_stream(
    file: &mut UploadSink,
    mut expected: ExpectedHashes,
    max_bytes: u64,
    mut body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    mut cancellation: oneshot::Receiver<()>,
    catalog: &Catalog,
) -> Result<(u64, [u8; 16], u32, [u8; 32]), ObjectError> {
    let mut size = 0_u64;
    let mut hasher = Hasher::default();
    let mut sha256 = ring::digest::Context::new(&ring::digest::SHA256);
    let closing = catalog.0.admin_shutdown.notified();
    tokio::pin!(closing);
    closing.as_mut().enable();
    #[cfg(test)]
    let data_dir = &catalog.0.data_dir;
    loop {
        if catalog.0.admission.is_closed() {
            return Err(object_unavailable("catalog is closing"));
        }
        let chunk = tokio::select! {
            biased;
            _ = &mut closing => return Err(object_unavailable("catalog is closing")),
            _ = &mut cancellation => return Err(ObjectError::BodyAborted("upload cancelled".into())),
            chunk = body.recv() => chunk,
        };
        let chunk = chunk
            .ok_or_else(|| ObjectError::BodyAborted("missing body EOF".into()))?
            .map_err(|error| {
                if error.kind() == io::ErrorKind::StorageFull {
                    ObjectError::Io(error.into())
                } else {
                    ObjectError::body_aborted(error)
                }
            })?;
        let chunk = match chunk {
            UploadChunk::End => break,
            UploadChunk::EndWithHashes(hashes) => {
                if hashes.md5.zip(expected.md5).is_some_and(|(a, b)| a != b)
                    || hashes
                        .crc32c
                        .zip(expected.crc32c)
                        .is_some_and(|(a, b)| a != b)
                {
                    return Err(ObjectError::ChecksumMismatch);
                }
                expected.md5 = expected.md5.or(hashes.md5);
                expected.crc32c = expected.crc32c.or(hashes.crc32c);
                break;
            }
            UploadChunk::Data(chunk) => chunk,
        };
        size = size
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        for part in chunk.chunks(CHUNK_SIZE) {
            hasher.update(part);
            sha256.update(part);
            #[cfg(test)]
            fault::hit(data_dir, Step::BodyWrite).map_err(object_unavailable)?;
            match file {
                UploadSink::Disk(staged) => staged.write(part).await?,
                UploadSink::Memory(stage) => stage.write(part)?,
            }
        }
    }
    let Hashes { md5, crc32c } = hasher.finish();
    if let Some(provided) = expected.crc32c.filter(|value| *value != crc32c) {
        return Err(ObjectError::HashMismatch(Box::new(HashMismatch {
            algorithm: HashAlgorithm::Crc32c,
            provided: provided.to_be_bytes().to_vec(),
            calculated: crc32c.to_be_bytes().to_vec(),
        })));
    }
    if let Some(provided) = expected.md5.filter(|value| *value != md5) {
        return Err(ObjectError::HashMismatch(Box::new(HashMismatch {
            algorithm: HashAlgorithm::Md5,
            provided: provided.to_vec(),
            calculated: md5.to_vec(),
        })));
    }
    if let UploadSink::Disk(staged) = file {
        staged.make_durable().await?;
    }
    Ok((
        size,
        md5,
        crc32c,
        sha256
            .finish()
            .as_ref()
            .try_into()
            .map_err(object_unavailable)?,
    ))
}
