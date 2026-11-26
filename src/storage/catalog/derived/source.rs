use super::super::{BlobBackend, transfer_io::InputReader};
use super::*;
use crate::storage::memory::BlobError;
use std::io;

/// A metadata snapshot and independent link to immutable bytes, including after source deletion.
/// Unconsumed handles clean their staging link; `begin_derived` transfers ownership to SQLite.
#[must_use = "dropping an unconsumed source removes its staging link"]
pub(crate) struct DerivedSource {
    pub object: ObjectRecord,
    pub(super) pin: SourcePin,
    snapshot_hashes: Option<Hashes>,
    activity: std::sync::Arc<super::super::Activity>,
}

pub(super) struct SourcePin {
    pub(super) backend: Option<BlobBackend>,
    pub(super) id: BlobId,
}

impl Drop for SourcePin {
    fn drop(&mut self) {
        if let Some(backend) = &self.backend
            && let Err(error) = backend.remove_pin(&self.id)
        {
            deferred_cleanup(&error);
        }
    }
}

pub(crate) struct PublicationPin {
    pub(super) pin: SourcePin,
    pub(super) input: BlobId,
    pub(super) hashes: Hashes,
    _activity: std::sync::Arc<super::super::Activity>,
}

fn link_unavailable(error: &BlobError) -> bool {
    let BlobError::Io(error) = error else {
        return false;
    };
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
        // Rust maps neither of these filesystem capability errors to Unsupported.
        if matches!(
            error
                .raw_os_error()
                .and_then(|code| u32::try_from(code).ok()),
            Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)
        ) {
            return true;
        }
    }
    matches!(
        error.kind(),
        io::ErrorKind::CrossesDevices
            | io::ErrorKind::Unsupported
            | io::ErrorKind::TooManyLinks
            | io::ErrorKind::PermissionDenied
    )
}

pub(crate) struct DerivedSegment {
    pub source: DerivedSource,
    pub offset: u64,
    pub length: u64,
}

/// The request a source is resolved for, which decides how a failed lookup is reported.
#[derive(Clone, Copy)]
enum SourceRole {
    /// A copy or rewrite source: a not-match that alone fails reports
    /// [`ObjectError::SourceNotModified`].
    Copied,
    /// A compose component: a missing component or a failed `ifGenerationMatch` names it, and
    /// a failed not-match is a failed precondition.
    Component,
}

impl SourceRole {
    fn request(self) -> objects::RequestKind {
        match self {
            Self::Copied => objects::RequestKind::CopySource,
            Self::Component => objects::RequestKind::Write,
        }
    }
}

/// Turns a missing source or a failed `ifGenerationMatch` into the compose component error.
fn name_component(
    connection: &Connection,
    source: &DerivedSourceSpec,
    error: ObjectError,
) -> ObjectError {
    let kind = match (&error, source.conditions.if_generation_match) {
        (ObjectError::ObjectNotFound, _) => ComponentFailureKind::NotFound,
        (ObjectError::PreconditionFailed, Some(required)) => {
            match objects::lookup(connection, &source.target.bucket, &source.target.name) {
                Ok(Some(live)) => ComponentFailureKind::GenerationMismatch {
                    required,
                    actual: live.record.generation,
                },
                // Deleted meanwhile: the precondition failure stands. Naming the component is
                // best-effort, so a failed lookup leaves it standing too.
                Ok(None) | Err(_) => return error,
            }
        }
        _ => return error,
    };
    ObjectError::Component(Box::new(ComponentFailure {
        bucket: source.target.bucket.clone(),
        name: source.target.name.clone(),
        kind,
    }))
}

#[derive(Clone, Debug)]
pub(crate) struct DerivedSourceSpec {
    pub(crate) target: ObjectTarget,
    pub(crate) conditions: ObjectConditions,
    pub(crate) access: BucketAccess,
}

impl DerivedSource {
    pub(crate) async fn publication_pin(&self) -> Result<Option<PublicationPin>, ObjectError> {
        let Some(backend) = self.pin.backend.clone() else {
            return Ok(None);
        };
        let input = self.pin.id;
        let hashes = self.object.hashes();
        let size = self.object.size;
        // A cancelled caller can leave the blocking link operation running until it returns.
        let activity = self.activity.clone();
        if let Some(hashes) = self.snapshot_hashes {
            // The input row owns this private file until publication takes it over.
            return Ok(Some(PublicationPin {
                pin: SourcePin {
                    backend: None,
                    id: input,
                },
                input,
                hashes,
                _activity: activity,
            }));
        }
        crate::task::run_blocking("derived-publication-pin", move || {
            // A short file must still report the streaming reader's EOF. A longer file
            // must publish only the recorded range, never its unexpected trailing bytes.
            if let BlobBackend::Disk(root) = &backend
                && fs::metadata(input.staging_path(root))?.len() != size
            {
                return Ok(None);
            }
            let id = random_id()?;
            match backend.pin_staged(&input, &id) {
                Ok(()) => Ok(Some(PublicationPin {
                    pin: SourcePin {
                        backend: Some(backend),
                        id,
                    },
                    input,
                    hashes,
                    _activity: activity,
                })),
                Err(error) if link_unavailable(&error) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await?
    }

    pub(crate) fn whole(self) -> DerivedSegment {
        let length = self.object.size;
        DerivedSegment {
            source: self,
            offset: 0,
            length,
        }
    }
}

impl Catalog {
    /// Recheck grants without changing the adapter's selection-time source snapshot.
    pub(crate) fn check_derived_source_access_in_transaction(
        connection: &Connection,
        source: &ObjectRecord,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        objects::authorize_generation(
            connection,
            &source.bucket,
            &source.name,
            source.generation,
            access,
        )
    }

    /// Call only from a publication callback: the caller's transaction owns all checks/mutations.
    pub(crate) fn check_derived_source_in_transaction(
        connection: &Connection,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        objects::selected(
            connection,
            bucket,
            name,
            generation,
            conditions,
            super::super::objects::RequestKind::Write,
            access,
        )
        .map(|stored| stored.record)
    }

    /// Hard deletion is intentionally separate from versioning retirement. Any later callback
    /// failure rolls this deletion back together with the destination and deletion journal.
    pub(crate) fn delete_derived_source_in_transaction(
        connection: &sql::WriteTx<'_>,
        bucket: &str,
        name: &str,
        generation: i64,
        conditions: ObjectConditions,
        access: &BucketAccess,
        now: std::time::SystemTime,
    ) -> Result<(), ObjectError> {
        let objects::StoredObject {
            record: object,
            blob: id,
        } = objects::selected(
            connection,
            bucket,
            name,
            Some(generation),
            conditions,
            super::super::objects::RequestKind::Write,
            access,
        )?;
        object.check_retention(now)?;
        super::super::notification_outbox::enqueue(
            connection,
            crate::notification::Message {
                event: crate::notification::Event::Delete,
                object: &object,
                time: now,
                replacement: None,
            },
        )?;
        sql::exec(connection, "DELETE FROM objects WHERE blob_id = ?1", [&id])?;
        objects::journal_new_deletion(connection, &id)?;
        sql::exec(
            connection,
            "INSERT INTO derived_deletions SELECT ?1, id FROM derived_transfers WHERE publishing = 1",
            [&id],
        )?;
        Ok(())
    }

    /// Every API resolves a source with the request's key; tests use this keyless shorthand.
    #[cfg(test)]
    pub(crate) async fn resolve_derived_source(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<DerivedSource, ObjectError> {
        self.resolve_derived_sources(vec![DerivedSourceSpec {
            target,
            conditions,
            access: access.clone(),
        }])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| object_unavailable("missing resolved source"))
    }

    /// Keyless [`Self::resolve_derived_sources_keyed`] for tests.
    #[cfg(test)]
    pub(crate) async fn resolve_derived_sources(
        &self,
        sources: Vec<DerivedSourceSpec>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_derived_sources_keyed(sources, None).await
    }

    /// Resolves copy or rewrite sources in one catalog snapshot; every source must be readable
    /// with `key`. A source not-match that alone fails is [`ObjectError::SourceNotModified`],
    /// not a conditional read's [`ObjectError::NotModified`].
    pub(crate) async fn resolve_derived_sources_keyed(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_sources(sources, key, SourceRole::Copied).await
    }

    /// Resolves compose components in one catalog snapshot; every component must be readable
    /// with `key`, the one key compose shares. A missing component or a failed generation
    /// precondition names the component, as Cloud Storage's compose does.
    pub(crate) async fn resolve_compose_components(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_sources(sources, key, SourceRole::Component)
            .await
    }

    async fn resolve_sources(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
        role: SourceRole,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        if sources.is_empty() || sources.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived request requires 1 to 32 sources".into(),
            ));
        }
        let activity = std::sync::Arc::new(self.await_activity(Work::New).await?);
        let backend = self.0.storage.backend.clone();
        let (pinned, failure) = self
            .run(move |connection| -> Result<_, ObjectError> {
                let transaction = connection.transaction()?;
                let mut pinned = Vec::with_capacity(sources.len());
                for source in sources {
                    match pin_source(&transaction, &backend, &source, key, role, &activity) {
                        Ok(source) => pinned.push(source),
                        Err(error) => return Ok((pinned, Some(error))),
                    }
                }
                Ok((pinned, None))
            })
            .await?;
        let Some(error) = failure else {
            let mut resolved = Vec::with_capacity(pinned.len());
            for (source, file) in pinned {
                let source = if let Some(file) = file {
                    // Keep the fallback's owned metadata out of the common request future.
                    Box::pin(self.copy_source_snapshot(source, file)).await?
                } else {
                    source
                };
                resolved.push(source);
            }
            return Ok(resolved);
        };
        // The pins a failed batch already made are unlinked on the blocking pool rather than in
        // the writer job, so the requests queued behind that job do not wait for the device.
        let released =
            crate::task::run_blocking("derived-source-release", move || drop(pinned)).await;
        if let Err(released) = released {
            deferred_cleanup(&released);
        }
        Err(error)
    }

    // Opening the source in the selecting transaction keeps its bytes alive across replacement.
    async fn copy_source_snapshot(
        &self,
        mut source: DerivedSource,
        file: fs::File,
    ) -> Result<DerivedSource, ObjectError> {
        let permit = self.continue_object_upload().await?;
        let catalog = self.clone();
        crate::task::run_blocking("derived-source-snapshot", move || {
            let root = &catalog.0.storage.data_dir;
            let mut output = objects::BatchWriter::create(
                source.pin.id.staging_path(root),
                std::sync::Arc::new(permit),
                #[cfg(test)]
                root,
            )?;
            source.pin.backend = Some(catalog.0.storage.backend.clone());
            #[cfg(all(test, unix))]
            fault::file_created(&source.pin.id.staging_path(root));
            let mut reader = InputReader::Disk(file);
            let mut hasher = Hasher::default();
            objects::with_copy_buffer(|buffer| {
                catalog.copy_range(
                    &mut reader,
                    source.object.size,
                    buffer,
                    |bytes| {
                        hasher.update(bytes);
                        output.write(bytes)
                    },
                    InputReader::short_derived_input,
                )
            })?;
            output.make_durable()?;
            source.snapshot_hashes = Some(hasher.finish());
            Ok(source)
        })
        .await?
    }
}

/// Selects one source in the resolving transaction and links its immutable bytes.
fn pin_source(
    transaction: &rusqlite::Transaction<'_>,
    backend: &super::super::BlobBackend,
    source: &DerivedSourceSpec,
    key: Option<crate::csek::CustomerKey>,
    role: SourceRole,
    activity: &std::sync::Arc<super::super::Activity>,
) -> Result<(DerivedSource, Option<fs::File>), ObjectError> {
    let objects::StoredObject {
        record: object,
        blob: original,
    } = objects::selected(
        transaction,
        &source.target.bucket,
        &source.target.name,
        source.target.generation,
        source.conditions,
        role.request(),
        &source.access,
    )
    .map_err(|error| match role {
        SourceRole::Copied => error,
        SourceRole::Component => name_component(transaction, source, error),
    })?;
    super::super::customer_keys::check(&object, key)?;
    let id = random_id()?;
    let file = match backend.pin(&original, &id) {
        Ok(()) => None,
        Err(error) if matches!(role, SourceRole::Copied) && link_unavailable(&error) => {
            let BlobBackend::Disk(root) = backend else {
                return Err(error.into());
            };
            Some(fs::File::open(original.blob_path(root))?)
        }
        Err(error) => return Err(error.into()),
    };
    Ok((
        DerivedSource {
            object,
            pin: SourcePin {
                backend: file.is_none().then(|| backend.clone()),
                id,
            },
            snapshot_hashes: None,
            activity: activity.clone(),
        },
        file,
    ))
}
