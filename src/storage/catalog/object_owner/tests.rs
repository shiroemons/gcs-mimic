use super::*;
use crate::auth::{Auth, Registry};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

fn strict(identities: serde_json::Value) -> Profile {
    let auth = Auth::strict(
        Registry::parse(json!({"identities":identities}).to_string().as_bytes()).unwrap(),
    );
    Profile::Strict(Arc::new(
        auth.principals()
            .unwrap()
            .iter()
            .map(|identity| (identity.id().to_owned(), identity.clone()))
            .collect::<BTreeMap<_, _>>(),
    ))
}

#[test]
fn owner_migration_keeps_unknown_uploaders_and_rolls_back_before_retry() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE objects(name TEXT PRIMARY KEY, generation INTEGER, metageneration INTEGER); INSERT INTO objects VALUES('legacy',7,3); PRAGMA user_version=43;").unwrap();
    let transaction = connection.transaction().unwrap();
    transaction.execute_batch(SCHEMA).unwrap();
    transaction.rollback().unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        43
    );
    connection.execute_batch(SCHEMA).unwrap();
    let legacy: (i64, i64, Option<String>) = connection
        .query_row(
            "SELECT generation,metageneration,owner FROM objects",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(legacy, (7, 3, None));
    for invalid in ["not-json", "[]", "null", "1"] {
        assert!(
            connection
                .execute("UPDATE objects SET owner=?1", [invalid])
                .is_err()
        );
    }
    connection
        .execute("UPDATE objects SET owner='{}'", [])
        .unwrap();
    assert!(
        connection
            .query_row("SELECT owner FROM objects", [], |row| read(row, 0))
            .is_err()
    );
}

#[test]
fn canonical_owner_entities_require_current_registered_ids_and_keep_profile_boundaries() {
    let profile = strict(json!([
        {"id":"local:owner","tokens":[],"acl_entity":"user-current@example.test"},
        {"id":"local:aliasless","tokens":[]},
        {"id":"local:other","tokens":[],"acl_entity":"user-old@example.test"}
    ]));
    for (id, expected) in [
        ("local:owner", Some("user-current@example.test")),
        ("local:aliasless", None),
        ("deleted@example.test", None),
    ] {
        let mut owner = ObjectOwner::Principal {
            id: id.into(),
            entity: Some("user-old@example.test".into()),
        };
        canonicalize(&mut owner, Some(&profile));
        assert_eq!(owner.entity().as_deref(), expected);
        assert!(matches!(owner, ObjectOwner::Principal { id: actual, .. } if actual == id));
    }
    let original = ObjectOwner::Principal {
        id: "local:owner".into(),
        entity: Some("user-original@example.test".into()),
    };
    for profile in [None, Some(Profile::Permissive), Some(Profile::TrustedLocal)] {
        let mut owner = original.clone();
        canonicalize(&mut owner, profile.as_ref());
        assert_eq!(owner, original);
    }
    let mut project = ObjectOwner::ProjectOwners {
        project: "actual-project".into(),
    };
    canonicalize(&mut project, Some(&profile));
    assert_eq!(
        project,
        ObjectOwner::ProjectOwners {
            project: "actual-project".into()
        }
    );
}

#[test]
fn interrupted_owner_reconciliation_retries_with_new_registry_without_changing_identity_or_metadata()
 {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE objects(bucket TEXT,name TEXT,generation INTEGER,metageneration INTEGER,updated INTEGER,bytes BLOB,owner TEXT,PRIMARY KEY(bucket,name,generation)); CREATE TABLE changes(name TEXT); CREATE TEMP TRIGGER changed AFTER UPDATE OF owner ON objects BEGIN INSERT INTO changes VALUES(NEW.name); END; CREATE TEMP TRIGGER fail_second BEFORE UPDATE OF owner ON objects WHEN OLD.name='second' BEGIN SELECT RAISE(ABORT,'injected owner update failure'); END;").unwrap();
    for name in ["first", "second"] {
        let owner = ObjectOwner::Principal {
            id: "local:owner".into(),
            entity: Some("user-old@example.test".into()),
        };
        connection
            .execute(
                "INSERT INTO objects VALUES('bucket',?1,7,3,42,x'010203',?2)",
                params![name, serde_json::to_string(&owner).unwrap()],
            )
            .unwrap();
    }
    let first =
        strict(json!([{"id":"local:owner","tokens":[],"acl_entity":"user-first@example.test"}]));
    assert!(reconcile(&mut connection, &first).is_err());
    assert_eq!(
        selected(&connection, "bucket", "first", 7)
            .unwrap()
            .unwrap()
            .entity()
            .as_deref(),
        Some("user-first@example.test")
    );
    assert_eq!(
        selected(&connection, "bucket", "second", 7)
            .unwrap()
            .unwrap()
            .entity()
            .as_deref(),
        Some("user-old@example.test")
    );
    connection
        .execute_batch("DROP TRIGGER fail_second;")
        .unwrap();
    let current = strict(json!([
        {"id":"local:owner","tokens":[],"acl_entity":"user-current@example.test"},
        {"id":"local:other","tokens":[],"acl_entity":"user-first@example.test"}
    ]));
    reconcile(&mut connection, &current).unwrap();
    let changes: i64 = connection
        .query_row("SELECT COUNT(*) FROM changes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(changes, 3);
    reconcile(&mut connection, &current).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM changes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        changes
    );
    for name in ["first", "second"] {
        assert_eq!(
            selected(&connection, "bucket", name, 7).unwrap(),
            Some(ObjectOwner::Principal {
                id: "local:owner".into(),
                entity: Some("user-current@example.test".into())
            })
        );
        let unchanged: (i64, i64, i64, Vec<u8>) = connection
            .query_row(
                "SELECT generation,metageneration,updated,bytes FROM objects WHERE name=?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(unchanged, (7, 3, 42, vec![1, 2, 3]));
    }
}
