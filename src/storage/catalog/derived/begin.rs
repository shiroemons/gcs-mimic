use super::*;

/// The transfer's total length; every range must lie inside its source.
fn derived_total(segments: &[DerivedSegment], max_bytes: u64) -> Result<u64, ObjectError> {
    segments.iter().try_fold(0_u64, |total, segment| {
        if segment.offset > segment.source.object.size
            || segment.length > segment.source.object.size - segment.offset
        {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived source range is out of bounds".into(),
            ));
        }
        total
            .checked_add(segment.length)
            .filter(|total| *total <= max_bytes && i64::try_from(*total).is_ok())
            .ok_or(ObjectError::TooLarge)
    })
}

fn insert_inputs(
    transaction: &rusqlite::Transaction<'_>,
    id: &BlobId,
    segments: &[DerivedSegment],
) -> Result<(), ObjectError> {
    let mut statement =
        transaction.prepare_cached("INSERT INTO derived_inputs VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    for (position, segment) in segments.iter().enumerate() {
        statement.execute(params![
            id,
            position,
            segment.source.id,
            segment.offset,
            segment.length,
            segment.source.object.size
        ])?;
    }
    Ok(())
}

enum BeginOutcome {
    Committed(DerivedTransfer),
    Rejected(ObjectError),
    Ambiguous(ObjectError),
}

struct BeginTransfer {
    id: BlobId,
    total: u64,
    ephemeral: bool,
    created: i64,
    deadline: i64,
}

fn begin_transfer(
    connection: &mut Connection,
    draft: &BeginTransfer,
    segments: &[DerivedSegment],
    bind: impl FnOnce(&Connection, &DerivedTransfer) -> Result<(), ObjectError>,
) -> BeginOutcome {
    let mut ambiguous = false;
    let result = (|| {
        let count: usize = sql::scalar(
            connection,
            "SELECT count(*) FROM derived_transfers WHERE ephemeral = ?1",
            [draft.ephemeral],
        )?;
        if count >= MAX_TRANSFERS_PER_KIND {
            return Err(object_unavailable("derived transfer limit reached"));
        }
        let transaction = sql::immediate(connection)?;
        sql::exec(
            &transaction,
            "INSERT INTO derived_transfers VALUES (?1, ?2, 0, ?3, 0, ?4, ?5, ?6, 0)",
            params![
                draft.id,
                draft.total,
                Hasher::default().md5_state().0.as_slice(),
                draft.ephemeral,
                draft.created,
                draft.deadline
            ],
        )?;
        insert_inputs(&transaction, &draft.id, segments)?;
        let transfer = DerivedTransfer {
            id: draft.id,
            total: draft.total,
            offset: 0,
            created: system_time_millis(draft.created)?,
            deadline: system_time_millis(draft.deadline)?,
        };
        bind(&transaction, &transfer)?;
        #[cfg(test)]
        fault::hit(&fault::database_dir(&transaction), Step::DerivedBeginCommit)
            .map_err(object_unavailable)?;
        if let Err(error) = transaction.commit() {
            // Failed probes retain every possibly committed reference for recovery.
            ambiguous = true;
            let committed: bool = sql::scalar(
                connection,
                "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
                [&draft.id],
            )?;
            ambiguous = false;
            if !committed {
                return Err(object_unavailable(error));
            }
        }
        Ok(transfer)
    })();
    match result {
        Ok(transfer) => BeginOutcome::Committed(transfer),
        Err(error) if ambiguous => BeginOutcome::Ambiguous(error),
        Err(error) => BeginOutcome::Rejected(error),
    }
}

impl Catalog {
    /// Input handles are consumed after ownership commits. Byte transfer needs no later source
    /// row; protocol adapters may still recheck source metadata, existence and grants.
    #[cfg(test)]
    pub(crate) async fn begin_derived(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_kind(segments, max_bytes, TransferKind::Continuable)
            .await
    }

    /// Single-call requests have no continuation owner after a process crash.
    pub(crate) async fn begin_derived_ephemeral(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_kind(segments, max_bytes, TransferKind::Ephemeral)
            .await
    }

    pub(super) async fn begin_derived_with_kind(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
        kind: TransferKind,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_binding(segments, max_bytes, kind, |_, _| Ok(()))
            .await
    }

    /// Request/session bindings commit with transfer ownership, preventing untracked tokens.
    pub(crate) async fn begin_derived_with_binding(
        &self,
        mut segments: Vec<DerivedSegment>,
        max_bytes: u64,
        kind: TransferKind,
        bind: impl FnOnce(&Connection, &DerivedTransfer) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<DerivedTransfer, ObjectError> {
        let ephemeral = kind == TransferKind::Ephemeral;
        if segments.is_empty() || segments.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived transfer requires 1 to 32 sources".into(),
            ));
        }
        let total = derived_total(&segments, max_bytes)?;
        let admission = self
            .0
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|error| super::super::admission_error(&error))?;
        let permit = self.admit_object_upload()?;
        let backend = self.0.backend.clone();
        let created = unix_millis(self.0.clock.now())?;
        let deadline = created
            .checked_add(if ephemeral {
                EPHEMERAL_LIFETIME_MILLIS
            } else {
                LIFETIME_MILLIS
            })
            .ok_or_else(|| object_unavailable("derived deadline overflow"))?;
        let id = random_id()?;
        let writer = self.derived_guard(&id)?;
        let mut owner = ephemeral.then(|| self.guard_derived_request(&id));
        let catalog = self.clone();
        let result = crate::task::spawn_supervised("derived-begin", async move {
            let (_permit, _writer) = (permit, writer);
            let files = backend.clone();
            #[cfg(test)]
            let root = catalog.0.data_dir.clone();
            let prepared = crate::task::run_blocking("derived-create", move || {
                files.create_draft(&id)?;
                #[cfg(all(test, unix))]
                if let super::super::BlobBackend::Disk(root) = &files {
                    fault::file_created(&id.staging_path(root));
                }
                #[cfg(test)]
                fault::hit(&root, Step::DerivedDraftSync).map_err(object_unavailable)?;
                // This sync makes both the draft and the selected input links durable before SQL.
                #[cfg(unix)]
                files.sync_staging_dir()?;
                Ok::<_, ObjectError>(())
            })
            .await
            .map_err(object_unavailable)
            .flatten();
            let (outcome, returned) = match prepared {
                Ok(()) => {
                    catalog
                        .run_admitted(admission, move |connection| {
                            let outcome = begin_transfer(
                                connection,
                                &BeginTransfer {
                                    id,
                                    total,
                                    ephemeral,
                                    created,
                                    deadline,
                                },
                                &segments,
                                bind,
                            );
                            if !matches!(outcome, BeginOutcome::Rejected(_)) {
                                for segment in &mut segments {
                                    segment.source.backend = None;
                                }
                            }
                            Ok::<_, ObjectError>((outcome, segments))
                        })
                        .await?
                }
                Err(error) => (BeginOutcome::Rejected(error), segments),
            };
            let result = match outcome {
                BeginOutcome::Committed(transfer) => Ok(transfer),
                BeginOutcome::Ambiguous(error) => Err(error),
                BeginOutcome::Rejected(error) => {
                    let cleanup = crate::task::run_blocking("derived-begin-cleanup", move || {
                        drop(returned);
                        backend.remove_draft(&id)
                    })
                    .await;
                    match cleanup {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => deferred_cleanup(&error),
                        Err(error) => deferred_cleanup(&error),
                    }
                    Err(error)
                }
            };
            catalog.0.maintenance_wake.notify_one();
            result
        })
        .await?;
        if result.is_ok()
            && let Some(owner) = &mut owner
        {
            owner.disarm();
        }
        result
    }
}
