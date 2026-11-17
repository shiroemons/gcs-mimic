use super::*;
use crate::storage::catalog::Catalog;
use crate::storage::test_fixtures::{self, PERMISSIVE, bucket, live, permissive_upload};
use crate::test_support::TestDir;

const LOCAL: &str = "902648055030";

fn entries(json: &str) -> Vec<AclEntry> {
    serde_json::from_str(json).unwrap()
}

#[test]
fn only_the_owning_projects_zero_numbered_teams_are_renamed() {
    let mut acl = entries(
        r#"[{"entity":"project-owners-0","role":"OWNER"},{"entity":"project-editors-0","role":"OWNER"},{"entity":"project-viewers-0","role":"READER"},{"entity":"project-owners-05","role":"READER"},{"entity":"project-owners-123","role":"READER"},{"entity":"user-0@example.test","role":"READER"}]"#,
    );
    assert!(renumber(&mut acl, "local"));
    let names: Vec<_> = acl.iter().map(|entry| entry.entity.as_str()).collect();
    assert_eq!(
        names,
        [
            &format!("project-owners-{LOCAL}"),
            &format!("project-editors-{LOCAL}"),
            &format!("project-viewers-{LOCAL}"),
            "project-owners-05",
            "project-owners-123",
            "user-0@example.test",
        ]
    );
    // Renaming twice changes nothing more, so a repeated upgrade is harmless.
    assert!(!renumber(&mut acl, "local"));
    // A numeric project rendered its own number, so its `-0` team names project 0.
    let mut numeric = entries(r#"[{"entity":"project-owners-0","role":"OWNER"}]"#);
    assert!(!renumber(&mut numeric, "123"));
    assert_eq!(numeric[0].entity.as_str(), "project-owners-0");
}

#[test]
fn stored_text_changes_only_when_an_entity_is_renamed() {
    let scoped = r#"[{"scope":"bucket","entries":[{"entity":"allUsers","role":"READER"}]},{"scope":"defaultObject","entries":[{"entity":"project-viewers-0","role":"READER"}]}]"#;
    assert_eq!(
        Shape::Scoped.renumbered(scoped, "local").unwrap().unwrap(),
        format!(
            r#"[{{"scope":"bucket","entries":[{{"entity":"allUsers","role":"READER"}}]}},{{"scope":"defaultObject","entries":[{{"entity":"project-viewers-{LOCAL}","role":"READER"}}]}}]"#
        )
    );
    for (shape, json) in [
        (Shape::Entries, r#"[{"entity":"allUsers","role":"READER"}]"#),
        (Shape::Entries, "[]"),
        // A row that does not decode is left for its reader to report.
        (
            Shape::Entries,
            r#"[{"entity":"project-owners-0","role":"NOPE"}]"#,
        ),
        (Shape::Scoped, r#"[{"scope":"bucket"}]"#),
    ] {
        assert_eq!(shape.renumbered(json, "local").unwrap(), None, "{json}");
    }
}

/// A catalog written by an earlier release: zero-numbered teams in a live bucket's two ACLs,
/// an object ACL, a soft-deleted bucket's ACLs and its retained object's ACL, plus a numeric
/// project whose `-0` team is a grant to project 0.
async fn version_54_catalog(dir: &TestDir) {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for (name, project) in [("named", "local"), ("numbered", "123"), ("gone", "local")] {
        catalog.insert_bucket(bucket(name, project)).await.unwrap();
    }
    // A retention period keeps the deleted object and then the deleted bucket.
    catalog
        .patch_bucket(
            "gone",
            crate::bucket::BucketMetadataUpdate {
                soft_delete_retention: Some((7 * 86_400_i64).try_into().unwrap()),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    for (bucket, name) in [("named", "object"), ("gone", "kept")] {
        catalog
            .put_object(
                permissive_upload(bucket, name, 1),
                test_fixtures::body(vec![bytes::Bytes::from_static(b"x")]),
            )
            .await
            .unwrap();
    }
    catalog
        .delete_object(live("gone", "kept"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog
        .delete_bucket("gone", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let owners =
        r#"[{"entity":"project-owners-0","role":"OWNER"},{"entity":"allUsers","role":"READER"}]"#;
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_running_hash_schema(&connection);
    connection
        .execute_batch(&format!(
            "INSERT INTO acl_bucket_entries VALUES ('named', 'bucket', '{owners}');
             INSERT INTO acl_bucket_entries VALUES ('named', 'defaultObject', '[{{\"entity\":\"project-viewers-0\",\"role\":\"READER\"}}]');
             INSERT INTO acl_bucket_entries VALUES ('numbered', 'bucket', '{owners}');
             UPDATE soft_deleted_buckets SET acl = '[{{\"scope\":\"bucket\",\"entries\":{owners}}}]' WHERE name = 'gone';
             INSERT OR REPLACE INTO acl_object_entries SELECT bucket, name, generation, '{owners}' FROM objects;
             PRAGMA user_version = 54;"
        ))
        .unwrap();
}

fn stored(connection: &rusqlite::Connection) -> Vec<String> {
    connection
        .prepare(
            "SELECT entries FROM acl_bucket_entries UNION ALL SELECT entries FROM acl_object_entries UNION ALL SELECT acl FROM soft_deleted_buckets",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn opening_an_older_catalog_renames_zero_numbered_teams_to_the_owning_project() {
    let dir = TestDir::new();
    version_54_catalog(&dir).await;
    let catalog = Catalog::open(&dir.0).await.unwrap();
    // The bucket's own team is the entity its project-private ACL spells today.
    let read = catalog
        .read_acl(
            crate::acl::AclTarget::Bucket("named".into()),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(
        read.entries[0].entity,
        Entity::project_team(crate::acl::ProjectTeam::Owners, "local")
    );
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, crate::storage::catalog::CATALOG_SCHEMA_VERSION);
    let renamed = format!(
        r#"[{{"entity":"project-owners-{LOCAL}","role":"OWNER"}},{{"entity":"allUsers","role":"READER"}}]"#
    );
    for (sql, expected) in [
        (
            "SELECT entries FROM acl_bucket_entries WHERE bucket = 'named' AND scope = 'bucket'",
            renamed.clone(),
        ),
        (
            "SELECT entries FROM acl_bucket_entries WHERE bucket = 'named' AND scope = 'defaultObject'",
            format!(r#"[{{"entity":"project-viewers-{LOCAL}","role":"READER"}}]"#),
        ),
        (
            "SELECT entries FROM acl_object_entries WHERE bucket = 'named'",
            renamed.clone(),
        ),
        (
            "SELECT entries FROM acl_object_entries WHERE bucket GLOB 'gone#*'",
            renamed.clone(),
        ),
        (
            "SELECT acl FROM soft_deleted_buckets WHERE name = 'gone'",
            format!(r#"[{{"scope":"bucket","entries":{renamed}}}]"#),
        ),
        (
            "SELECT entries FROM acl_bucket_entries WHERE bucket = 'numbered'",
            r#"[{"entity":"project-owners-0","role":"OWNER"},{"entity":"allUsers","role":"READER"}]"#
                .to_owned(),
        ),
    ] {
        let stored: String = connection.query_row(sql, [], |row| row.get(0)).unwrap();
        assert_eq!(stored, expected, "{sql}");
    }
}

#[tokio::test]
async fn a_failed_rename_leaves_every_row_and_the_version_unchanged() {
    let dir = TestDir::new();
    version_54_catalog(&dir).await;
    let mut connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let before = stored(&connection);
    // The last column fails after the bucket and object ACLs were already rewritten.
    connection
        .execute_batch("CREATE TEMP TRIGGER fail_rename BEFORE UPDATE ON soft_deleted_buckets BEGIN SELECT RAISE(ABORT, 'injected rename failure'); END;")
        .unwrap();
    let error = migrate(&mut connection).unwrap_err();
    assert!(
        error.to_string().contains("injected rename failure"),
        "{error}"
    );
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 54);
    assert_eq!(stored(&connection), before);
    connection
        .execute_batch("DROP TRIGGER fail_rename;")
        .unwrap();
    migrate(&mut connection).unwrap();
    let zero_numbered = stored(&connection)
        .iter()
        .filter(|json| json.contains(r#""project-owners-0""#))
        .count();
    // Only the numeric project's grant to project 0 keeps its number.
    assert_eq!(zero_numbered, 1);
}
