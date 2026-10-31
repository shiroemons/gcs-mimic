//! Payload snapshots belong to the object mutation transaction, independently of sync replay.

use super::{Catalog, unavailable, unix_micros};
use crate::{
    bucket::CatalogError,
    notification::{GlobalConfig, Message, Runtime},
};
use rusqlite::{Connection, OptionalExtension, params};

mod worker;

pub(super) const SCHEMA: &str = "CREATE TABLE notification_outbox_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    last_id INTEGER NOT NULL CHECK(typeof(last_id) = 'integer' AND last_id >= 0),
    pending_count INTEGER NOT NULL CHECK(pending_count BETWEEN 0 AND 4096),
    pending_bytes INTEGER NOT NULL CHECK(pending_bytes BETWEEN 0 AND 16777216)
) WITHOUT ROWID;
INSERT INTO notification_outbox_state VALUES (1, 0, 0, 0);
CREATE TABLE notification_outbox (
    id INTEGER PRIMARY KEY CHECK(id > 0),
    topic TEXT NOT NULL CHECK(length(CAST(topic AS BLOB)) <= 1024),
    message TEXT NOT NULL CHECK(length(CAST(message AS BLOB)) <= 131072 AND json_valid(message)),
    due INTEGER NOT NULL,
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 63)
);
CREATE INDEX notification_outbox_due ON notification_outbox(due, id);
CREATE TRIGGER notification_outbox_insert AFTER INSERT ON notification_outbox BEGIN
    UPDATE notification_outbox_state SET pending_count = pending_count + 1,
        pending_bytes = pending_bytes + length(CAST(NEW.message AS BLOB)) + length(CAST(NEW.topic AS BLOB)) WHERE singleton = 1;
END;
CREATE TRIGGER notification_outbox_delete AFTER DELETE ON notification_outbox BEGIN
    UPDATE notification_outbox_state SET pending_count = pending_count - 1,
        pending_bytes = pending_bytes - length(CAST(OLD.message AS BLOB)) - length(CAST(OLD.topic AS BLOB)) WHERE singleton = 1;
END;
PRAGMA user_version = 38;";

pub(super) fn open(connection: &Connection) -> Result<(), CatalogError> {
    connection.execute_batch("CREATE TEMP TABLE notification_runtime (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1), bucket TEXT NOT NULL, config TEXT NOT NULL
    ) WITHOUT ROWID;").map_err(unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "catalog integrity is validated once at open"
)]
pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row(
        "SELECT count(*) = 1 AND min(last_id) >= (SELECT coalesce(max(id), 0) FROM notification_outbox) AND min(pending_count) = (SELECT count(*) FROM notification_outbox) AND min(pending_bytes) = (SELECT coalesce(sum(length(CAST(message AS BLOB)) + length(CAST(topic AS BLOB))), 0) FROM notification_outbox) FROM notification_outbox_state", [], |row| row.get(0)
    ).map_err(unavailable)?;
    if !valid {
        return Err(CatalogError::Corrupt(
            "invalid notification outbox accounting".into(),
        ));
    }
    Ok(())
}

pub(super) fn enqueue(connection: &Connection, message: Message<'_>) -> Result<(), CatalogError> {
    let global: Option<(String, String)> = connection
        .prepare_cached("SELECT bucket, config FROM notification_runtime WHERE singleton = 1")
        .and_then(|mut statement| statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?))))
        .optional()
        .map_err(unavailable)?;
    if let Some((bucket, stored)) = global
        && (bucket.is_empty() || bucket == message.object.bucket)
    {
        let config = crate::notification::NotificationConfig::from_stored(&stored)
            .map_err(|reason| CatalogError::Corrupt(reason.into()))?;
        if message.matches(&config) {
            insert(
                connection,
                &config.topic,
                &message.encode(&config, None)?,
                &message,
            )?;
        }
    }
    for record in super::notifications::read_all(connection, &message.object.bucket)? {
        if message.matches(&record.config) {
            insert(
                connection,
                &record.config.topic,
                &message.encode(&record.config, Some(record.id))?,
                &message,
            )?;
        }
    }
    Ok(())
}

pub(super) fn published(
    connection: &Connection,
    origin: super::admin::Origin,
    record: &crate::object::ObjectRecord,
    previous: Option<&(crate::object::ObjectRecord, String)>,
    time: std::time::SystemTime,
) -> Result<(), CatalogError> {
    if origin == super::admin::Origin::Import {
        return Ok(());
    }
    enqueue(
        connection,
        Message {
            event: crate::notification::Event::Finalize,
            object: record,
            time,
            replacement: previous.map(|(object, _)| object.generation),
        },
    )
}

#[derive(Debug)]
struct OutboxFull;

impl std::fmt::Display for OutboxFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("notification outbox limit reached")
    }
}

impl std::error::Error for OutboxFull {}

impl crate::storage::failure::StorageCause for OutboxFull {
    fn into_cause(self) -> crate::storage::failure::BackendCause {
        crate::storage::failure::BackendCause::from_error(self)
    }
}

fn insert(
    connection: &Connection,
    topic: &str,
    encoded: &str,
    message: &Message<'_>,
) -> Result<(), CatalogError> {
    let bytes = i64::try_from(topic.len() + encoded.len()).map_err(unavailable)?;
    let full: bool = connection.prepare_cached("SELECT pending_count >= 4096 OR pending_bytes + ?1 > 16777216 FROM notification_outbox_state WHERE singleton = 1")
        .and_then(|mut statement| statement.query_row([bytes], |row| row.get(0))).map_err(unavailable)?;
    if full {
        return Err(unavailable(OutboxFull));
    }
    let id: i64 = connection.prepare_cached(
        "UPDATE notification_outbox_state SET last_id = last_id + 1 WHERE singleton = 1 AND last_id < 9223372036854775807 RETURNING last_id"
    ).and_then(|mut statement| statement.query_row([], |row| row.get(0))).optional().map_err(unavailable)?
        .ok_or_else(|| unavailable("notification outbox ID exhausted"))?;
    let due = unix_micros(message.time)?;
    connection
        .prepare_cached("INSERT INTO notification_outbox VALUES (?1, ?2, ?3, ?4, 0)")
        .and_then(|mut statement| statement.execute(params![id, topic, encoded, due]))
        .map_err(unavailable)?;
    Ok(())
}

pub(crate) struct Pending {
    pub(crate) id: i64,
    pub(crate) topic: String,
    pub(crate) message: String,
    pub(crate) due: i64,
    pub(crate) attempts: u32,
}

impl Catalog {
    pub(crate) async fn configure_notifications(
        &self,
        runtime: &Runtime,
    ) -> Result<(), CatalogError> {
        let global: Option<GlobalConfig> = runtime.global.clone();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            transaction
                .prepare_cached("DELETE FROM notification_runtime")
                .and_then(|mut statement| statement.execute([]))
                .map_err(unavailable)?;
            if let Some(global) = global {
                transaction
                    .prepare_cached("INSERT INTO notification_runtime VALUES (1, ?1, ?2)")
                    .and_then(|mut statement| {
                        statement.execute(params![global.bucket, global.notification.to_stored()])
                    })
                    .map_err(unavailable)?;
            }
            transaction.commit().map_err(unavailable)
        })
        .await
    }

    pub(crate) async fn next_notification(&self) -> Result<Option<Pending>, CatalogError> {
        self.run(|connection| {
            connection.prepare_cached("SELECT id, topic, message, due, attempts FROM notification_outbox ORDER BY due, id LIMIT 1")
                .and_then(|mut statement| statement.query_row([], |row| Ok(Pending {
                    id: row.get(0)?, topic: row.get(1)?, message: row.get(2)?, due: row.get(3)?, attempts: row.get(4)?,
                }))).optional().map_err(unavailable)
        }).await
    }

    pub(crate) async fn acknowledge_notification(&self, id: i64) -> Result<(), CatalogError> {
        self.run(move |connection| {
            connection
                .prepare_cached("DELETE FROM notification_outbox WHERE id = ?1")
                .and_then(|mut statement| statement.execute([id]))
                .map_err(unavailable)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn retry_notification(&self, id: i64, due: i64) -> Result<(), CatalogError> {
        self.run(move |connection| {
            connection.prepare_cached("UPDATE notification_outbox SET due = ?2, attempts = min(attempts + 1, 63) WHERE id = ?1")
                .and_then(|mut statement| statement.execute(params![id, due])).map_err(unavailable)?;
            Ok(())
        }).await
    }
}

#[cfg(test)]
mod tests;
