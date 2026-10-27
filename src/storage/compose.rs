use super::{Catalog, DerivedDestination, DerivedSourceSpec, compose_crc32c::combine, objects};
use crate::{
    bucket::BucketAccess,
    object::{ComposeSource, ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord},
};
use rusqlite::Connection;
use std::collections::BTreeSet;

fn unavailable(error: impl std::fmt::Display) -> ObjectError {
    ObjectError::Unavailable(error.to_string())
}

fn incarnation(connection: &Connection, bucket: &str) -> Result<String, ObjectError> {
    connection
        .query_row(
            "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
            [bucket],
            |row| row.get(0),
        )
        .map_err(unavailable)
}

impl Catalog {
    pub(crate) async fn compose_object(
        &self,
        mut destination: DerivedDestination,
        sources: Vec<ComposeSource>,
        delete_sources: bool,
        max_bytes: u64,
    ) -> Result<ObjectRecord, ObjectError> {
        let catalog = self.clone();
        // A complete JSON request owns its cleanup even when the HTTP response is cancelled.
        tokio::spawn(async move {
            if !(1..=32).contains(&sources.len()) {
                return Err(ObjectError::InvalidDerivedArgument(
                    "Compose requires 1 to 32 sources".into(),
                ));
            }
            let bucket = destination.bucket.clone();
            let mut source_access = destination.access.clone();
            if let BucketAccess::Policy { permission, .. } = &mut source_access {
                *permission = crate::auth::Permission::ObjectsGet;
            }
            let initial = destination.clone();
            let read_access = source_access.clone();
            let expected_incarnation = catalog
                .run_object(move |connection| {
                    let transaction = connection.transaction().map_err(unavailable)?;
                    let (_, overwrite) =
                        objects::authorized_bucket(&transaction, &initial.bucket, &initial.access)?;
                    let (_, delete) =
                        objects::authorized_bucket(&transaction, &initial.bucket, &read_access)?;
                    if delete_sources && !delete {
                        return Err(ObjectError::Forbidden);
                    }
                    let previous = objects::lookup(&transaction, &initial.bucket, &initial.name)?;
                    objects::check_publication(previous.as_ref(), initial.conditions, overwrite)?;
                    incarnation(&transaction, &initial.bucket)
                })
                .await?;
            let specs = sources
                .into_iter()
                .map(|source| DerivedSourceSpec {
                    bucket: bucket.clone(),
                    name: source.name,
                    generation: source.generation,
                    conditions: ObjectConditions {
                        if_generation_match: source.if_generation_match,
                        ..Default::default()
                    },
                    access: source_access.clone(),
                })
                .collect();
            let sources = catalog.resolve_derived_sources(specs).await?;
            let class = sources[0].object.storage_class.clone();
            let mut crc = 0;
            let mut count = 0_i64;
            let mut keys = BTreeSet::new();
            for source in &sources {
                if source.object.storage_class != class {
                    return Err(ObjectError::InvalidDerivedArgument(
                        "Compose sources must use the same storage class".into(),
                    ));
                }
                if delete_sources && source.object.has_hold() {
                    return Err(ObjectError::ActiveHold);
                }
                crc = combine(
                    crc,
                    source.object.crc32c,
                    usize::try_from(source.object.size).map_err(unavailable)?,
                );
                count = count
                    .saturating_add(source.object.component_count.unwrap_or(1))
                    .min(i32::MAX as i64);
                keys.insert((source.object.name.clone(), source.object.generation));
            }
            destination.storage_class = Some(class);
            destination.component_count = Some(count);
            destination.expected = ExpectedHashes {
                md5: None,
                crc32c: Some(crc),
            };
            let transfer = catalog
                .begin_derived_ephemeral(
                    sources.into_iter().map(|source| source.whole()).collect(),
                    max_bytes,
                )
                .await?;
            let read_access = source_access.clone();
            let checked_bucket = bucket.clone();
            let checked_keys = keys.clone();
            let result = async {
                catalog
                    .advance_derived(&transfer.id, transfer.total)
                    .await?;
                catalog
                    .publish_derived_with_validation(
                        &transfer.id,
                        destination,
                        move |connection, _| {
                            let (_, delete) = objects::authorized_bucket(
                                connection,
                                &checked_bucket,
                                &read_access,
                            )?;
                            if incarnation(connection, &checked_bucket)? != expected_incarnation {
                                return Err(ObjectError::PreconditionFailed);
                            }
                            if delete_sources {
                                if !delete {
                                    return Err(ObjectError::Forbidden);
                                }
                                for (name, generation) in checked_keys {
                                    let source = Catalog::check_derived_source_in_transaction(
                                        connection,
                                        &checked_bucket,
                                        &name,
                                        Some(generation),
                                        ObjectConditions::default(),
                                        &read_access,
                                    )?;
                                    if source.has_hold() {
                                        return Err(ObjectError::ActiveHold);
                                    }
                                }
                            }
                            Ok(())
                        },
                        move |connection, record| {
                            if delete_sources {
                                for (name, generation) in keys {
                                    // The destination follows normal overwrite/versioning rules, never source deletion.
                                    if name == record.name {
                                        continue;
                                    }
                                    Catalog::delete_derived_source_in_transaction(
                                        connection,
                                        &bucket,
                                        &name,
                                        generation,
                                        ObjectConditions::default(),
                                        &source_access,
                                    )?;
                                }
                            }
                            Ok(())
                        },
                    )
                    .await
            }
            .await;
            if result.is_err() {
                match catalog.cancel_derived(&transfer.id).await {
                    Ok(()) | Err(ObjectError::DerivedTransferNotFound) => {}
                    Err(error) => crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "compose_cleanup_deferred",
                        serde_json::json!({"message":format!("deferred compose cleanup: {error}")}),
                    ),
                }
            }
            result
        })
        .await
        .map_err(unavailable)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::UploadChunk;
    use bytes::Bytes;
    use std::{fs, path::PathBuf, time::UNIX_EPOCH};
    use tokio::sync::mpsc;

    async fn setup() -> (Catalog, PathBuf) {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path =
            std::env::temp_dir().join(format!("gcs-mimic-compose-{:x}", u64::from_be_bytes(nonce)));
        let catalog = Catalog::open(&path).await.unwrap();
        let bucket =
            crate::json::parse_insert(br#"{"name":"bucket"}"#, "project", UNIX_EPOCH).unwrap();
        catalog.insert_bucket(bucket).await.unwrap();
        let (sender, receiver) = mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"source"))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        catalog
            .put_object(
                "bucket",
                "source",
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                100,
                receiver,
            )
            .await
            .unwrap();
        (catalog, path)
    }

    fn destination() -> DerivedDestination {
        DerivedDestination {
            bucket: "bucket".into(),
            name: "destination".into(),
            ..DerivedDestination::new(BucketAccess::Permissive)
        }
    }

    fn source() -> ComposeSource {
        ComposeSource {
            name: "source".into(),
            generation: None,
            if_generation_match: None,
        }
    }

    #[tokio::test]
    async fn composed_size_cap_preserves_sources_and_leaves_no_transfer() {
        let (catalog, path) = setup().await;
        assert_eq!(
            catalog
                .compose_object(destination(), vec![source(), source()], true, 11)
                .await,
            Err(ObjectError::TooLarge)
        );
        assert_eq!(
            catalog
                .get_object("bucket", "source", None, ObjectConditions::default())
                .await
                .unwrap()
                .size,
            6
        );
        assert_eq!(
            catalog
                .get_object("bucket", "destination", None, ObjectConditions::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        let connection = Connection::open(path.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        drop(connection);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn component_count_saturates_and_success_cleans_temporary_links() {
        let (catalog, path) = setup().await;
        let connection = Connection::open(path.join("catalog.sqlite3")).unwrap();
        connection
            .execute(
                "UPDATE objects SET component_count = 2147483647 WHERE name = 'source'",
                [],
            )
            .unwrap();
        let result = catalog
            .compose_object(destination(), vec![source(), source()], false, 100)
            .await
            .unwrap();
        assert_eq!(result.component_count, Some(i32::MAX as i64));
        assert_eq!(result.size, 12);
        assert_eq!(result.crc32c, crc32c::crc32c(b"sourcesource"));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        drop(connection);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
