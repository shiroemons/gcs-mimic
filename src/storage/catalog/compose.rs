use super::{
    Catalog, DerivedDestination, DerivedSource, DerivedSourceSpec, compose_crc32c::combine, objects,
};
use crate::{
    bucket::BucketAccess,
    clock::Clock,
    object::{ComposeSource, ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord},
};
use rusqlite::Connection;
use std::{collections::BTreeSet, sync::Arc};

use crate::storage::failure::object_unavailable;

/// Whether a compose deletes its source objects once the destination is published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceDisposal {
    Keep,
    Delete,
}

fn incarnation(connection: &Connection, bucket: &str) -> Result<String, ObjectError> {
    connection
        .prepare_cached("SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1")
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
        .map_err(object_unavailable)
}

/// The source identities a compose read, re-checked when it publishes.
type SourceKeys = BTreeSet<(String, i64)>;

/// Combines source checksums and component counts into the destination's expectations.
fn combine_sources(
    sources: &[DerivedSource],
    disposal: SourceDisposal,
    clock: &dyn Clock,
    destination: &mut DerivedDestination,
) -> Result<SourceKeys, ObjectError> {
    let class = sources[0].object.storage_class.clone();
    let mut crc = 0;
    let mut count = 0_i64;
    let mut keys = BTreeSet::new();
    for source in sources {
        if source.object.storage_class != class {
            return Err(ObjectError::InvalidDerivedArgument(
                "Compose sources must use the same storage class".into(),
            ));
        }
        if disposal == SourceDisposal::Delete {
            source.object.check_retention(clock.now())?;
        }
        crc = combine(
            crc,
            source.object.crc32c,
            usize::try_from(source.object.size).map_err(object_unavailable)?,
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
    Ok(keys)
}

/// What a compose observed before reading its sources, re-checked and acted on inside the
/// publication transaction.
#[derive(Clone)]
struct ComposePlan {
    bucket: String,
    /// The bucket incarnation the destination check saw; a recreated bucket fails publication.
    expected_incarnation: String,
    /// The destination grant narrowed to reads, which also authorizes source deletion.
    read_access: BucketAccess,
    disposal: SourceDisposal,
    keys: SourceKeys,
}

impl ComposePlan {
    /// The bucket is unchanged and, when sources are deleted, the caller may still delete
    /// them and their retention allows it.
    fn validate(&self, connection: &Connection, clock: &dyn Clock) -> Result<(), ObjectError> {
        let (_, delete) = objects::authorized_bucket(connection, &self.bucket, &self.read_access)?;
        if incarnation(connection, &self.bucket)? != self.expected_incarnation {
            return Err(ObjectError::PreconditionFailed);
        }
        if self.disposal == SourceDisposal::Keep {
            return Ok(());
        }
        if delete == objects::DeletePermission::Denied {
            return Err(ObjectError::Forbidden);
        }
        for (name, generation) in &self.keys {
            let source = Catalog::check_derived_source_in_transaction(
                connection,
                &self.bucket,
                name,
                Some(*generation),
                ObjectConditions::default(),
                &self.read_access,
            )?;
            source.check_retention(clock.now())?;
        }
        Ok(())
    }

    /// Deletes every source except the destination itself when the compose asked for it.
    fn dispose_sources(
        &self,
        connection: &Connection,
        record: &ObjectRecord,
        clock: &dyn Clock,
    ) -> Result<(), ObjectError> {
        if self.disposal == SourceDisposal::Keep {
            return Ok(());
        }
        for (name, generation) in &self.keys {
            // The destination follows normal overwrite/versioning rules, never source deletion.
            if *name == record.name {
                continue;
            }
            Catalog::delete_derived_source_in_transaction(
                connection,
                &record.bucket,
                name,
                *generation,
                ObjectConditions::default(),
                &self.read_access,
                clock.now(),
            )?;
        }
        Ok(())
    }
}

/// The read access a compose needs on its sources: the destination grant, narrowed to reads.
fn source_access(destination: &BucketAccess) -> BucketAccess {
    let mut access = destination.clone();
    if let BucketAccess::Policy { permission, .. } = &mut access {
        *permission = crate::auth::Permission::ObjectsGet;
    }
    access
}

impl Catalog {
    pub(crate) async fn compose_object(
        &self,
        destination: DerivedDestination,
        sources: Vec<ComposeSource>,
        disposal: SourceDisposal,
        max_bytes: u64,
    ) -> Result<ObjectRecord, ObjectError> {
        let catalog = self.clone();
        // A complete JSON request owns its cleanup even when the HTTP response is cancelled.
        crate::task::spawn_supervised("compose", async move {
            catalog
                .compose_detached(destination, sources, disposal, max_bytes)
                .await
        })
        .await
        .map_err(object_unavailable)?
    }

    async fn compose_detached(
        &self,
        mut destination: DerivedDestination,
        sources: Vec<ComposeSource>,
        disposal: SourceDisposal,
        max_bytes: u64,
    ) -> Result<ObjectRecord, ObjectError> {
        if !(1..=32).contains(&sources.len()) {
            return Err(ObjectError::InvalidDerivedArgument(
                "Compose requires 1 to 32 sources".into(),
            ));
        }
        let access = source_access(&destination.access);
        let expected_incarnation = self
            .check_compose_destination(destination.clone(), access.clone(), disposal)
            .await?;
        let specs = sources
            .into_iter()
            .map(|source| DerivedSourceSpec {
                bucket: destination.bucket.clone(),
                name: source.name,
                generation: source.generation,
                conditions: ObjectConditions {
                    if_generation_match: source.if_generation_match,
                    ..Default::default()
                },
                access: access.clone(),
            })
            .collect();
        // Compose components must all use the request key, which also encrypts the result.
        let key = destination
            .metadata
            .customer_key_sha256
            .map(crate::csek::CustomerKey::from_sha256);
        let sources = self.resolve_derived_sources_keyed(specs, key).await?;
        let keys = combine_sources(&sources, disposal, &*self.0.clock, &mut destination)?;
        let transfer = self
            .begin_derived_ephemeral(
                sources.into_iter().map(|source| source.whole()).collect(),
                max_bytes,
            )
            .await?;
        let plan = ComposePlan {
            bucket: destination.bucket.clone(),
            expected_incarnation,
            read_access: access,
            disposal,
            keys,
        };
        let result = self.publish_composed(&transfer, destination, plan).await;
        if result.is_err() {
            match self.cancel_derived(&transfer.id).await {
                Ok(()) | Err(ObjectError::DerivedTransferNotFound) => {}
                Err(error) => crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "compose_cleanup_deferred",
                    serde_json::json!({"message":format!("deferred compose cleanup: {error}")}),
                ),
            }
        }
        result
    }

    /// Checks destination preconditions before any source is read; returns the bucket
    /// incarnation that publication must still observe.
    async fn check_compose_destination(
        &self,
        destination: DerivedDestination,
        read_access: BucketAccess,
        disposal: SourceDisposal,
    ) -> Result<String, ObjectError> {
        let clock = self.0.clock.clone();
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(object_unavailable)?;
            let (_, destination_delete) =
                objects::authorized_bucket(&transaction, &destination.bucket, &destination.access)?;
            let (_, source_delete) =
                objects::authorized_bucket(&transaction, &destination.bucket, &read_access)?;
            if disposal == SourceDisposal::Delete
                && source_delete == objects::DeletePermission::Denied
            {
                return Err(ObjectError::Forbidden);
            }
            let previous = objects::lookup(&transaction, &destination.bucket, &destination.name)?;
            objects::check_publication(
                previous.as_ref(),
                destination.conditions,
                destination_delete,
                clock.now(),
            )?;
            incarnation(&transaction, &destination.bucket)
        })
        .await
    }

    async fn publish_composed(
        &self,
        transfer: &super::derived::DerivedTransfer,
        destination: DerivedDestination,
        plan: ComposePlan,
    ) -> Result<ObjectRecord, ObjectError> {
        self.advance_derived(&transfer.id, transfer.total).await?;
        let checked = plan.clone();
        let check_clock: Arc<dyn Clock> = self.0.clock.clone();
        let delete_clock = check_clock.clone();
        self.publish_derived_with_validation(
            &transfer.id,
            destination,
            move |connection, _| checked.validate(connection, &*check_clock),
            move |connection, record| plan.dispose_sources(connection, record, &*delete_clock),
        )
        .await
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
                .compose_object(
                    destination(),
                    vec![source(), source()],
                    SourceDisposal::Delete,
                    11
                )
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
            .compose_object(
                destination(),
                vec![source(), source()],
                SourceDisposal::Keep,
                100,
            )
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
