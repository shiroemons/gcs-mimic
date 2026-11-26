use super::*;
use crate::storage::catalog::sql;
use crate::{
    hashing::Hasher,
    object::{ExpectedHashes, UploadChunk},
};
use std::io;
use tokio::sync::{mpsc, oneshot};

pub(crate) struct PartUpload {
    pub expected: ExpectedHashes,
    pub max_bytes: u64,
}

/// Validated in manifest order while the completion holds the whole upload.
struct CompletionParts {
    first: Part,
    remaining: Vec<Part>,
    total: u64,
}

/// The bytes the other parts of an upload retain. Every part reads this twice: before its body,
/// to refuse early, and in its commit, because parts in flight have not counted each other yet.
const OTHER_PARTS_BYTES: &str =
    "SELECT coalesce(sum(size),0) FROM xml_multipart_parts WHERE upload_id=?1 AND number!=?2";

/// Sync the directory on the blocking pool before committing the row that names the new entry.
#[cfg(unix)]
fn sync_staging_entry(
    catalog: &Catalog,
    #[cfg(test)] step: super::super::fault::Step,
) -> Result<(), ObjectError> {
    #[cfg(test)]
    super::super::fault::hit(&catalog.0.storage.data_dir, step).map_err(object_unavailable)?;
    catalog
        .0
        .storage
        .backend
        .sync_staging_dir()
        .map_err(object_unavailable)
}

fn publication_error(error: Error) -> ObjectError {
    match error {
        Error::NoSuchUpload => ObjectError::DerivedTransferNotFound,
        Error::Object(error) => error,
        error @ (Error::InvalidPart
        | Error::InvalidPartOrder
        | Error::PartTooSmall
        | Error::Busy) => {
            ObjectError::Unavailable(crate::storage::failure::BackendCause::from_error(error))
        }
    }
}

impl Catalog {
    pub(crate) async fn put_xml_part(
        &self,
        lease: PartLease,
        request: PartUpload,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<Part, Error> {
        let permit = self.admit_object_upload()?;
        let catalog = self.clone();
        let (_request, cancellation) = oneshot::channel();
        crate::task::spawn_supervised("xml-multipart-part",async move {
            let (_permit,lease)=(permit,lease);
            let number=lease.number;
            let now=UnixMillis::try_from(catalog.0.policy.clock.now())?;
            let target=lease.upload.target.clone();
            let access=lease.access.clone();
            let retained=catalog.run(move |connection| -> Result<_, Error> {
                let transaction=connection.transaction().map_err(object_unavailable)?;
                active(&transaction,&target,&access,now)?;
                objects::authorized_bucket(&transaction,&target.bucket,&access.with_permission(Permission::ObjectsCreate))?;
                sql::scalar::<u64>(&transaction, OTHER_PARTS_BYTES, params![target.id,number]).map_err(object_unavailable).map_err(Error::from)
            }).await?;
            let budget=request.max_bytes.checked_sub(retained).ok_or(ObjectError::TooLarge)?.min(MAX_PART_BYTES);
            let blob=super::super::super::blob_id::BlobId::generate().map_err(object_unavailable)?;
            let id=blob;
            let mut file=catalog.0.storage.backend.stage(&id, lease.guard.clone()).await?;
            let result=async {
                let objects::UploadedBody{size,md5,crc32c}=objects::write_stream(&mut file,request.expected,budget,body,cancellation,&catalog,None).await?;
                #[cfg(test)]
                let data_dir=catalog.0.storage.data_dir.clone();
                let target=lease.upload.target.clone();
                let access=lease.access.clone();
                let clock=catalog.0.policy.clock.clone();
                #[cfg(unix)]
                if !catalog.0.storage.backend.is_memory() {
                    let syncing = catalog.clone();
                    crate::task::run_blocking("xml-multipart-part-sync", move || sync_staging_entry(&syncing, #[cfg(test)] super::super::fault::Step::MultipartPartSync)).await.map_err(object_unavailable)??;
                }
                catalog.run(move |connection| -> Result<_, Error> {
                    if let objects::UploadSink::Memory(stage)=file { stage.publish(id.to_hex()).map_err(object_unavailable)?; }
                    #[cfg(test)] super::super::fault::hit(&data_dir,super::super::fault::Step::Checkpoint).map_err(object_unavailable)?;
                    let transaction=sql::immediate(connection).map_err(object_unavailable)?;
                    let now=StoredTime::try_from(clock.now())?;
                    active(&transaction,&target,&access,now.millis)?;
                    objects::authorized_bucket(&transaction,&target.bucket,&access.with_permission(Permission::ObjectsCreate))?;
                    let others=sql::scalar::<u64>(&transaction, OTHER_PARTS_BYTES, params![target.id,number]).map_err(object_unavailable)?;
                    others.checked_add(size).filter(|total| *total<=request.max_bytes).ok_or(ObjectError::TooLarge)?;
                    sql::exec(&transaction, "INSERT OR IGNORE INTO xml_multipart_deletions SELECT blob_id FROM xml_multipart_parts WHERE upload_id=?1 AND number=?2", params![target.id,number]).map_err(object_unavailable)?;
                    sql::exec(&transaction, "INSERT INTO xml_multipart_parts (upload_id,number,blob_id,size,md5,crc32c,updated,updated_sub_milli_micros) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(upload_id,number) DO UPDATE SET blob_id=excluded.blob_id,size=excluded.size,md5=excluded.md5,crc32c=excluded.crc32c,updated=excluded.updated,updated_sub_milli_micros=excluded.updated_sub_milli_micros", params![target.id,number,id,size,md5.as_slice(),crc32c,now.millis,now.micros]).map_err(object_unavailable)?;
                    let stored=part(&transaction,&target.id,number)?;
                    objects::commit_publication(transaction)?;
                    Ok((stored, maintenance::pending(connection)))
                }).await
            }.await;
            let (result, cleanup) = match result {
                Ok((stored, pending)) => (Ok(stored), pending),
                Err(error) => (Err(error), catalog.journal_unreferenced_xml_part(id).await),
            };
            let cleanup = match cleanup {
                Ok(true) => catalog.drain_xml_multipart().await.map(|_| ()),
                Ok(false) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = cleanup {catalog.multipart_cleanup_failed(error);}
            drop(lease);
            result
        }).await.map_err(object_unavailable)?
    }

    /// A commit error can arrive after the row was published. Check ownership before queuing
    /// the staged part for deletion.
    async fn journal_unreferenced_xml_part(&self, id: BlobId) -> Result<bool, ObjectError> {
        self.run(move |connection| {
            let referenced: bool = sql::scalar(
                connection,
                "SELECT EXISTS(SELECT 1 FROM xml_multipart_parts WHERE blob_id=?1)",
                [id],
            )?;
            if !referenced {
                sql::exec_on_writer(
                    connection,
                    "INSERT OR IGNORE INTO xml_multipart_deletions VALUES (?1)",
                    [id],
                )?;
            }
            maintenance::pending(connection)
        })
        .await
    }

    pub(crate) async fn complete_xml_multipart(
        &self,
        lease: UploadLease,
        parts: Vec<CompletedPart>,
        max_bytes: u64,
    ) -> Result<ObjectRecord, Error> {
        if parts.len() > MAX_PARTS {
            return Err(Error::InvalidPart);
        }
        let mut previous = 0;
        for part in &parts {
            if part.number <= previous {
                return Err(Error::InvalidPartOrder);
            }
            if part.number > MAX_PART_NUMBER {
                return Err(Error::InvalidPart);
            }
            previous = part.number;
        }
        let mut parts = parts.into_iter();
        let first = parts.next().ok_or(Error::InvalidPart)?;
        let catalog = self.clone();
        let permit = self.admit_object_upload()?;
        crate::task::spawn_supervised("xml-multipart-complete", async move {
            let (_permit, lease) = (permit, lease);
            let id = BlobId::generate().map_err(object_unavailable)?;
            let owner = catalog.guard_derived_request(&id);
            let parts = catalog
                .xml_completion_parts(&lease, first, parts, max_bytes)
                .await?;
            if let Err(error) = catalog.assemble_xml_parts(&lease, parts, &id).await {
                let orphan = id;
                let cleanup = catalog
                    .run(move |connection| {
                        sql::exec_on_writer(
                            connection,
                            "INSERT OR IGNORE INTO xml_multipart_deletions VALUES (?1)",
                            [orphan],
                        )?;
                        Ok(())
                    })
                    .await;
                let cleanup = match cleanup {
                    Ok(()) => catalog.drain_xml_multipart().await.map(|_| ()),
                    Err(error) => Err(error),
                };
                if let Err(cleanup) = cleanup {
                    catalog.multipart_cleanup_failed(cleanup);
                }
                return Err(error);
            }
            let target = lease.upload.target.clone();
            let incarnation = lease.upload.incarnation;
            let access = lease.access.clone();
            let clock = catalog.0.policy.clock.clone();
            let destination = super::super::derived::DerivedDestination {
                bucket: target.bucket.clone(),
                name: target.name.clone(),
                content_type: lease.upload.options.content_type.clone(),
                metadata: lease.upload.options.metadata.clone(),
                storage_class: lease.upload.options.metadata.storage_class,
                declared: Default::default(),
                recorded: Default::default(),
                conditions: Default::default(),
                access: access.with_permission(Permission::ObjectsCreate),
                component_count: None,
            };
            let result = catalog
                .publish_derived_prepared(
                    (&id).into(),
                    destination,
                    move |connection, record| {
                        record.md5_availability = crate::object::Md5Availability::Unavailable;
                        let upload = active(
                            connection,
                            &target,
                            &access,
                            UnixMillis::try_from(clock.now())?,
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
                .await
                .map(|(record, ())| record);
            drop(owner);
            if let Err(error) = catalog.drain_xml_multipart().await {
                catalog.multipart_cleanup_failed(error);
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

    async fn xml_completion_parts(
        &self,
        lease: &UploadLease,
        first: CompletedPart,
        requested: std::vec::IntoIter<CompletedPart>,
        max_bytes: u64,
    ) -> Result<CompletionParts, Error> {
        let target = lease.upload.target.clone();
        let access = lease.access.clone();
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        self.run_read(move |connection| -> Result<_, Error> {
            let snapshot = sql::read_snapshot(connection).map_err(object_unavailable)?;
            active(&snapshot, &target, &access, now)?;
            objects::authorized_bucket(
                &snapshot,
                &target.bucket,
                &access.with_permission(Permission::ObjectsCreate),
            )?;
            let mut total = 0u64;
            let mut load = |wanted: &CompletedPart, successors: usize| {
                let stored = part(&snapshot, &target.id, wanted.number)?;
                if stored.etag != wanted.etag {
                    return Err(Error::InvalidPart);
                }
                if successors > 0 && stored.size < MIN_PART_BYTES {
                    return Err(Error::PartTooSmall);
                }
                total = total
                    .checked_add(stored.size)
                    .filter(|n| *n <= max_bytes)
                    .ok_or(ObjectError::TooLarge)?;
                Ok(stored)
            };
            let first = load(&first, requested.len())?;
            let mut remaining = Vec::with_capacity(requested.len());
            for (index, wanted) in requested.as_slice().iter().enumerate() {
                remaining.push(load(wanted, requested.len() - index - 1)?);
            }
            Ok(CompletionParts {
                first,
                remaining,
                total,
            })
        })
        .await
    }

    async fn assemble_xml_parts(
        &self,
        lease: &UploadLease,
        completion: CompletionParts,
        id: &BlobId,
    ) -> Result<(), Error> {
        use md5::digest::common::hazmat::SerializableState;

        let id = *id;
        let catalog = self.clone();
        let guard = lease.guard.clone();
        let mut assembly = crate::task::run_blocking("xml-assemble-part", move || {
            let mut assembly = Assembly {
                file: catalog.0.storage.backend.stage_copy_output(&id, guard)?,
                md5: md5::Md5::default(),
                crc32c: 0,
            };
            assembly.append(&catalog, &completion.first)?;
            Ok::<_, ObjectError>(assembly)
        })
        .await
        .map_err(object_unavailable)??;
        for part in completion.remaining {
            let catalog = self.clone();
            assembly = crate::task::run_blocking("xml-assemble-part", move || {
                assembly.append(&catalog, &part)?;
                Ok::<_, ObjectError>(assembly)
            })
            .await
            .map_err(object_unavailable)??;
        }
        #[cfg(unix)]
        let catalog = self.clone();
        let Assembly {
            mut file,
            md5,
            crc32c,
        } = crate::task::run_blocking("xml-multipart-assemble-sync", move || {
            assembly.file.make_durable()?;
            #[cfg(unix)]
            if !catalog.0.storage.backend.is_memory() {
                sync_staging_entry(
                    &catalog,
                    #[cfg(test)]
                    super::super::fault::Step::MultipartAssembleSync,
                )?;
            }
            Ok::<_, ObjectError>(assembly)
        })
        .await
        .map_err(object_unavailable)??;
        let state = md5.serialize();
        let total = completion.total;
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        self.run(move |connection| {
            sql::exec_on_writer(
                connection,
                "INSERT INTO derived_transfers VALUES (?1,?2,?2,?3,?4,1,?5,?6,0)",
                params![
                    id,
                    total,
                    state.as_slice(),
                    crc32c,
                    now,
                    now.saturating_add_millis(900000)
                ],
            )?;
            file.acknowledge(total);
            Ok::<_, ObjectError>(())
        })
        .await?;
        Ok(())
    }
}

struct Assembly {
    file: super::super::transfer_io::CopyOutput,
    md5: md5::Md5,
    crc32c: u32,
}

impl Assembly {
    fn append(&mut self, catalog: &Catalog, part: &Part) -> Result<(), ObjectError> {
        use md5::Digest;

        let mut input = catalog.0.storage.backend.open_input(&part.blob_id, 0)?;
        let mut part_hash = Hasher::default();
        objects::with_copy_buffer(|buffer| {
            catalog.copy_range(
                &mut input,
                part.size,
                buffer,
                |bytes| {
                    part_hash.update(bytes);
                    self.md5.update(bytes);
                    self.file.write(bytes)
                },
                |_| object_unavailable("multipart part is truncated"),
            )
        })?;
        if part_hash.finish() != part.hashes {
            return Err(object_unavailable("multipart part data is corrupt"));
        }
        self.crc32c = super::super::compose_crc32c::combine(
            self.crc32c,
            part.hashes.crc32c,
            usize::try_from(part.size).map_err(object_unavailable)?,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clock::SystemClock, storage::catalog::read_pool::tests::assert_point_read_with_setup,
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn completion_reads_the_committed_parts() {
        let (_dir, catalog) = super::super::tests::setup(Arc::new(SystemClock)).await;
        let target = super::super::tests::begin(&catalog).await;
        let written = super::super::tests::put(&catalog, &target, 1, b"part")
            .await
            .unwrap();
        assert_point_read_with_setup(
            &catalog,
            async || {
                catalog
                    .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
                    .await
                    .unwrap()
            },
            async |lease| {
                let parts = catalog
                    .xml_completion_parts(
                        &lease,
                        CompletedPart {
                            number: written.number,
                            etag: written.etag.clone(),
                        },
                        Vec::new().into_iter(),
                        4,
                    )
                    .await
                    .unwrap();
                assert_eq!(parts.first.blob_id, written.blob_id);
                assert!(parts.remaining.is_empty());
                assert_eq!(parts.total, 4);
            },
        )
        .await;
        catalog.close().await.unwrap();
    }
}
