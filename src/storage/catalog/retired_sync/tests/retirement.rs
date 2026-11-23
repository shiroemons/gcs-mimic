//! Schema 58 on catalogs that released synchronization left behind: the files their rows name
//! are listed in a durable manifest, every synchronization table goes, and no file moves.

use super::*;
use crate::{
    object::ObjectRecord,
    storage::{
        catalog::fault::{self, Crash, Fault, Step},
        test_fixtures::read_all,
    },
};
use rusqlite::types::Value;
use serde_json::{Value as Json, json};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

const BUCKET: &str = "bucket";
const MANIFEST: &str = "sync-retired.json";
const STAGED: &str = "sync-retired.json.tmp";
/// The bytes each object holds, chosen per role: a pending write-back publishes `published` to
/// the folder, `deleted-in-folder` waits in a deletion set, and `kept` is the object a rejected
/// deletion writes back.
const OBJECTS: [(&str, &[u8]); 3] = [
    ("published", b"api bytes"),
    ("deleted-in-folder", b"still in the bucket"),
    ("kept", b"rejected deletion"),
];

fn id(number: u32) -> String {
    format!("{number:032x}")
}

/// The manifest at `path`, parsed.
#[track_caller]
fn read_manifest(path: &Path) -> Json {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// The manifest that lists `copies` and `entries`.
fn manifest(copies: &[Json], entries: &[Json]) -> Json {
    json!({
        "description": preserved::DESCRIPTION,
        "conflict_copies": copies,
        "workspace_entries": entries,
    })
}

/// How the manifest lists the workspace entry `number` in `folder`: one a write-back of `object`
/// left, or one a recovery left after its write-back was gone (`None`).
fn workspace_entry(folder: &Path, number: u32, object: Option<&str>) -> Json {
    let mut entry = json!({
        "root": folder.to_str().unwrap(),
        "path": folder.join(".gcs-mimic-sync-work").join(id(number)).to_str().unwrap(),
    });
    if let Some(object) = object {
        entry["bucket"] = json!(BUCKET);
        entry["object"] = json!(object);
        entry["target"] = json!(format!("{BUCKET}/{object}"));
    }
    entry
}

/// A data directory at schema 57 with a synchronized folder beside it, both as a 0.27.0 server
/// could have left them.
struct Released {
    dir: TestDir,
    objects: Vec<(ObjectRecord, Vec<u8>)>,
    /// The `objects` table as SQLite holds it.
    rows: Vec<Vec<Value>>,
}

impl Released {
    fn data(&self) -> PathBuf {
        self.dir.0.join("data")
    }

    fn folder(&self) -> PathBuf {
        self.dir.0.join("folder")
    }

    /// The objects with their bytes, read through `catalog`.
    async fn assert_objects(&self, catalog: &Catalog) {
        for (record, bytes) in &self.objects {
            let (read, mut reader) = catalog
                .open_object(live(BUCKET, &record.name), Default::default(), PERMISSIVE)
                .await
                .unwrap();
            assert_eq!(&read, record);
            assert_eq!(&read_all(&mut reader).await, bytes);
        }
    }

    /// The manifest the rows of [`released`] give.
    fn manifest(&self) -> Json {
        let folder = self.folder();
        manifest(
            &[
                json!({
                    "path": format!("sync-conflicts/{}", id(0xc2)),
                    "bucket": BUCKET,
                    "object": "kept",
                    "size": 4,
                    "state": "capturing",
                }),
                json!({
                    "path": format!("sync-conflicts/{}", id(0xc1)),
                    "bucket": BUCKET,
                    "object": "published",
                    "generation": self.objects[0].0.generation,
                    "size": 11,
                    "md5": "AAECAwQFBgcICQoLDA0ODw==",
                    "crc32c": "AQIDBA==",
                    "state": "registered",
                }),
            ],
            &[
                workspace_entry(&folder, 0xa2, Some("published")),
                workspace_entry(&folder, 0xa3, Some("removed")),
                workspace_entry(&folder, 0xa5, Some("conflicted")),
                workspace_entry(&folder, 0xa6, None),
            ],
        )
    }
}

/// Every file under `root` with its bytes, whether it is read-only, and its modification time.
fn files(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, bool, SystemTime)> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            found.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (
                    fs::read(&path).unwrap(),
                    metadata.permissions().readonly(),
                    metadata.modified().unwrap(),
                ),
            );
        }
    }
    found
}

fn objects_table(connection: &Connection) -> Vec<Vec<Value>> {
    let mut statement = connection
        .prepare("SELECT * FROM objects ORDER BY bucket, name, generation")
        .unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn names(connection: &Connection, query: &str) -> Vec<String> {
    connection
        .prepare(query)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// What 0.27.0 kept about a copy once it was written: its file identity and digests.
fn retained(size: u64) -> Vec<u8> {
    let md5: Vec<u8> = (0..16).collect();
    format!(
        "{{\"device\":1,\"inode\":2,\"size\":{size},\"modified_ns\":3,\"changed_ns\":4,\"hashes\":{{\"md5\":{md5:?},\"crc32c\":{}}}}}",
        0x0102_0304_u32
    )
    .into_bytes()
}

/// Records a write-back of `name` into the folder in `phase` (0 prepared to 4 complete), a
/// publish (kind 0) or a removal (kind 1).
fn write_back(connection: &Connection, number: u32, name: &str, kind: i64, phase: i64) {
    connection
        .execute(
            "INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) SELECT ?1,?2,'owner',bucket,incarnation,?3,bucket||'/'||?3,1,?4,?5,1,0,0,zeroblob(8),4,zeroblob(16),0 FROM bucket_incarnations WHERE bucket=?6",
            params![id(number), ROOT, name, kind, phase, BUCKET],
        )
        .unwrap();
}

/// Records a recovery workspace in `phase` (0 protected, 1 discard accepted, 2 released).
fn recovery(connection: &Connection, number: u32, phase: i64) {
    connection
        .execute(
            "INSERT INTO sync_recovery_workspaces VALUES(?1,?2,?3,?3,X'7B7D',?4)",
            params![id(number), ROOT, id(0xe2), phase],
        )
        .unwrap();
}

/// Records a conflict copy in `state` (0 capturing, 1 registered, 2 discarded).
fn conflict(
    connection: &Connection,
    number: u32,
    name: &str,
    copy: (Option<i64>, i64, u64),
    retained: Option<Vec<u8>>,
) {
    let (generation, state, size) = copy;
    connection
        .execute(
            "INSERT INTO sync_conflicts(id,root_id,revision,owner,bucket,incarnation,name,generation,state,size,source_op,source,retained) SELECT ?1,?2,?3,'owner',bucket,incarnation,?4,?5,?6,?7,NULL,X'7B7D',?8 FROM bucket_incarnations WHERE bucket=?9",
            params![id(number), ROOT, id(0xe3), name, generation, state, size, retained, BUCKET],
        )
        .unwrap();
}

/// A new catalog in `data` holding [`BUCKET`], open.
async fn bucket_catalog(data: &Path) -> Catalog {
    let catalog = Catalog::open(data).await.unwrap();
    catalog
        .insert_bucket(bucket(BUCKET, "owner"))
        .await
        .unwrap();
    catalog
}

/// The closed catalog in `data` back at schema 57, as 0.27.0 left it, with foreign keys
/// enforced.
fn at_schema_57(data: &Path) -> Connection {
    let connection = Connection::open(data.join("catalog.sqlite3")).unwrap();
    test_support::downgrade_retired_sync_schema(&connection);
    connection
        .pragma_update(None, "user_version", RETIREMENT_SCHEMA_VERSION - 1)
        .unwrap();
    validate_schema(&connection, RETIREMENT_SCHEMA_VERSION - 1).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    connection
}

/// Records `folder` as synchronized with [`BUCKET`], with the per-folder rows that write-backs
/// and conflict copies need.
fn record_folder(connection: &Connection, folder: &Path) {
    let incarnation: String = connection
        .query_row(
            "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
            [BUCKET],
            |row| row.get(0),
        )
        .unwrap();
    record_root(connection, BUCKET, &[&incarnation]);
    connection
        .execute(
            "UPDATE sync_roots SET location = ?1",
            [folder.to_str().unwrap().as_bytes()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_writeback_roots VALUES(?1,1024,16,zeroblob(16),zeroblob(16),0,0,0,0,zeroblob(8),zeroblob(8),NULL,NULL)",
            [ROOT],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_conflict_roots VALUES(?1,?2)",
            params![ROOT, id(0xe1)],
        )
        .unwrap();
}

/// The rows of every kind 0.27.0 wrote for one root, those that name files and those that do
/// not, with the files they name.
fn record_synchronization(connection: &Connection, folder: &Path, data: &Path, generation: i64) {
    record_folder(connection, folder);
    // The write-back of a rejected deletion, prepared: its entry holds only its own copy.
    write_back(connection, 0xa1, "kept", 0, 0);
    write_back(connection, 0xa2, "published", 0, 1);
    write_back(connection, 0xa3, "removed", 1, 3);
    write_back(connection, 0xa4, "done", 0, 4);
    write_back(connection, 0xa5, "conflicted", 0, 2);
    recovery(connection, 0xa5, 0);
    recovery(connection, 0xa6, 1);
    recovery(connection, 0xa7, 2);
    // Listed in capture order, which is not the order of their IDs.
    conflict(connection, 0xc2, "kept", (None, 0, 4), None);
    conflict(
        connection,
        0xc1,
        "published",
        (Some(generation), 1, 11),
        Some(retained(11)),
    );
    conflict(
        connection,
        0xc3,
        "published",
        (Some(generation), 2, 3),
        None,
    );
    connection
        .execute(
            "INSERT INTO sync_deletion_sets VALUES(?1,?2,?3,1,1,1,3,1,1,0,1,1,0)",
            params![ROOT, id(0xd1), id(0xd2)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_deletion_members SELECT ?1,1,bucket,'deleted-in-folder','owner',incarnation,1,'deleted-in-folder',0 FROM bucket_incarnations WHERE bucket=?2",
            params![ROOT, BUCKET],
        )
        .unwrap();
    let copies = data.join("sync-conflicts");
    fs::create_dir(&copies).unwrap();
    // A discarded copy's file was gone before its row said so; this one stands for a file the
    // rows no longer name, which stays all the same.
    for (number, bytes) in [
        (0xc1, b"edited copy".as_slice()),
        (0xc2, b"part"),
        (0xc3, b"old"),
    ] {
        let path = copies.join(id(number));
        fs::write(&path, bytes).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
    }
    let workspace = folder.join(".gcs-mimic-sync-work");
    fs::create_dir_all(&workspace).unwrap();
    for number in [0xa1, 0xa2, 0xa3, 0xa5, 0xa6] {
        fs::write(workspace.join(id(number)), id(number)).unwrap();
    }
    fs::write(folder.join("published"), b"edited in the folder").unwrap();
}

/// A schema-57 data directory whose synchronization rows name files in it and in the folder.
async fn released() -> Released {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    let folder = dir.0.join("folder");
    fs::create_dir(&data).unwrap();
    let catalog = bucket_catalog(&data).await;
    let mut objects = Vec::new();
    for (name, bytes) in OBJECTS {
        let record = catalog
            .put_object(
                permissive_upload(BUCKET, name, 100),
                body(vec![Bytes::from_static(bytes)]),
            )
            .await
            .unwrap();
        objects.push((record, bytes.to_vec()));
    }
    catalog.close().await.unwrap();
    let connection = at_schema_57(&data);
    record_synchronization(&connection, &folder, &data, objects[0].0.generation);
    let rows = objects_table(&connection);
    Released { dir, objects, rows }
}

/// Schema 58 with no synchronization object left.
#[track_caller]
fn assert_retired(connection: &Connection) {
    assert_eq!(user_version(connection), RETIREMENT_SCHEMA_VERSION);
    validate_schema(connection, RETIREMENT_SCHEMA_VERSION).unwrap();
    assert_eq!(
        names(
            connection,
            "SELECT name FROM sqlite_schema WHERE name GLOB 'sync_*' OR tbl_name GLOB 'sync_*' UNION ALL SELECT name FROM sqlite_sequence WHERE name GLOB 'sync_*'"
        ),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn a_released_catalog_lists_the_files_its_rows_name_and_drops_every_sync_object() {
    let released = released().await;
    let data = released.data();
    let copies = files(&data.join("sync-conflicts"));
    let folder = files(&released.folder());
    let connection = Connection::open(data.join("catalog.sqlite3")).unwrap();
    let mut retired = names(
        &connection,
        "SELECT type || ' ' || name FROM sqlite_schema WHERE tbl_name GLOB 'sync_*' AND name NOT GLOB 'sqlite_autoindex_*'",
    );
    retired.sort();
    let tables: Vec<_> = TABLES
        .iter()
        .map(|table| format!("table {table}"))
        .collect();
    assert_eq!(
        retired
            .iter()
            .filter(|name| name.starts_with("table "))
            .collect::<Vec<_>>(),
        tables.iter().collect::<Vec<_>>()
    );
    assert_eq!(
        retired
            .iter()
            .filter(|name| name.starts_with("index "))
            .count(),
        9
    );
    assert_eq!(
        retired
            .iter()
            .filter(|name| name.starts_with("trigger "))
            .count(),
        32
    );
    drop(connection);

    let catalog = Catalog::open(&data).await.unwrap();
    released.assert_objects(&catalog).await;
    catalog.close().await.unwrap();
    assert_eq!(read_manifest(&data.join(MANIFEST)), released.manifest());
    assert!(!data.join(STAGED).exists());
    assert_eq!(files(&data.join("sync-conflicts")), copies);
    assert_eq!(files(&released.folder()), folder);
    let connection = Connection::open(data.join("catalog.sqlite3")).unwrap();
    assert_retired(&connection);
    assert_eq!(objects_table(&connection), released.rows);
    // The identity schema 17 created is not synchronization state.
    validate_identity(&connection).unwrap();
    drop(connection);

    // A later start finds schema 58 and leaves the manifest as it was.
    let written = fs::metadata(data.join(MANIFEST))
        .unwrap()
        .modified()
        .unwrap();
    Catalog::open(&data).await.unwrap().close().await.unwrap();
    assert_eq!(
        fs::metadata(data.join(MANIFEST))
            .unwrap()
            .modified()
            .unwrap(),
        written
    );
}

#[tokio::test]
async fn rows_that_name_no_file_retire_without_a_manifest() {
    let dir = TestDir::new();
    bucket_catalog(&dir.0).await.close().await.unwrap();
    assert!(!dir.0.join(MANIFEST).exists());
    let connection = at_schema_57(&dir.0);
    record_folder(&connection, &dir.0.join("folder"));
    // A prepared publish holds only its own copy of the object, a complete write-back and a
    // released workspace hold nothing, and a discarded copy's file was removed first.
    write_back(&connection, 0xa1, "object", 0, 0);
    write_back(&connection, 0xa4, "object", 0, 4);
    recovery(&connection, 0xa7, 2);
    conflict(&connection, 0xc3, "object", (Some(1), 2, 3), None);
    drop(connection);

    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
    assert!(!dir.0.join(MANIFEST).exists());
    assert!(!dir.0.join(STAGED).exists());
    assert_retired(&Connection::open(dir.0.join("catalog.sqlite3")).unwrap());
}

/// Each kind of row that names a file writes the manifest on its own, listing just that file.
#[tokio::test]
async fn each_kind_of_row_that_names_a_file_alone_writes_the_manifest() {
    /// What a case records, and the manifest it gives for the folder.
    type Case = (&'static str, fn(&Connection), fn(&Path) -> Json);
    let cases: [Case; 3] = [
        (
            "recovery workspace",
            |connection| recovery(connection, 0xa6, 0),
            |folder| manifest(&[], &[workspace_entry(folder, 0xa6, None)]),
        ),
        (
            "write-back",
            |connection| write_back(connection, 0xa3, "removed", 1, 1),
            |folder| manifest(&[], &[workspace_entry(folder, 0xa3, Some("removed"))]),
        ),
        (
            "conflict copy",
            |connection| conflict(connection, 0xc1, "edited", (Some(1), 1, 3), None),
            |_| {
                manifest(
                    &[json!({
                        "path": format!("sync-conflicts/{}", id(0xc1)),
                        "bucket": BUCKET,
                        "object": "edited",
                        "generation": 1,
                        "size": 3,
                        "state": "registered",
                    })],
                    &[],
                )
            },
        ),
    ];
    for (kind, record, expected) in cases {
        let dir = TestDir::new();
        let folder = dir.0.join("folder");
        bucket_catalog(&dir.0).await.close().await.unwrap();
        let connection = at_schema_57(&dir.0);
        record_folder(&connection, &folder);
        record(&connection);
        drop(connection);

        Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
        assert_eq!(
            read_manifest(&dir.0.join(MANIFEST)),
            expected(&folder),
            "{kind}"
        );
        assert_retired(&Connection::open(dir.0.join("catalog.sqlite3")).unwrap());
    }
}

/// A copy whose recorded observation no release could have written is still listed, marked as
/// having unreadable digests, and the upgrade goes on.
#[tokio::test]
async fn a_copy_whose_digests_cannot_be_read_is_listed_and_retires() {
    let dir = TestDir::new();
    bucket_catalog(&dir.0).await.close().await.unwrap();
    let connection = at_schema_57(&dir.0);
    record_folder(&connection, &dir.0.join("folder"));
    conflict(
        &connection,
        0xc1,
        "truncated",
        (None, 1, 3),
        Some(b"{".to_vec()),
    );
    conflict(
        &connection,
        0xc2,
        "short",
        (None, 1, 3),
        Some(retained(3)[..40].to_vec()),
    );
    conflict(&connection, 0xc3, "integer", (None, 1, 3), None);
    connection
        .execute(
            "UPDATE sync_conflicts SET retained = 7 WHERE id = ?1",
            [id(0xc3)],
        )
        .unwrap();
    drop(connection);

    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
    let copy = |number: u32, object: &str| {
        json!({
            "path": format!("sync-conflicts/{}", id(number)),
            "bucket": BUCKET,
            "object": object,
            "size": 3,
            "digests": "unreadable",
            "state": "registered",
        })
    };
    assert_eq!(
        read_manifest(&dir.0.join(MANIFEST)),
        manifest(
            &[
                copy(0xc1, "truncated"),
                copy(0xc2, "short"),
                copy(0xc3, "integer")
            ],
            &[]
        )
    );
    assert_retired(&Connection::open(dir.0.join("catalog.sqlite3")).unwrap());
}

/// The step drops the synchronization tables in one transaction: when the last drop fails, the
/// tables dropped before it come back with their rows.
#[test]
fn the_retirement_step_commits_whole() {
    let mut connection = catalog_at(RETIREMENT_SCHEMA_VERSION - 1);
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    record_root(&connection, BUCKET, &[&"0".repeat(32)]);
    // A reference the step does not know about keeps `sync_roots`, which goes last, from
    // losing its rows.
    connection
        .execute_batch("CREATE TABLE blocker(root BLOB REFERENCES sync_roots(root_id))")
        .unwrap();
    connection
        .execute("INSERT INTO blocker VALUES(?1)", [ROOT])
        .unwrap();
    assert!(retire(&mut connection, None).is_err());
    assert_eq!(user_version(&connection), RETIREMENT_SCHEMA_VERSION - 1);
    assert_eq!(count(&connection, "SELECT count(*) FROM sync_findings"), 1);
    assert_eq!(count(&connection, "SELECT count(*) FROM sync_objects"), 1);
    connection.execute_batch("DROP TABLE blocker").unwrap();
    retire(&mut connection, None).unwrap();
    assert_retired(&connection);
    assert!(
        validate_schema_version(RETIREMENT_SCHEMA_VERSION, RETIREMENT_SCHEMA_VERSION - 1).is_err()
    );
}

/// A RAM catalog starts empty; rows that name files without a data directory to list them in
/// stop the upgrade instead of dropping the only record of those files.
#[test]
fn rows_that_name_files_need_a_data_directory() {
    let mut connection = catalog_at(RETIREMENT_SCHEMA_VERSION - 1);
    record_root(&connection, BUCKET, &[&"0".repeat(32)]);
    connection
        .execute(
            "INSERT INTO sync_conflict_roots VALUES(?1,?2)",
            params![ROOT, id(0xe1)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_conflicts(id,root_id,revision,owner,bucket,incarnation,name,generation,state,size,source_op,source,retained) VALUES(?1,?2,?3,'owner',?4,?5,'object',NULL,1,3,NULL,X'7B7D',NULL)",
            params![id(0xc1), ROOT, id(0xe3), BUCKET, "0".repeat(32)],
        )
        .unwrap();
    assert!(retire(&mut connection, None).is_err());
    assert_eq!(user_version(&connection), RETIREMENT_SCHEMA_VERSION - 1);
    assert_eq!(count(&connection, "SELECT count(*) FROM sync_conflicts"), 1);
}

/// A failure while the manifest is written, or of the transaction after it, leaves schema 57
/// and every file; the next open writes the same manifest and retires the rows.
#[tokio::test]
async fn a_failed_manifest_step_or_commit_leaves_schema_57_and_the_next_open_finishes() {
    let full = || Fault::Io(io::ErrorKind::StorageFull);
    let faults = [
        (Step::RetiredManifestWrite, 0, full()),
        (Step::RetiredManifestWrite, 4, full()),
        (Step::RetiredManifestFlush, 0, full()),
        (
            Step::RetiredManifestSync,
            0,
            Fault::Io(io::ErrorKind::Other),
        ),
        (
            Step::RetiredManifestRename,
            0,
            Fault::Io(io::ErrorKind::PermissionDenied),
        ),
        #[cfg(unix)]
        (
            Step::RetiredManifestDirSync,
            0,
            Fault::Io(io::ErrorKind::Other),
        ),
        (Step::RetiredSyncCommit, 0, Fault::RollBack),
        (Step::RetiredSyncCommit, 0, full()),
    ];
    for (step, skip, fault) in faults {
        let released = released().await;
        let data = released.data();
        let copies = files(&data.join("sync-conflicts"));
        let folder = files(&released.folder());
        fault::arm(&data, step, skip, fault);
        assert!(Catalog::open(&data).await.is_err(), "{step:?}");
        assert_eq!(fault::pending(&data), 0, "{step:?}");
        let connection = Connection::open(data.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            user_version(&connection),
            RETIREMENT_SCHEMA_VERSION - 1,
            "{step:?}"
        );
        assert_eq!(count(&connection, "SELECT count(*) FROM sync_conflicts"), 3);
        drop(connection);
        assert_eq!(files(&data.join("sync-conflicts")), copies, "{step:?}");
        let published = fs::read(data.join(MANIFEST)).ok();

        let catalog = Catalog::open(&data).await.unwrap();
        released.assert_objects(&catalog).await;
        catalog.close().await.unwrap();
        assert_eq!(
            read_manifest(&data.join(MANIFEST)),
            released.manifest(),
            "{step:?}"
        );
        // Written again from the same rows, a manifest the failed open published keeps its
        // bytes.
        if let Some(published) = published {
            assert_eq!(
                fs::read(data.join(MANIFEST)).unwrap(),
                published,
                "{step:?}"
            );
        }
        assert!(!data.join(STAGED).exists(), "{step:?}");
        assert_eq!(files(&data.join("sync-conflicts")), copies, "{step:?}");
        assert_eq!(files(&released.folder()), folder, "{step:?}");
        let connection = Connection::open(data.join("catalog.sqlite3")).unwrap();
        assert_retired(&connection);
        assert_eq!(objects_table(&connection), released.rows);
    }
}

/// A process abort or a power loss at each manifest step, or between the durable manifest and
/// the commit, leaves a data directory whose next open writes the same manifest and retires
/// the rows, with no file lost.
#[tokio::test]
async fn a_crash_at_each_manifest_step_or_before_the_commit_restarts_with_the_same_manifest() {
    let steps = [
        (Step::RetiredManifestWrite, 0),
        (Step::RetiredManifestWrite, 4),
        (Step::RetiredManifestFlush, 0),
        (Step::RetiredManifestSync, 0),
        (Step::RetiredManifestRename, 0),
        #[cfg(unix)]
        (Step::RetiredManifestDirSync, 0),
        (Step::RetiredSyncCommit, 0),
    ];
    for &crash in Crash::ALL {
        for (step, skip) in steps {
            let released = released().await;
            let data = released.data();
            let copies = files(&data.join("sync-conflicts"));
            let snapshot = released.dir.0.join("crash");
            fault::arm(&data, step, skip, crash.fault(snapshot.clone()));
            Catalog::open(&data).await.unwrap().close().await.unwrap();
            assert_eq!(fault::pending(&data), 0, "{crash:?} at {step:?}");
            assert_eq!(
                files(&snapshot.join("sync-conflicts")),
                copies,
                "{crash:?} at {step:?}"
            );
            // The rows go only once the manifest that lists them survives a crash.
            if step == Step::RetiredSyncCommit {
                assert_eq!(
                    read_manifest(&snapshot.join(MANIFEST)),
                    released.manifest(),
                    "{crash:?}"
                );
            }

            let catalog = Catalog::open(&snapshot).await.unwrap();
            released.assert_objects(&catalog).await;
            catalog.close().await.unwrap();
            assert_eq!(
                read_manifest(&snapshot.join(MANIFEST)),
                released.manifest(),
                "{crash:?} at {step:?}"
            );
            assert!(!snapshot.join(STAGED).exists(), "{crash:?} at {step:?}");
            assert_eq!(
                files(&snapshot.join("sync-conflicts")),
                copies,
                "{crash:?} at {step:?}"
            );
            let connection = Connection::open(snapshot.join("catalog.sqlite3")).unwrap();
            assert_retired(&connection);
            assert_eq!(objects_table(&connection), released.rows);
        }
    }
}
