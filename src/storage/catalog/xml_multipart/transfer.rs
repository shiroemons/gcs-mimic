use super::super::transfer_io::{InputReader, Output};
use super::*;
use crate::{
    hashing::Hasher,
    object::{ExpectedHashes, UploadChunk},
};
use std::io;
use tokio::sync::{mpsc, oneshot};

pub(crate) struct PartUpload {
    pub number: u16,
    pub expected: ExpectedHashes,
    pub max_bytes: u64,
}

fn stage(data_dir: &Path, id: &str) -> Result<std::path::PathBuf, ObjectError> {
    super::super::super::blob_id::BlobId::parse(id)
        .map(|id| id.staging_path(data_dir))
        .ok_or_else(|| object_unavailable("invalid multipart blob ID"))
}

fn publication_error(error: Error) -> ObjectError {
    match error {
        Error::NoSuchUpload => ObjectError::DerivedTransferNotFound,
        Error::Object(error) => error,
        error => ObjectError::Unavailable(crate::storage::failure::BackendCause::from_error(error)),
    }
}

impl Catalog {
    pub(crate) async fn put_xml_part(
        &self,
        lease: Lease,
        request: PartUpload,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<Part, Error> {
        if !(1..=10000).contains(&request.number) {
            return Err(Error::InvalidPart);
        }
        let permit = self.admit_object_upload()?;
        let catalog = self.clone();
        let (_request, cancellation) = oneshot::channel();
        crate::task::spawn_supervised("xml-multipart-part",async move {
            let (_permit,lease)=(permit,lease);
            let now=unix_millis(catalog.0.clock.now())?;
            let target=lease.upload.target.clone();
            let access=lease.access.clone();
            let retained=catalog.run_object(move |connection| {
                let transaction=connection.transaction().map_err(object_unavailable)?;
                let result=(|| -> Result<_,Error> {
                    active(&transaction,&target,&access,now)?;
                    objects::authorized_bucket(&transaction,&target.bucket,&access_for(&access,Permission::ObjectsCreate))?;
                    transaction.prepare_cached("SELECT coalesce(sum(size),0) FROM xml_multipart_parts WHERE upload_id=?1 AND number!=?2")
                        .and_then(|mut s|s.query_row(params![target.id,request.number],|r|r.get::<_,i64>(0))).map(|value|value as u64).map_err(object_unavailable).map_err(Error::from)
                })();
                Ok(result)
            }).await??;
            let budget=request.max_bytes.checked_sub(retained).ok_or(ObjectError::TooLarge)?.min(MAX_PART_BYTES);
            let blob=super::super::super::blob_id::BlobId::generate().map_err(object_unavailable)?;
            let id=blob.to_hex();
            let path=blob.staging_path(&catalog.0.data_dir);
            let mut file=if let Some(memory)=&catalog.0.memory {
                objects::UploadSink::Memory(memory.stage().map_err(object_unavailable)?)
            } else {
                objects::UploadSink::Disk(tokio::fs::File::from_std(objects::create_upload_stage(path.clone(),blob.blob_path(&catalog.0.data_dir)).await?))
            };
            let result=async {
                let (size,md5,crc32c,_)=objects::write_body(&mut file,request.expected,budget,body,cancellation,&catalog).await?;
                #[cfg(any(unix, test))]
                let data_dir=catalog.0.data_dir.clone();
                let target=lease.upload.target.clone();
                let access=lease.access.clone();
                let id=id.clone();
                let clock=catalog.0.clock.clone();
                #[cfg(unix)]
                let memory=catalog.0.memory.clone();
                catalog.run_object(move |connection| {
                    if let objects::UploadSink::Memory(stage)=file { stage.publish(id.clone()).map_err(object_unavailable)?; }
                    #[cfg(unix)] if memory.is_none() { objects::sync_dir(&data_dir.join("staging")).map_err(object_unavailable)?; }
                    #[cfg(test)] super::super::fault::hit(&data_dir,super::super::fault::Step::Checkpoint).map_err(object_unavailable)?;
                    let transaction=connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(object_unavailable)?;
                    let result=(|| -> Result<_,Error> {
                        let now=unix_millis(clock.now())?;
                        active(&transaction,&target,&access,now)?;
                        objects::authorized_bucket(&transaction,&target.bucket,&access_for(&access,Permission::ObjectsCreate))?;
                        transaction.prepare_cached("INSERT OR IGNORE INTO xml_multipart_deletions SELECT blob_id FROM xml_multipart_parts WHERE upload_id=?1 AND number=?2")
                            .and_then(|mut s|s.execute(params![target.id,request.number])).map_err(object_unavailable)?;
                        transaction.prepare_cached("INSERT INTO xml_multipart_parts VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(upload_id,number) DO UPDATE SET blob_id=excluded.blob_id,size=excluded.size,md5=excluded.md5,crc32c=excluded.crc32c,updated=excluded.updated")
                            .and_then(|mut s|s.execute(params![target.id,request.number,id,size as i64,md5.as_slice(),crc32c,now])).map_err(object_unavailable)?;
                        let stored=part(&transaction,&target.id,request.number)?;
                        objects::commit_publication(transaction)?;
                        Ok(stored)
                    })();
                    Ok(result)
                }).await?
            }.await;
            // An error after COMMIT is ambiguous: only reclaim a file without a committed reference.
            let data_dir=catalog.0.data_dir.clone();
            let memory=catalog.0.memory.clone();
            if let Err(error)=catalog.run_object(move |connection| {
                let referenced:bool=connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM xml_multipart_parts WHERE blob_id=?1)").and_then(|mut s|s.query_row([&id],|r|r.get(0))).map_err(object_unavailable)?;
                if !referenced {
                    connection.prepare_cached("INSERT OR IGNORE INTO xml_multipart_deletions VALUES (?1)").and_then(|mut s|s.execute([id])).map_err(object_unavailable)?;
                }
                maintenance::drain_storage(connection,&data_dir,memory.as_deref())?;
                Ok(())
            }).await {catalog.multipart_cleanup_failed(&error);}
            drop(lease);
            result
        }).await.map_err(object_unavailable)?
    }

    pub(crate) async fn complete_xml_multipart(
        &self,
        lease: Lease,
        parts: Vec<CompletedPart>,
        max_bytes: u64,
    ) -> Result<ObjectRecord, Error> {
        if parts.is_empty() || parts.len() > MAX_PARTS {
            return Err(Error::InvalidPart);
        }
        let mut previous = 0;
        for part in &parts {
            if part.number <= previous {
                return Err(Error::InvalidPartOrder);
            }
            if part.number > 10000 {
                return Err(Error::InvalidPart);
            }
            previous = part.number;
        }
        let catalog = self.clone();
        let permit = self.admit_object_upload()?;
        crate::task::spawn_supervised("xml-multipart-complete", async move {
            let (_permit, lease) = (permit, lease);
            let id = super::super::super::blob_id::BlobId::generate()
                .map_err(object_unavailable)?
                .to_hex();
            let owner = catalog.guard_derived_request(&id);
            let total = catalog
                .xml_completion_total(&lease, parts.clone(), max_bytes)
                .await?;
            if let Err(error) = catalog.assemble_xml_parts(&lease, &parts, &id, total).await {
                let data_dir = catalog.0.data_dir.clone();
                let orphan = id.clone();
                let memory = catalog.0.memory.clone();
                if let Err(cleanup) = catalog
                    .run_object(move |connection| {
                        connection
                            .prepare_cached(
                                "INSERT OR IGNORE INTO xml_multipart_deletions VALUES (?1)",
                            )
                            .and_then(|mut s| s.execute([orphan]))
                            .map_err(object_unavailable)?;
                        maintenance::drain_storage(connection, &data_dir, memory.as_deref())?;
                        Ok(())
                    })
                    .await
                {
                    catalog.multipart_cleanup_failed(&cleanup);
                }
                return Err(error);
            }
            let target = lease.upload.target.clone();
            let incarnation = lease.upload.incarnation.clone();
            let access = lease.access.clone();
            let clock = catalog.0.clock.clone();
            let destination = super::super::derived::DerivedDestination {
                bucket: target.bucket.clone(),
                name: target.name.clone(),
                content_type: lease.upload.options.content_type.clone(),
                metadata: lease.upload.options.metadata.clone(),
                storage_class: lease.upload.options.metadata.storage_class.clone(),
                expected: Default::default(),
                conditions: Default::default(),
                access: access_for(&access, Permission::ObjectsCreate),
                component_count: None,
            };
            let result = catalog
                .publish_derived_prepared(
                    &id,
                    destination,
                    move |connection, record| {
                        record.md5_availability = crate::object::Md5Availability::Unavailable;
                        let upload = active(
                            connection,
                            &target,
                            &access,
                            unix_millis(clock.now()).map_err(object_unavailable)?,
                        )
                        .map_err(publication_error)?;
                        if upload.incarnation != incarnation {
                            return Err(ObjectError::DerivedTransferNotFound);
                        }
                        maintenance::retire(connection, &target.id)
                    },
                    |_, _| Ok(()),
                    None,
                )
                .await;
            drop(owner);
            let data_dir = catalog.0.data_dir.clone();
            let memory = catalog.0.memory.clone();
            if let Err(error) = catalog
                .run_object(move |connection| {
                    maintenance::drain_storage(connection, &data_dir, memory.as_deref())?;
                    Ok(())
                })
                .await
            {
                catalog.multipart_cleanup_failed(&error);
            }
            drop(lease);
            match result {
                Err(ObjectError::DerivedTransferNotFound) => Err(Error::NoSuchUpload),
                other => other.map_err(Error::from),
            }
        })
        .await
        .map_err(object_unavailable)?
    }

    async fn xml_completion_total(
        &self,
        lease: &Lease,
        requested: Vec<CompletedPart>,
        max_bytes: u64,
    ) -> Result<u64, Error> {
        let target = lease.upload.target.clone();
        let access = lease.access.clone();
        let now = unix_millis(self.0.clock.now())?;
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(object_unavailable)?;
            Ok((|| -> Result<_, Error> {
                active(&transaction, &target, &access, now)?;
                objects::authorized_bucket(
                    &transaction,
                    &target.bucket,
                    &access_for(&access, Permission::ObjectsCreate),
                )?;
                let mut total = 0u64;
                for (index, wanted) in requested.iter().enumerate() {
                    let stored = part(&transaction, &target.id, wanted.number)?;
                    if stored.etag != wanted.etag {
                        return Err(Error::InvalidPart);
                    }
                    if index + 1 < requested.len() && stored.size < MIN_PART_BYTES {
                        return Err(Error::PartTooSmall);
                    }
                    total = total
                        .checked_add(stored.size)
                        .filter(|n| *n <= max_bytes)
                        .ok_or(ObjectError::TooLarge)?;
                }
                Ok(total)
            })())
        })
        .await?
    }

    async fn assemble_xml_parts(
        &self,
        lease: &Lease,
        parts: &[CompletedPart],
        id: &str,
        total: u64,
    ) -> Result<(), Error> {
        let mut file = if let Some(memory) = &self.0.memory {
            memory.create_draft(id.into()).map_err(object_unavailable)?;
            Output::open(self, id, 0).await?
        } else {
            let path = stage(&self.0.data_dir, id)?;
            let blob_path = objects::blob_path(&self.0.data_dir, id)?;
            Output::Disk(tokio::fs::File::from_std(
                objects::create_upload_stage(path, blob_path).await?,
            ))
        };
        let mut hasher = Hasher::default();
        let mut buffer = [0; 64 * 1024];
        for wanted in parts {
            let upload_id = lease.upload.target.id.clone();
            let number = wanted.number;
            let stored = self
                .run_object(move |connection| Ok(part(connection, &upload_id, number)))
                .await??;
            let mut input = InputReader::open(self, &stored.blob_id, 0).await?;
            let mut remaining = stored.size;
            let mut part_hash = Hasher::default();
            while remaining > 0 {
                if self.0.admission.is_closed() {
                    return Err(object_unavailable("catalog is closing").into());
                }
                let length = buffer.len().min(remaining as usize);
                let n = input.read(&mut buffer[..length]).await?;
                if n == 0 {
                    return Err(object_unavailable("multipart part is truncated").into());
                }
                part_hash.update(&buffer[..n]);
                hasher.update(&buffer[..n]);
                #[cfg(test)]
                super::super::fault::hit(&self.0.data_dir, super::super::fault::Step::BodyWrite)
                    .map_err(object_unavailable)?;
                file.write(&buffer[..n]).await?;
                remaining -= n as u64;
                if self.0.memory.is_some() {
                    tokio::task::yield_now().await;
                }
            }
            let hashes = part_hash.finish();
            if hashes.md5 != stored.md5 || hashes.crc32c != stored.crc32c {
                return Err(object_unavailable("multipart part data is corrupt").into());
            }
        }
        file.finish(self).await?;
        #[cfg(unix)]
        let data_dir = self.0.data_dir.clone();
        let id = id.to_owned();
        let now = unix_millis(self.0.clock.now())?;
        let (state, crc32c) = hasher.md5_state();
        #[cfg(unix)]
        let memory = self.0.memory.clone();
        self.run_object(move |connection| {
            #[cfg(unix)]
            if memory.is_none() {
                objects::sync_dir(&data_dir.join("staging")).map_err(object_unavailable)?;
            }
            let count: i64 = connection
                .prepare_cached("SELECT count(*) FROM derived_transfers WHERE ephemeral=1")
                .and_then(|mut s| s.query_row([], |r| r.get(0)))
                .map_err(object_unavailable)?;
            if count >= 128 {
                return Err(object_unavailable("derived transfer limit reached"));
            }
            connection
                .prepare_cached("INSERT INTO derived_transfers VALUES (?1,?2,?2,?3,?4,1,?5,?6,0)")
                .and_then(|mut s| {
                    s.execute(params![
                        id,
                        total as i64,
                        state.as_slice(),
                        crc32c,
                        now,
                        now + 900000
                    ])
                })
                .map_err(object_unavailable)?;
            file.acknowledge(total);
            Ok(())
        })
        .await?;
        Ok(())
    }
}
