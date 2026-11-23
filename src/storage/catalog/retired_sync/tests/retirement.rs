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
/// How each attempt's staged manifest is named, before its random part.
const STAGED: &str = "sync-retired.json.";

/// The names in `dir` that look like a staged manifest.
fn staged(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(STAGED) && name.ends_with(".tmp"))
        .collect()
}

/// A staged manifest name an earlier attempt could have used.
fn earlier_staged(number: u32) -> String {
    format!("{STAGED}{}.tmp", id(number))
}
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

/// The manifest that lists `folder`, `copies` and `entries`.
fn manifest(folder: &Path, copies: &[Json], entries: &[Json]) -> Json {
    json!({
        "description": preserved::DESCRIPTION,
        "folders": [{"folder": folder.to_str().unwrap()}],
        "conflict_copies": copies,
        "workspace_entries": entries,
    })
}

/// How the manifest lists the copy `number` of an edit to `object` in `folder`, whose bucket
/// was `incarnation`: before its generation and digests, which not every copy has.
fn conflict_copy(
    folder: &Path,
    number: u32,
    incarnation: &str,
    object: &str,
    (size, state): (u64, &str),
) -> Json {
    json!({
        "path": format!("sync-conflicts/{}", id(number)),
        "folder": folder.to_str().unwrap(),
        "target": format!("{BUCKET}/{object}"),
        "bucket": BUCKET,
        "bucket_incarnation": incarnation,
        "object": object,
        "size": size,
        "state": state,
    })
}

/// How the manifest lists the workspace entry `number` in `folder`: one a write-back of an
/// object, given with its bucket's incarnation, left, or one a recovery left after its
/// write-back was gone (`None`).
fn workspace_entry(folder: &Path, number: u32, write_back: Option<(&str, &str)>) -> Json {
    let mut entry = json!({
        "folder": folder.to_str().unwrap(),
        "path": folder.join(".gcs-mimic-sync-work").join(id(number)).to_str().unwrap(),
    });
    if let Some((incarnation, object)) = write_back {
        entry["target"] = json!(format!("{BUCKET}/{object}"));
        entry["bucket"] = json!(BUCKET);
        entry["bucket_incarnation"] = json!(incarnation);
        entry["object"] = json!(object);
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
    /// The incarnation of [`BUCKET`] the rows name.
    incarnation: String,
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
        let incarnation = self.incarnation.as_str();
        let mut published =
            conflict_copy(&folder, 0xc1, incarnation, "published", (11, "registered"));
        published["generation"] = json!(self.objects[0].0.generation);
        published["md5"] = json!("AAECAwQFBgcICQoLDA0ODw==");
        published["crc32c"] = json!("AQIDBA==");
        manifest(
            &folder,
            &[
                conflict_copy(&folder, 0xc2, incarnation, "kept", (4, "capturing")),
                published,
            ],
            &[
                workspace_entry(&folder, 0xa2, Some((incarnation, "published"))),
                workspace_entry(&folder, 0xa3, Some((incarnation, "removed"))),
                workspace_entry(&folder, 0xa5, Some((incarnation, "conflicted"))),
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
/// and conflict copies need, and returns the bucket's incarnation.
fn record_folder(connection: &Connection, folder: &Path) -> String {
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
    incarnation
}

/// The rows of every kind 0.27.0 wrote for one root, those that name files and those that do
/// not, with the files they name. Returns the bucket's incarnation.
fn record_synchronization(
    connection: &Connection,
    folder: &Path,
    data: &Path,
    generation: i64,
) -> String {
    let incarnation = record_folder(connection, folder);
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
    fs::create_dir(folder.join(BUCKET)).unwrap();
    fs::write(
        folder.join(BUCKET).join("published"),
        b"edited in the folder",
    )
    .unwrap();
    incarnation
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
    let incarnation = record_synchronization(&connection, &folder, &data, objects[0].0.generation);
    let rows = objects_table(&connection);
    Released {
        dir,
        objects,
        rows,
        incarnation,
    }
}

/// Schema 58 with no synchronization or change journal object left.
#[track_caller]
fn assert_retired(connection: &Connection) {
    assert_eq!(user_version(connection), RETIREMENT_SCHEMA_VERSION);
    validate_schema(connection, RETIREMENT_SCHEMA_VERSION).unwrap();
    assert_eq!(
        names(
            connection,
            "SELECT name FROM sqlite_schema WHERE name GLOB 'sync_*' OR tbl_name GLOB 'sync_*' OR tbl_name GLOB 'change_journal*' UNION ALL SELECT name FROM sqlite_sequence WHERE name GLOB 'sync_*'"
        ),
        Vec::<String>::new()
    );
}

/// The reset epoch a catalog at schema 57 keeps in the change journal's state row.
fn journal_epoch(connection: &Connection) -> i64 {
    count(
        connection,
        "SELECT epoch FROM change_journal_state WHERE singleton = 1",
    )
}

/// The reset epoch a catalog at schema 58 keeps.
fn reset_epoch(connection: &Connection) -> i64 {
    count(
        connection,
        "SELECT epoch FROM reset_epoch WHERE singleton = 1",
    )
}

/// A data directory at schema 57 whose resets moved the epoch on keeps it through the upgrade,
/// and the next reset counts on from it in the table that replaced the change journal.
#[tokio::test]
async fn the_reset_epoch_moves_out_of_the_change_journal_and_reset_counts_on_from_it() {
    let dir = TestDir::new();
    bucket_catalog(&dir.0).await.close().await.unwrap();
    let connection = at_schema_57(&dir.0);
    connection
        .execute_batch("UPDATE change_journal_state SET epoch = 7 WHERE singleton = 1")
        .unwrap();
    drop(connection);
    let open = || {
        Catalog::open_for_server(
            &dir.0,
            std::sync::Arc::new(crate::clock::SystemClock),
            crate::storage::admin::LocalAdmin::Enabled,
        )
    };

    open().await.unwrap().close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_retired(&connection);
    assert_eq!(reset_epoch(&connection), 7);
    drop(connection);

    let catalog = open().await.unwrap();
    catalog.local_admin().unwrap().reset().await.unwrap();
    assert!(
        catalog
            .get_bucket(BUCKET, PERMISSIVE)
            .await
            .unwrap()
            .is_none()
    );
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(reset_epoch(&connection), 8);
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
    assert_eq!(staged(&data), Vec::<String>::new());
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

/// The warning the commit gives names the manifest and counts the copies and, per folder in
/// the order of its path's bytes, the workspace entries.
#[tokio::test]
async fn the_warning_at_the_commit_counts_the_copies_and_each_folders_workspace_entries() {
    let released = released().await;
    let data = released.data();
    let folder = released.folder();
    let connection = Connection::open(data.join("catalog.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO sync_roots VALUES(?1,?2,?3,?4,1)",
            params![[2u8; 16], b"/srv/other", [0u8; 8], [2u8; 8]],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_recovery_workspaces VALUES(?1,?2,?3,?3,X'7B7D',0)",
            params![id(0xb1), [2u8; 16], id(0xe2)],
        )
        .unwrap();

    let warning = preserved::write_manifest(&connection, &data)
        .unwrap()
        .warning()
        .unwrap();
    let manifest = data.join(MANIFEST);
    assert_eq!(warning["manifest"], manifest.to_str().unwrap());
    assert_eq!(warning["conflict_copies"], 2);
    assert_eq!(
        warning["workspace_entries"],
        json!([
            {"folder": "/srv/other", "entries": 1},
            {"folder": folder.to_str().unwrap(), "entries": 4},
        ])
    );
    let message = warning["message"].as_str().unwrap();
    for part in [
        format!("{} lists the files", manifest.display()),
        format!(
            "2 conflict copies in {}",
            data.join("sync-conflicts").display()
        ),
        "5 workspace entries".to_owned(),
        format!("1 in /srv/other, 4 in {}", folder.display()),
        "never deletes".to_owned(),
        "delete the manifest to acknowledge it".to_owned(),
    ] {
        assert!(message.contains(&part), "{part}: {message}");
    }
}

/// Rows that name no file write no manifest, and leave one an earlier upgrade wrote as it is.
#[tokio::test]
async fn rows_that_name_no_file_retire_without_a_manifest() {
    for earlier in [None, Some(b"an earlier manifest".as_slice())] {
        let dir = TestDir::new();
        bucket_catalog(&dir.0).await.close().await.unwrap();
        assert!(!dir.0.join(MANIFEST).exists());
        let connection = at_schema_57(&dir.0);
        record_folder(&connection, &dir.0.join("folder"));
        // A prepared publish holds only its own copy of the object, a complete write-back and
        // a released workspace hold nothing, and a discarded copy's file was removed first.
        write_back(&connection, 0xa1, "object", 0, 0);
        write_back(&connection, 0xa4, "object", 0, 4);
        recovery(&connection, 0xa7, 2);
        conflict(&connection, 0xc3, "object", (Some(1), 2, 3), None);
        drop(connection);
        if let Some(earlier) = earlier {
            fs::write(dir.0.join(MANIFEST), earlier).unwrap();
        }

        Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
        assert_eq!(fs::read(dir.0.join(MANIFEST)).ok().as_deref(), earlier);
        assert!(!dir.0.join("sync-retired.1.json").exists());
        assert_eq!(staged(&dir.0), Vec::<String>::new());
        assert_retired(&Connection::open(dir.0.join("catalog.sqlite3")).unwrap());
    }
}

/// A manifest with other content, such as one the upgrade of a restored catalog wrote, keeps
/// its bytes: the new manifest takes the first free numbered name, and a retry after a failed
/// commit finds it there instead of writing another.
#[tokio::test]
async fn a_manifest_with_other_content_is_kept_and_the_next_free_name_is_used() {
    let released = released().await;
    let data = released.data();
    fault::arm(&data, Step::RetiredSyncCommit, 0, Fault::RollBack);
    assert!(Catalog::open(&data).await.is_err());
    // One taken name holds as many bytes as the manifest, the other a different number.
    let mut same_length = fs::read(data.join(MANIFEST)).unwrap();
    *same_length.last_mut().unwrap() = b' ';
    let taken = [
        (MANIFEST, same_length),
        ("sync-retired.1.json", b"edited by hand".to_vec()),
    ];
    for (name, bytes) in &taken {
        fs::write(data.join(name), bytes).unwrap();
    }
    let next = data.join("sync-retired.2.json");
    fault::arm(&data, Step::RetiredSyncCommit, 0, Fault::RollBack);
    assert!(Catalog::open(&data).await.is_err());
    let published = fs::read(&next).unwrap();

    Catalog::open(&data).await.unwrap().close().await.unwrap();
    assert_eq!(fs::read(&next).unwrap(), published);
    assert_eq!(read_manifest(&next), released.manifest());
    for (name, bytes) in &taken {
        assert_eq!(&fs::read(data.join(name)).unwrap(), bytes, "{name}");
    }
    assert!(!data.join("sync-retired.3.json").exists());
    assert_eq!(staged(&data), Vec::<String>::new());
    assert_retired(&Connection::open(data.join("catalog.sqlite3")).unwrap());
}

/// A name that something takes after the search found it free keeps what was put there: the
/// rename refuses to replace it, and the manifest goes to the next free name.
#[tokio::test(flavor = "multi_thread")]
async fn a_manifest_name_taken_before_the_rename_is_kept_and_the_next_name_is_used() {
    let released = released().await;
    let data = released.data();
    let (entered, reached) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    fault::arm(
        &data,
        Step::RetiredManifestRename,
        0,
        Fault::Block {
            entered: std::sync::Arc::new(std::sync::Mutex::new(Some(entered))),
            release: std::sync::Arc::new(std::sync::Mutex::new(held)),
        },
    );
    let open = tokio::spawn({
        let data = data.clone();
        async move { Catalog::open(&data).await }
    });
    reached.await.unwrap();
    fs::write(data.join(MANIFEST), b"written meanwhile\n").unwrap();
    release.send(()).unwrap();
    open.await.unwrap().unwrap().close().await.unwrap();

    assert_eq!(
        fs::read(data.join(MANIFEST)).unwrap(),
        b"written meanwhile\n"
    );
    assert_eq!(
        read_manifest(&data.join("sync-retired.1.json")),
        released.manifest()
    );
    assert_eq!(staged(&data), Vec::<String>::new());
    assert_retired(&Connection::open(data.join("catalog.sqlite3")).unwrap());
}

/// Each kind of row that names a file writes the manifest on its own, listing just that file.
#[tokio::test]
async fn each_kind_of_row_that_names_a_file_alone_writes_the_manifest() {
    /// What a case records, and the manifest it gives for the folder and the bucket's
    /// incarnation.
    type Case = (&'static str, fn(&Connection), fn(&Path, &str) -> Json);
    let cases: [Case; 3] = [
        (
            "recovery workspace",
            |connection| recovery(connection, 0xa6, 0),
            |folder, _| manifest(folder, &[], &[workspace_entry(folder, 0xa6, None)]),
        ),
        (
            "write-back",
            |connection| write_back(connection, 0xa3, "removed", 1, 1),
            |folder, incarnation| {
                let entry = workspace_entry(folder, 0xa3, Some((incarnation, "removed")));
                manifest(folder, &[], &[entry])
            },
        ),
        (
            "conflict copy",
            |connection| conflict(connection, 0xc1, "edited", (Some(1), 1, 3), None),
            |folder, incarnation| {
                let mut copy =
                    conflict_copy(folder, 0xc1, incarnation, "edited", (3, "registered"));
                copy["generation"] = json!(1);
                manifest(folder, &[copy], &[])
            },
        ),
    ];
    for (kind, record, expected) in cases {
        let dir = TestDir::new();
        let folder = dir.0.join("folder");
        bucket_catalog(&dir.0).await.close().await.unwrap();
        let connection = at_schema_57(&dir.0);
        let incarnation = record_folder(&connection, &folder);
        record(&connection);
        drop(connection);

        Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
        assert_eq!(
            read_manifest(&dir.0.join(MANIFEST)),
            expected(&folder, &incarnation),
            "{kind}"
        );
        assert_retired(&Connection::open(dir.0.join("catalog.sqlite3")).unwrap());
    }
}

/// Every former folder is listed, sorted by its path's bytes, whether or not a row names a file
/// in it; a path that is not UTF-8 keeps its bytes in base64 beside a readable form.
#[tokio::test]
async fn every_former_folder_is_listed_with_the_bytes_of_a_path_that_is_not_utf8() {
    let dir = TestDir::new();
    bucket_catalog(&dir.0).await.close().await.unwrap();
    let connection = at_schema_57(&dir.0);
    record_folder(&connection, &dir.0.join("folder"));
    let latin1 = b"/srv/caf\xe9".as_slice();
    connection
        .execute("UPDATE sync_roots SET location = ?1", [latin1])
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_roots VALUES(?1,?2,?3,?4,1)",
            params![[2u8; 16], b"/srv/other", [0u8; 8], [2u8; 8]],
        )
        .unwrap();
    recovery(&connection, 0xa6, 0);
    drop(connection);

    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
    let readable = "/srv/caf\u{fffd}";
    let mut entry = workspace_entry(Path::new(readable), 0xa6, None);
    entry["folder_base64"] = json!("L3Nydi9jYWbp");
    let mut expected = manifest(Path::new(readable), &[], &[entry]);
    expected["folders"] = json!([
        {"folder": readable, "folder_base64": "L3Nydi9jYWbp"},
        {"folder": "/srv/other"},
    ]);
    assert_eq!(read_manifest(&dir.0.join(MANIFEST)), expected);
    assert_retired(&Connection::open(dir.0.join("catalog.sqlite3")).unwrap());
}

/// A copy whose recorded observation no release could have written is still listed, marked as
/// having unreadable digests, and the upgrade goes on.
#[tokio::test]
async fn a_copy_whose_digests_cannot_be_read_is_listed_and_retires() {
    let dir = TestDir::new();
    let folder = dir.0.join("folder");
    bucket_catalog(&dir.0).await.close().await.unwrap();
    let connection = at_schema_57(&dir.0);
    let incarnation = record_folder(&connection, &folder);
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
        let mut copy = conflict_copy(&folder, number, &incarnation, object, (3, "registered"));
        copy["digests"] = json!("unreadable");
        copy
    };
    assert_eq!(
        read_manifest(&dir.0.join(MANIFEST)),
        manifest(
            &folder,
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
    connection
        .execute_batch("UPDATE change_journal_state SET epoch = 5 WHERE singleton = 1")
        .unwrap();
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
    // The epoch moved and the change journal dropped first, and both rolled back.
    assert_eq!(journal_epoch(&connection), 5);
    assert_eq!(
        names(
            &connection,
            "SELECT name FROM sqlite_schema WHERE name = 'reset_epoch'"
        ),
        Vec::<String>::new()
    );
    connection.execute_batch("DROP TABLE blocker").unwrap();
    retire(&mut connection, None).unwrap();
    assert_retired(&connection);
    assert_eq!(reset_epoch(&connection), 5);
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

/// A link at a staged manifest's name, to a file or to nothing, is removed rather than
/// written through.
#[cfg(unix)]
#[tokio::test]
async fn a_link_at_the_staged_name_is_removed_rather_than_followed() {
    for existing in [true, false] {
        let released = released().await;
        let data = released.data();
        let outside = released.dir.0.join("outside");
        if existing {
            fs::write(&outside, b"not the manifest").unwrap();
        }
        std::os::unix::fs::symlink(&outside, data.join(earlier_staged(1))).unwrap();

        Catalog::open(&data).await.unwrap().close().await.unwrap();
        assert_eq!(read_manifest(&data.join(MANIFEST)), released.manifest());
        assert_eq!(staged(&data), Vec::<String>::new());
        if existing {
            assert_eq!(fs::read(&outside).unwrap(), b"not the manifest");
        } else {
            assert!(!outside.exists());
        }
    }
}

/// The staged manifests earlier attempts left are removed whatever they hold, and only names of
/// exactly that form: a file that merely resembles one stays. One that cannot be removed, here
/// a directory, stays without failing the start, which stages under a name of its own.
#[tokio::test]
async fn staged_manifests_earlier_attempts_left_are_removed_and_nothing_else() {
    let released = released().await;
    let data = released.data();
    let left = [earlier_staged(1), earlier_staged(0xab)];
    for name in &left {
        fs::write(data.join(name), b"{\"partial\":").unwrap();
    }
    let kept = [
        "sync-retired.json.tmp".to_owned(),
        format!("{STAGED}{}.tmp", id(0xcd).to_uppercase()),
        format!("{STAGED}{}.tmp", &id(4)[1..]),
        format!("{STAGED}{}.tmp.bak", id(5)),
        "sync-retired.json.notes.tmp".to_owned(),
    ];
    for name in &kept {
        fs::write(data.join(name), b"mine").unwrap();
    }
    let stuck = data.join(earlier_staged(6));
    fs::create_dir(&stuck).unwrap();

    Catalog::open(&data).await.unwrap().close().await.unwrap();
    for name in &left {
        assert!(!data.join(name).exists(), "{name}");
    }
    for name in &kept {
        assert_eq!(fs::read(data.join(name)).unwrap(), b"mine", "{name}");
    }
    assert!(stuck.is_dir());
    assert_eq!(read_manifest(&data.join(MANIFEST)), released.manifest());
    assert_retired(&Connection::open(data.join("catalog.sqlite3")).unwrap());
}

/// A failure while the manifest is written, or of the transaction after it, leaves schema 57
/// and every file; the next open writes the same manifest and retires the rows. A manifest
/// step's error names the step and the file it worked on.
#[tokio::test]
async fn a_failed_manifest_step_or_commit_leaves_schema_57_and_the_next_open_finishes() {
    let full = || Fault::Io(io::ErrorKind::StorageFull);
    // The step that failed, and the file in the data directory it names, the staged manifest by
    // the start of its name; `None` names the directory itself.
    let write = Some(("write", Some(STAGED)));
    let faults = [
        (Step::RetiredManifestWrite, 0, full(), write),
        (Step::RetiredManifestWrite, 4, full(), write),
        (
            Step::RetiredManifestFlush,
            0,
            full(),
            Some(("flush", Some(STAGED))),
        ),
        (
            Step::RetiredManifestSync,
            0,
            Fault::Io(io::ErrorKind::Other),
            Some(("sync", Some(STAGED))),
        ),
        (
            Step::RetiredManifestRename,
            0,
            Fault::Io(io::ErrorKind::PermissionDenied),
            Some(("rename the staged file to", Some(MANIFEST))),
        ),
        #[cfg(unix)]
        (
            Step::RetiredManifestDirSync,
            0,
            Fault::Io(io::ErrorKind::Other),
            Some(("sync the directory", None)),
        ),
        (Step::RetiredSyncCommit, 0, Fault::RollBack, None),
        (Step::RetiredSyncCommit, 0, full(), None),
    ];
    for (step, skip, fault, context) in faults {
        let released = released().await;
        let data = released.data();
        let copies = files(&data.join("sync-conflicts"));
        let folder = files(&released.folder());
        fault::arm(&data, step, skip, fault);
        let Err(error) = Catalog::open(&data).await else {
            panic!("{step:?}: the open succeeded");
        };
        if let Some((phrase, file)) = context {
            let path = file.map_or_else(|| data.clone(), |file| data.join(file));
            let expected = format!("could not {phrase} {}", path.display());
            assert!(error.to_string().contains(&expected), "{step:?}: {error}");
        }
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
        assert_eq!(staged(&data), Vec::<String>::new(), "{step:?}");
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
            assert_eq!(
                staged(&snapshot),
                Vec::<String>::new(),
                "{crash:?} at {step:?}"
            );
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
