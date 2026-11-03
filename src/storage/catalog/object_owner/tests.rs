use super::*;
use crate::auth::{Auth, Registry};
use crate::storage::test_fixtures::PERMISSIVE;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn owner_acl_reconciliation_rejects_corrupt_stored_roles_and_json() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE acl_object_entries(bucket TEXT,name TEXT,generation INTEGER,entries TEXT);").unwrap();
    let owner = ObjectOwner::Principal {
        id: "owner".into(),
        entity: Some("user-old@example.test".into()),
    };
    for stored in [
        r#"[{"entity":"user-old@example.test","role":"UNKNOWN"}]"#,
        "not-json",
    ] {
        connection
            .execute("DELETE FROM acl_object_entries", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO acl_object_entries VALUES ('bucket','object',1,?1)",
                [stored],
            )
            .unwrap();
        assert!(
            matches!(
                reconcile_grant(&connection, ("bucket", "object", 1), &owner, &owner),
                Err(CatalogError::Corrupt(_))
            ),
            "{stored}"
        );
    }
}

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
    for profile in [
        None,
        Some(Profile::Permissive { creator: None }),
        Some(Profile::TrustedLocal),
    ] {
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
    connection.execute_batch("CREATE TABLE acl_object_entries(bucket TEXT,name TEXT,generation INTEGER,entries TEXT,PRIMARY KEY(bucket,name,generation));").unwrap();
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
        connection.execute("INSERT INTO acl_object_entries VALUES ('bucket',?1,7,?2)", params![name,
            json!([{"entity":"project-owners-0","role":"OWNER"},{"entity":"user-old@example.test","role":"OWNER"},{"entity":"allUsers","role":"READER"}]).to_string()]).unwrap();
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
    let stale: String = connection
        .query_row(
            "SELECT entries FROM acl_object_entries WHERE name='second'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stale.contains("user-old@example.test"));
    assert!(!stale.contains("user-first@example.test"));
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
        let entries: String = connection
            .query_row(
                "SELECT entries FROM acl_object_entries WHERE name=?1",
                [name],
                |row| row.get(0),
            )
            .unwrap();
        let entries: serde_json::Value = serde_json::from_str(&entries).unwrap();
        assert_eq!(
            entries,
            json!([{"entity":"project-owners-0","role":"OWNER"},{"entity":"user-current@example.test","role":"OWNER"},{"entity":"allUsers","role":"READER"}])
        );
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

fn permissive(email: Option<&str>) -> Profile {
    Profile::Permissive {
        creator: email.map(|email| Arc::from(format!("user-{email}"))),
    }
}

fn local_creator(entity: &str) -> ObjectOwner {
    ObjectOwner::Principal {
        id: LOCAL_CREATOR_ID.into(),
        entity: Some(entity.into()),
    }
}

#[test]
fn permissive_creator_owns_unauthenticated_publications_without_widening_other_paths() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE buckets(name TEXT, project TEXT); INSERT INTO buckets VALUES('b','project');")
        .unwrap();
    let with_creator = permissive(Some("who@example.test"));
    assert_eq!(
        for_publication(&connection, "b", PERMISSIVE, Some(&with_creator)).unwrap(),
        local_creator("user-who@example.test")
    );
    // No creator configured, no profile yet, or the legacy-style trusted profile keep the
    // project owners, which is also what existing data holds.
    for profile in [Some(permissive(None)), Some(Profile::TrustedLocal), None] {
        assert_eq!(
            for_publication(&connection, "b", PERMISSIVE, profile.as_ref()).unwrap(),
            ObjectOwner::ProjectOwners {
                project: "project".into()
            }
        );
    }
}

#[test]
fn permissive_canonicalize_follows_the_configured_address_only_for_the_local_creator() {
    let mut owner = local_creator("user-old@example.test");
    canonicalize(&mut owner, Some(&permissive(Some("new@example.test"))));
    assert_eq!(owner, local_creator("user-new@example.test"));
    // Disabling the creator keeps what was recorded.
    canonicalize(&mut owner, Some(&permissive(None)));
    assert_eq!(owner, local_creator("user-new@example.test"));
    // Strict restarts neither erase nor rewrite the recorded permissive creator.
    canonicalize(
        &mut owner,
        Some(&strict(json!([{"id":"local:owner","tokens":[]}]))),
    );
    assert_eq!(owner, local_creator("user-new@example.test"));
    let mut other = ObjectOwner::Principal {
        id: "local:owner".into(),
        entity: Some("user-x@example.test".into()),
    };
    canonicalize(&mut other, Some(&permissive(Some("new@example.test"))));
    assert_eq!(other.entity().as_deref(), Some("user-x@example.test"));
    let mut project = ObjectOwner::ProjectOwners {
        project: "project".into(),
    };
    canonicalize(&mut project, Some(&permissive(Some("new@example.test"))));
    assert!(matches!(project, ObjectOwner::ProjectOwners { .. }));
}

#[test]
fn permissive_reconcile_renames_the_creator_and_leaves_project_owner_rows() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE objects(bucket TEXT,name TEXT,generation INTEGER,owner TEXT,PRIMARY KEY(bucket,name,generation));").unwrap();
    connection.execute_batch("CREATE TABLE acl_object_entries(bucket TEXT,name TEXT,generation INTEGER,entries TEXT,PRIMARY KEY(bucket,name,generation));").unwrap();
    for (name, owner) in [
        ("created", local_creator("user-old@example.test")),
        (
            "legacy",
            ObjectOwner::ProjectOwners {
                project: "project".into(),
            },
        ),
    ] {
        connection
            .execute(
                "INSERT INTO objects VALUES('b',?1,1,?2)",
                params![name, serde_json::to_string(&owner).unwrap()],
            )
            .unwrap();
    }
    reconcile(&mut connection, &permissive(Some("new@example.test"))).unwrap();
    assert_eq!(
        selected(&connection, "b", "created", 1).unwrap(),
        Some(local_creator("user-new@example.test"))
    );
    assert!(matches!(
        selected(&connection, "b", "legacy", 1).unwrap(),
        Some(ObjectOwner::ProjectOwners { .. })
    ));
}

#[test]
fn the_local_creator_owner_is_never_a_caller_even_with_a_matching_identity() {
    let identity = crate::auth::Identity::from_upload_snapshot(
        &json!({"id": LOCAL_CREATOR_ID, "projects": {}}),
    )
    .unwrap();
    assert!(!local_creator("user-x@example.test").is_principal(Some(&identity)));
    let ordinary =
        crate::auth::Identity::from_upload_snapshot(&json!({"id": "local:owner", "projects": {}}))
            .unwrap();
    let owner = ObjectOwner::Principal {
        id: "local:owner".into(),
        entity: None,
    };
    assert!(owner.is_principal(Some(&ordinary)));
}
