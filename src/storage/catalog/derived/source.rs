use super::*;

/// A metadata snapshot and independent link to immutable bytes, including after source deletion.
/// Unconsumed handles clean their staging link; `begin_derived` transfers ownership to SQLite.
pub(crate) struct DerivedSource {
    pub object: ObjectRecord,
    pub(super) backend: Option<super::super::BlobBackend>,
    pub(super) id: BlobId,
    _activity: std::sync::Arc<super::super::Activity>,
}

impl Drop for DerivedSource {
    fn drop(&mut self) {
        if let Some(backend) = &self.backend
            && let Err(error) = backend.remove_pin(&self.id)
        {
            deferred_cleanup(&error);
        }
    }
}

pub(crate) struct DerivedSegment {
    pub source: DerivedSource,
    pub offset: u64,
    pub length: u64,
}

/// Whether a failed source lookup keeps the generic error or names the failing component.
#[derive(Clone, Copy)]
enum SourceFailures {
    Plain,
    NameComponent,
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
                _ => return error,
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
        connection: &Connection,
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
        sql::exec(
            connection,
            "INSERT INTO pending_blob_deletions VALUES (?1)",
            [&id],
        )?;
        sql::exec(
            connection,
            "INSERT INTO derived_deletions SELECT ?1, id FROM derived_transfers WHERE publishing = 1",
            [&id],
        )?;
        super::super::change_journal::object(
            connection,
            super::super::change_journal::origin(access),
            super::super::change_journal::ChangeKind::Delete,
            &object,
        )?;
        Ok(())
    }

    /// Source NotMatch conditions are mutation conditions (412), not conditional GETs (304).
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

    /// A compose request observes one catalog snapshot for all source generations and metadata.
    pub(crate) async fn resolve_derived_sources(
        &self,
        sources: Vec<DerivedSourceSpec>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_derived_sources_keyed(sources, None).await
    }

    /// Every source must be readable with `key` (compose requires one shared key).
    pub(crate) async fn resolve_derived_sources_keyed(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_sources(sources, key, SourceFailures::Plain)
            .await
    }

    /// As [`Self::resolve_derived_sources_keyed`], but a missing component or a failed
    /// generation precondition names the component, as Cloud Storage's compose does.
    pub(crate) async fn resolve_compose_components(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_sources(sources, key, SourceFailures::NameComponent)
            .await
    }

    async fn resolve_sources(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
        failures: SourceFailures,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        if sources.is_empty() || sources.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived request requires 1 to 32 sources".into(),
            ));
        }
        let activity = std::sync::Arc::new(self.await_activity(Work::New).await?);
        let backend = self.0.backend.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            sources
                .into_iter()
                .map(|source| {
                    let objects::StoredObject {
                        record: object,
                        blob: original,
                    } = objects::selected(
                        &transaction,
                        &source.target.bucket,
                        &source.target.name,
                        source.target.generation,
                        source.conditions,
                        super::super::objects::RequestKind::Write,
                        &source.access,
                    )
                    .map_err(|error| match failures {
                        SourceFailures::Plain => error,
                        SourceFailures::NameComponent => {
                            name_component(&transaction, &source, error)
                        }
                    })?;
                    super::super::customer_keys::check(&object, key)?;
                    let id = random_id()?;
                    backend.pin(&original, &id)?;
                    #[cfg(all(test, unix))]
                    if let super::super::BlobBackend::Disk(root) = &backend {
                        fault::file_created(&id.staging_path(root));
                    }
                    Ok(DerivedSource {
                        object,
                        backend: Some(backend.clone()),
                        id,
                        _activity: activity.clone(),
                    })
                })
                .collect()
        })
        .await
    }
}
