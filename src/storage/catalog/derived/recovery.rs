use super::*;

#[expect(
    clippy::disallowed_methods,
    reason = "deferred derived deletions are reclaimed only during open-time recovery"
)]
fn reclaim_derived_deletions(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    let ids = connection
        .prepare_cached("SELECT blob_id FROM derived_deletions ORDER BY blob_id LIMIT 128")?
        .query_map([], |row| row.get::<_, BlobId>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut failure = None;
    for id in ids {
        match objects::reclaim(connection, data_dir, &id) {
            Ok(()) => {
                connection.execute("DELETE FROM derived_deletions WHERE blob_id = ?1", [id])?;
            }
            Err(error) => failure = Some(error),
        }
    }
    failure.map_or(Ok(()), Err)
}

fn discard_record(connection: &Connection, id: &BlobId) -> Result<(), ObjectError> {
    let transaction = connection.unchecked_transaction()?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_inputs WHERE transfer_id = ?1",
        [id],
    )?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_transfers WHERE id = ?1",
        [id],
    )?;
    transaction.commit().map_err(object_unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "transfer quotas and each retained transfer are inspected once during open-time recovery"
)]
pub(in crate::storage::catalog) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), ObjectError> {
    reclaim_derived_deletions(connection, data_dir)?;
    let over_limit: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM derived_transfers GROUP BY ephemeral HAVING count(*) > ?1)",
        [MAX_TRANSFERS_PER_KIND],
        |row| row.get(0),
    )?;
    if over_limit {
        return Err(object_unavailable("too many derived transfers"));
    }
    let ids = connection
        .prepare_cached(
            "SELECT id, ephemeral, deadline FROM derived_transfers ORDER BY id LIMIT ?1",
        )?
        .query_map([2 * MAX_TRANSFERS_PER_KIND + 1], |row| {
            Ok((
                row.get::<_, BlobId>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > 2 * MAX_TRANSFERS_PER_KIND {
        return Err(object_unavailable("too many derived transfers"));
    }
    for (id, ephemeral, deadline) in ids {
        if ephemeral || deadline <= now {
            discard_record(connection, &id)?;
            continue;
        }
        let validity = (|| {
            let (progress, saved_hash) = transfer(connection, &id, now)?;
            let sources = inputs(connection, &id)?;
            if sources.is_empty()
                || sources.len() > 32
                || sources
                    .iter()
                    .try_fold(0_u64, |total, source| total.checked_add(source.length))
                    != Some(progress.total)
            {
                return Err(object_unavailable("invalid derived sources"));
            }
            super::super::blob_backend::restore_disk_draft(data_dir, &id)?;
            let stage = stage_path(data_dir, &id);
            let mut output = fs::OpenOptions::new().write(true).read(true).open(stage)?;
            if output.metadata()?.len() < progress.offset {
                return Err(object_unavailable(
                    "derived prefix is shorter than its checkpoint",
                ));
            }
            output.set_len(progress.offset)?;
            #[cfg(test)]
            fault::truncated(data_dir, &output.metadata()?);
            output.sync_all()?;
            // A same-length corrupted prefix must not inherit the acknowledged hash state.
            let mut hasher = Hasher::default();
            let mut remaining = progress.offset;
            let mut buffer = [0; BUFFER_SIZE];
            while remaining > 0 {
                let length = usize::try_from(remaining.min(BUFFER_SIZE as u64))
                    .map_err(object_unavailable)?;
                let bytes = buffer
                    .get_mut(..length)
                    .ok_or_else(|| object_unavailable("invalid derived buffer length"))?;
                std::io::Read::read_exact(&mut output, bytes)?;
                hasher.update(bytes);
                remaining -= length as u64;
            }
            if hasher.finish() != saved_hash.finish() {
                return Err(object_unavailable(
                    "derived prefix checksum differs from checkpoint",
                ));
            }
            for source in sources {
                let metadata = fs::symlink_metadata(stage_path(data_dir, &source.id))?;
                if !metadata.is_file() || metadata.len() != source.size {
                    return Err(object_unavailable("invalid pinned derived source"));
                }
            }
            Ok::<_, ObjectError>(())
        })();
        if let Err(error) = validity {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "derived_transfer_discarded",
                serde_json::json!({"message":format!("discarding invalid derived transfer {id}: {error}")}),
            );
            discard_record(connection, &id)?;
        }
    }
    Ok(())
}
