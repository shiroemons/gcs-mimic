use super::super::switch as rules;
use super::*;
use crate::storage::catalog::fault::{self, Crash, Fault, Step};
use std::sync::{Arc as SharedArc, Mutex as SharedMutex};

/// The registered roots with whether each baseline is complete, and how many files the catalog
/// tracks.
async fn registered(catalog: &Catalog) -> (Vec<(RootId, bool)>, i64) {
    catalog
        .run_admin(|connection| {
            let mut roots = connection
                .prepare("SELECT root_id, complete FROM sync_roots ORDER BY root_id")
                .unwrap();
            let roots = roots
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let tracked = connection
                .query_row("SELECT count(*) FROM sync_objects", [], |row| row.get(0))
                .unwrap();
            Ok((roots, tracked))
        })
        .await
        .unwrap()
}

/// Tracks a file named `tracked`, as an export or ingestion of the root would.
async fn track(catalog: &Catalog) {
    catalog
        .run_admin(|connection| {
            connection
                .execute(
                    "INSERT INTO sync_objects VALUES('test-bucket','tracked','00000000000000000000000000000000','tracked',1,8,zeroblob(16),zeroblob(16),zeroblob(8),zeroblob(16),0,0)",
                    [],
                )
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
}

/// Registers a root at `root` and completes its baseline, with a file of it the catalog tracks.
async fn synchronized(catalog: &Catalog, root: &Path) -> RootId {
    let lease = catalog
        .sync_state()
        .activate_root(root, Activation::Open)
        .await
        .unwrap();
    let id = lease.status().root_id;
    lease.complete_baseline().await.unwrap();
    drop(lease);
    fs::write(root.join("tracked"), b"previous").unwrap();
    track(catalog).await;
    assert_eq!(registered(catalog).await, (vec![(id, true)], 1));
    id
}

/// Registers a root at `root`, completes its baseline and tracks a file of it, then moves its
/// directory into `aside` and puts an empty directory in its place. Returns the root and where
/// its directory went.
async fn replaced(catalog: &Catalog, root: &TestDir, aside: &TestDir) -> (RootId, PathBuf) {
    let previous = synchronized(catalog, &root.0).await;
    let saved = aside.0.join("previous");
    fs::rename(&root.0, &saved).unwrap();
    fs::create_dir(&root.0).unwrap();
    (previous, saved)
}

/// Registers another root beside the registered ones, as an earlier release could, at
/// `location` with the directory identity `node`.
async fn register_beside(catalog: &Catalog, location: &[u8], node: NodeId) -> RootId {
    let id = RootId::generate().unwrap();
    let location = location.to_vec();
    catalog
        .run_admin(move |connection| {
            let (device, inode) = node.to_be_bytes();
            connection
                .execute(
                    "INSERT INTO sync_roots VALUES (?1,?2,?3,?4,1)",
                    params![id, location, device, inode],
                )
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    id
}

fn node(path: &Path) -> NodeId {
    fsutil::stat_identity(&fsutil::open_directory_nofollow(path).unwrap())
        .unwrap()
        .node()
}

fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn published(root: &Path) -> RootId {
    read_marker(
        &fsutil::open_directory_nofollow(root).unwrap(),
        &fsutil::Lookup::unprobed(),
    )
    .unwrap()
    .unwrap()
    .0
    .root_id
}

/// Every table with a `root_id` column loses the rows of a root the catalog forgets: a statement
/// of [`rules::FORGET`] deletes them, or a foreign key deletes them with the rows it references.
#[tokio::test]
async fn forgetting_a_root_reaches_every_table_that_holds_its_rows() {
    let (_data, _root, catalog) = setup().await;
    catalog
        .run_admin(|connection| {
            let tables = connection
                .prepare(
                    "SELECT DISTINCT m.name FROM sqlite_schema m, pragma_table_info(m.name) p
                     WHERE m.type = 'table' AND p.name = 'root_id' ORDER BY m.name",
                )
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let forgotten = rules::FORGET
                .iter()
                .map(|statement| {
                    statement
                        .strip_prefix("DELETE FROM ")
                        .and_then(|rest| rest.strip_suffix(" WHERE root_id=?1"))
                        .unwrap()
                })
                .collect::<Vec<_>>();
            for table in &tables {
                assert_eq!(
                    usize::from(forgotten.contains(&table.as_str()))
                        + usize::from(rules::CASCADED.contains(&table.as_str())),
                    1,
                    "missing or ambiguous: {table}"
                );
            }
            for table in forgotten.iter().chain(rules::CASCADED) {
                assert!(tables.iter().any(|name| name == table), "obsolete: {table}");
            }
            for table in rules::CASCADED {
                let cascades = connection
                    .prepare("SELECT \"table\", on_delete FROM pragma_foreign_key_list(?1)")
                    .unwrap()
                    .query_map([table], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert!(
                    cascades.iter().any(|(parent, action)| action == "CASCADE"
                        && forgotten.contains(&parent.as_str())),
                    "{table} does not follow a forgotten table: {cascades:?}"
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

/// An empty directory in a root's place, or one that holds only files directly in it that a scan
/// never makes objects of, becomes a new root whose baseline is still to build. The catalog
/// forgets the previous root and every tracked file, the new directory gets a marker of its own
/// beside its files, and the previous directory keeps its marker and its files.
/// Reinitialization never switches.
#[tokio::test]
async fn an_empty_directory_in_a_root_place_becomes_a_new_root() {
    let cases: [&[&str]; 3] = [&[], &[".DS_Store"], &[".gitkeep", "README.md", "._notes"]];
    for files in cases {
        let (_data, root, catalog) = setup().await;
        let aside = TestDir::new();
        let (previous, saved) = replaced(&catalog, &root, &aside).await;
        let marker = fs::read(saved.join(MARKER)).unwrap();
        for name in files {
            fs::write(root.0.join(name), name).unwrap();
        }
        let state = catalog.sync_state();
        let id = state.catalog_id().await.unwrap();
        assert!(matches!(
            state
                .activate_root(&root.0, Activation::Reinitialize { catalog_id: id })
                .await,
            Err(RootError::RootChanged)
        ));
        assert!(!root.0.join(MARKER_STAGING).exists());

        let lease = state
            .activate_root(&root.0, Activation::Open)
            .await
            .unwrap();
        let switched = lease.status().root_id;
        assert_ne!(switched, previous);
        assert_eq!(lease.status().baseline, Baseline::Activating);
        let change = lease.change().unwrap();
        assert_eq!(change.reason, "empty_directory");
        assert_eq!(change.previous_paths, [canonical(&root.0)]);
        drop(lease);
        assert_eq!(registered(&catalog).await, (vec![(switched, false)], 0));
        assert_eq!(published(&root.0), switched);
        assert!(!root.0.join(MARKER_STAGING).exists());
        for name in files {
            assert_eq!(fs::read(root.0.join(name)).unwrap(), name.as_bytes());
        }
        assert_eq!(fs::read(saved.join(MARKER)).unwrap(), marker);
        assert_eq!(fs::read(saved.join("tracked")).unwrap(), b"previous");

        let lease = state
            .activate_root(&root.0, Activation::Open)
            .await
            .unwrap();
        assert_eq!(lease.status().root_id, switched);
        assert!(lease.change().is_none());
        drop(lease);
        catalog.close().await.unwrap();
    }
}

/// A directory without a marker in a root's place that holds a directory, a symlink or a file
/// below it is refused, naming the directory the data directory synchronizes: startup writes
/// nothing into it and keeps the previous registration, so the previous directory, put back,
/// opens as the root it was.
#[tokio::test]
async fn a_directory_that_holds_more_than_root_files_in_a_root_place_is_refused() {
    type Hold = fn(&Path);
    let held: [(&str, Hold); 4] = [
        ("directory", |path| {
            fs::create_dir(path.join("test-bucket")).unwrap();
        }),
        ("file below", |path| {
            fs::create_dir(path.join("test-bucket")).unwrap();
            fs::write(path.join("test-bucket/notes.txt"), b"mine").unwrap();
        }),
        ("finder directory", |path| {
            fs::create_dir(path.join(".DS_Store")).unwrap();
        }),
        ("symlink", |path| {
            fs::write(path.join("README.md"), b"mine").unwrap();
            symlink("README.md", path.join("link")).unwrap();
        }),
    ];
    for (case, hold) in held {
        let (_data, root, catalog) = setup().await;
        let aside = TestDir::new();
        let (previous, saved) = replaced(&catalog, &root, &aside).await;
        hold(&root.0);
        let state = catalog.sync_state();
        assert_eq!(
            state.activate_root(&root.0, Activation::Open).await.err(),
            Some(RootError::ReplacementOccupied {
                synchronized: vec![canonical(&root.0)]
            }),
            "{case}"
        );
        assert!(!root.0.join(MARKER).exists(), "{case}");
        assert!(!root.0.join(MARKER_STAGING).exists(), "{case}");
        assert_eq!(
            registered(&catalog).await,
            (vec![(previous, true)], 1),
            "{case}"
        );

        fs::rename(&root.0, aside.0.join("refused")).unwrap();
        fs::rename(&saved, &root.0).unwrap();
        let lease = state
            .activate_root(&root.0, Activation::Open)
            .await
            .unwrap();
        assert_eq!(lease.status().root_id, previous, "{case}");
        assert_eq!(lease.status().baseline, Baseline::Complete, "{case}");
        assert!(lease.change().is_none(), "{case}");
        drop(lease);
        catalog.close().await.unwrap();
    }
}

/// After a switch, the previous directory put back in its place holds this data directory's
/// marker for a root it forgot. Startup merges it under that root and keeps its marker, forgets
/// the root it switched to and every tracked file, and builds the baseline again, so none of the
/// files of the directory it switched to counts as missing.
#[tokio::test]
async fn the_previous_directory_is_merged_after_a_switch() {
    let (_data, root, catalog) = setup().await;
    let aside = TestDir::new();
    let (previous, saved) = replaced(&catalog, &root, &aside).await;
    let marker = fs::read(saved.join(MARKER)).unwrap();
    let state = catalog.sync_state();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    drop(lease);
    track(&catalog).await;

    fs::rename(&root.0, aside.0.join("switched")).unwrap();
    fs::rename(&saved, &root.0).unwrap();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    assert_eq!(lease.status().root_id, previous);
    assert_eq!(lease.status().baseline, Baseline::Activating);
    assert_eq!(lease.change().unwrap().reason, "marked_directory");
    drop(lease);
    assert_eq!(fs::read(root.0.join(MARKER)).unwrap(), marker);
    assert_eq!(fs::read(root.0.join("tracked")).unwrap(), b"previous");
    assert!(!root.0.join(MARKER_STAGING).exists());
    assert_eq!(registered(&catalog).await, (vec![(previous, false)], 0));
    catalog.close().await.unwrap();
}

/// A root moved to another path, as `mv` does, keeps its marker and its directory. Startup
/// merges it under the same root at the new path, forgetting the tracked files, and the status
/// names the path it came from.
#[tokio::test]
async fn a_root_moved_to_another_path_is_merged_under_its_root() {
    let (_data, root, catalog) = setup().await;
    let aside = TestDir::new();
    let id = synchronized(&catalog, &root.0).await;
    let before = canonical(&root.0);
    let moved = aside.0.join("moved");
    fs::rename(&root.0, &moved).unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&moved, Activation::Open)
        .await
        .unwrap();
    assert_eq!(lease.status().root_id, id);
    assert_eq!(lease.status().baseline, Baseline::Activating);
    let change = lease.change().unwrap();
    assert_eq!(change.reason, "marked_directory");
    assert_eq!(change.previous_paths, [before]);
    drop(lease);
    assert_eq!(published(&moved), id);
    assert_eq!(registered(&catalog).await, (vec![(id, false)], 0));
    catalog.close().await.unwrap();
    fs::rename(&moved, &root.0).unwrap();
}

/// A catalog that an earlier release let register another root beside the one it opens keeps
/// only that one, forgets the tracked files, which may be the other tree's, and builds the
/// baseline again. Reinitializing such a root does the same.
#[tokio::test]
async fn a_catalog_with_several_roots_keeps_the_one_it_opens() {
    for reinitialize in [false, true] {
        let (_data, root, catalog) = setup().await;
        let id = synchronized(&catalog, &root.0).await;
        register_beside(
            &catalog,
            b"/elsewhere",
            NodeId::from_be_bytes([0; 8], [1; 8]),
        )
        .await;
        let state = catalog.sync_state();
        let activation = if reinitialize {
            fs::remove_file(root.0.join(MARKER)).unwrap();
            Activation::Reinitialize {
                catalog_id: state.catalog_id().await.unwrap(),
            }
        } else {
            Activation::Open
        };
        let lease = state.activate_root(&root.0, activation).await.unwrap();
        assert_eq!(lease.status().root_id, id, "{reinitialize}");
        assert_eq!(lease.status().baseline, Baseline::Activating);
        let change = lease.change().unwrap();
        assert_eq!(change.reason, "several_roots");
        let mut paths = change.previous_paths.clone();
        paths.sort();
        let mut expected = vec![canonical(&root.0), "/elsewhere".to_owned()];
        expected.sort();
        assert_eq!(paths, expected);
        drop(lease);
        assert_eq!(published(&root.0), id);
        assert_eq!(registered(&catalog).await, (vec![(id, false)], 0));
        catalog.close().await.unwrap();
    }
}

/// When the selected directory has one registered root's path and another's identity, as when
/// an old registration's inode is reused, both are the roots it takes the place of: an empty
/// directory replaces both, and one that holds more is refused before any marker is staged.
#[tokio::test]
async fn a_directory_that_two_registrations_match_apart_replaces_both_or_is_refused() {
    for occupied in [false, true] {
        let (_data, root, catalog) = setup().await;
        let aside = TestDir::new();
        let (previous, _) = replaced(&catalog, &root, &aside).await;
        let other = register_beside(&catalog, b"/reused", node(&root.0)).await;
        if occupied {
            fs::create_dir(root.0.join("test-bucket")).unwrap();
        }
        let mut before = vec![(previous, true), (other, true)];
        before.sort();
        let state = catalog.sync_state();
        let result = state.activate_root(&root.0, Activation::Open).await;
        assert!(!root.0.join(MARKER_STAGING).exists(), "{occupied}");
        if occupied {
            let mut synchronized = match result {
                Err(RootError::ReplacementOccupied { synchronized }) => synchronized,
                other => panic!("{:?}", other.err()),
            };
            synchronized.sort();
            let mut expected = vec![canonical(&root.0), "/reused".to_owned()];
            expected.sort();
            assert_eq!(synchronized, expected);
            assert!(!root.0.join(MARKER).exists());
            assert_eq!(registered(&catalog).await, (before, 1));
        } else {
            let lease = result.unwrap();
            let switched = lease.status().root_id;
            assert_eq!(lease.change().unwrap().previous_paths.len(), 2);
            drop(lease);
            assert_eq!(registered(&catalog).await, (vec![(switched, false)], 0));
            assert_eq!(published(&root.0), switched);
        }
        catalog.close().await.unwrap();
    }
}

/// The switch transaction rechecks the registrations it replaces. When they changed after the
/// marker was staged, it registers nothing and removes the staged marker, which no root owns.
#[tokio::test]
async fn a_switch_whose_registrations_changed_removes_its_staged_marker() {
    let (_data, root, catalog) = setup().await;
    let aside = TestDir::new();
    let (previous, _) = replaced(&catalog, &root, &aside).await;
    let (entered, reached) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    fault::arm(
        &root.0,
        Step::SyncRootSwitchStaged,
        0,
        Fault::Block {
            entered: SharedArc::new(SharedMutex::new(Some(entered))),
            release: SharedArc::new(SharedMutex::new(released)),
        },
    );
    let state = catalog.sync_state();
    let path = root.0.clone();
    let switching = tokio::spawn(async move { state.activate_root(&path, Activation::Open).await });
    reached.await.unwrap();
    assert!(root.0.join(MARKER_STAGING).exists());
    let other = register_beside(&catalog, b"/raced", NodeId::from_be_bytes([0; 8], [2; 8])).await;
    release.send(()).unwrap();
    assert!(matches!(
        switching.await.unwrap(),
        Err(RootError::RootChanged)
    ));
    assert!(!root.0.join(MARKER_STAGING).exists());
    assert!(!root.0.join(MARKER).exists());
    let mut expected = vec![(previous, true), (other, true)];
    expected.sort();
    assert_eq!(registered(&catalog).await, (expected, 1));
    catalog.close().await.unwrap();
}

/// A failure while writing or syncing the staged marker, or in the switch transaction before it
/// commits, keeps the previous registration and its tracked files. The next start removes what
/// was staged, which no registered root owns, and switches.
#[tokio::test]
async fn failures_while_switching_keep_the_previous_registration() {
    for (step, in_catalog) in [
        (Step::SyncMarkerWrite, false),
        (Step::SyncMarkerSync, false),
        (Step::SyncRootSwitchStaged, true),
    ] {
        let case = format!("{step:?}");
        let (_data, root, catalog) = setup().await;
        let aside = TestDir::new();
        let (previous, _) = replaced(&catalog, &root, &aside).await;
        let failure = Fault::Io(io::ErrorKind::Other);
        if in_catalog {
            catalog.inject(step, 0, failure);
        } else {
            fault::arm(&root.0, step, 0, failure);
        }
        let state = catalog.sync_state();
        assert!(
            matches!(
                state.activate_root(&root.0, Activation::Open).await,
                Err(RootError::Admin(_))
            ),
            "{case}"
        );
        assert_eq!(fault::pending(&root.0), 0, "{case}");
        assert_eq!(catalog.pending_faults(), 0, "{case}");
        assert!(!root.0.join(MARKER).exists(), "{case}");
        assert_eq!(
            registered(&catalog).await,
            (vec![(previous, true)], 1),
            "{case}"
        );

        let lease = state
            .activate_root(&root.0, Activation::Open)
            .await
            .unwrap();
        let switched = lease.status().root_id;
        assert_ne!(switched, previous, "{case}");
        drop(lease);
        assert_eq!(
            registered(&catalog).await,
            (vec![(switched, false)], 0),
            "{case}"
        );
        assert_eq!(published(&root.0), switched, "{case}");
        assert!(!root.0.join(MARKER_STAGING).exists(), "{case}");
        catalog.close().await.unwrap();
    }
}

/// A crash or power loss while switching leaves either the previous registration or the new
/// one, never a mix. Before the switch commits, the catalog still registers the previous root and
/// the next start removes the staged marker, which no registered root owns, and switches again;
/// after it commits, the next start publishes the staged marker of the new root.
#[tokio::test]
async fn crashes_and_power_losses_in_a_switch_leave_the_previous_or_the_new_root() {
    for (step, committed) in [
        (Step::SyncRootSwitchStaged, false),
        (Step::SyncRootSwitched, true),
    ] {
        for &crash in Crash::ALL {
            let case = format!("{step:?} {crash:?}");
            let (_data, root, catalog) = setup().await;
            let aside = TestDir::new();
            let (previous, _) = replaced(&catalog, &root, &aside).await;
            let snapshots = TestDir::new();
            let (snapshot_data, snapshot_tree) =
                (snapshots.0.join("data"), snapshots.0.join("tree"));
            fault::arm(&root.0, step, 0, crash.fault(snapshot_tree.clone()));
            catalog.inject(step, 0, Fault::Snapshot(snapshot_data.clone()));
            let lease = catalog
                .sync_state()
                .activate_root(&root.0, Activation::Open)
                .await
                .unwrap();
            let switched = lease.status().root_id;
            drop(lease);
            assert_eq!(fault::pending(&root.0), 0, "{case}");
            assert_eq!(catalog.pending_faults(), 0, "{case}");
            catalog.close().await.unwrap();

            // The directory keeps its identity; only the markers go back to the snapshot.
            assert!(snapshot_tree.join(MARKER_STAGING).exists(), "{case}");
            for name in [MARKER, MARKER_STAGING] {
                if root.0.join(name).exists() {
                    fs::remove_file(root.0.join(name)).unwrap();
                }
                if snapshot_tree.join(name).exists() {
                    fs::copy(snapshot_tree.join(name), root.0.join(name)).unwrap();
                }
            }
            let reopened = Catalog::open(&snapshot_data).await.unwrap();
            let registration = if committed {
                (vec![(switched, false)], 0)
            } else {
                (vec![(previous, true)], 1)
            };
            assert_eq!(registered(&reopened).await, registration, "{case}");

            let lease = reopened
                .sync_state()
                .activate_root(&root.0, Activation::Open)
                .await
                .unwrap();
            let resumed = lease.status().root_id;
            assert_ne!(resumed, previous, "{case}");
            if committed {
                assert_eq!(resumed, switched, "{case}");
            }
            assert_eq!(lease.status().baseline, Baseline::Activating, "{case}");
            drop(lease);
            assert_eq!(
                registered(&reopened).await,
                (vec![(resumed, false)], 0),
                "{case}"
            );
            assert_eq!(published(&root.0), resumed, "{case}");
            assert!(!root.0.join(MARKER_STAGING).exists(), "{case}");
            reopened.close().await.unwrap();
        }
    }
}

/// The warning about a change names what the catalog forgot of the previous root: the counts,
/// and the first retained conflicts and recovery workspaces with the paths of their bytes, which
/// nothing leads to once their rows are gone. A discarded conflict is not retained.
#[tokio::test]
async fn a_change_lists_the_conflicts_recoveries_and_writebacks_it_forgets() {
    let (_data, root, catalog) = setup().await;
    let previous = synchronized(&catalog, &root.0).await;
    let location = canonical(&root.0);
    let event = catalog
        .run_admin(move |connection| {
            let transaction = connection.transaction().unwrap();
            transaction.execute_batch(&format!(
                "INSERT INTO sync_writeback_roots VALUES (x'{root}',1,1,zeroblob(16),zeroblob(16),0,0,0,0,zeroblob(8),zeroblob(8),NULL,NULL);
                 INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c)
                     VALUES ('{recovery}',x'{root}','owner','test-bucket','{incarnation}','kept','test-bucket/kept',7,0,2,1,0,0,zeroblob(8),0,zeroblob(16),0),
                            ('{writeback}',x'{root}','owner','test-bucket','{incarnation}','pending','test-bucket/pending',8,0,0,1,0,0,zeroblob(8),0,zeroblob(16),0);
                 INSERT INTO sync_recovery_workspaces VALUES ('{recovery}',x'{root}','{incarnation}','{incarnation}',x'',0);
                 INSERT INTO sync_conflicts(id,root_id,revision,owner,bucket,incarnation,name,generation,state,size,source)
                     VALUES ('{conflict}',x'{root}','{incarnation}','owner','test-bucket','{incarnation}','edited',5,1,0,x''),
                            ('{discarded}',x'{root}','{incarnation}','owner','test-bucket','{incarnation}','gone',6,2,0,x'');",
                root = previous.to_string(),
                recovery = "1".repeat(32),
                writeback = "2".repeat(32),
                conflict = "3".repeat(32),
                discarded = "4".repeat(32),
                incarnation = "5".repeat(32),
            ))
            .unwrap();
            let roots = rules::registered(&transaction).unwrap();
            let change = rules::rebind(
                &transaction,
                rules::Reason::EmptyDirectory,
                roots,
                RootId::generate().unwrap(),
                b"/switched",
                NodeId::from_be_bytes([0; 8], [3; 8]),
            )
            .unwrap();
            Ok(change.event())
        })
        .await
        .unwrap();
    assert_eq!(event["reason"], "empty_directory");
    assert_eq!(event["path"], "/switched");
    assert_eq!(event["previous_roots"][0]["path"], location.as_str());
    assert_eq!(event["forgotten_tracked_files"], 1);
    assert_eq!(event["forgotten_writebacks"], 2);
    assert_eq!(event["forgotten_retained_conflicts"], 1);
    assert_eq!(
        event["forgotten_conflicts"],
        serde_json::json!([{
            "bucket": "test-bucket",
            "object": "edited",
            "generation": 5,
            "path": format!("sync-conflicts/{}", "3".repeat(32)),
        }])
    );
    assert_eq!(event["forgotten_recovery_workspaces"], 1);
    assert_eq!(
        event["forgotten_recoveries"],
        serde_json::json!([{
            "bucket": "test-bucket",
            "object": "kept",
            "generation": 7,
            "root_path": location,
            "path": format!(".gcs-mimic-sync-work/{}", "1".repeat(32)),
        }])
    );
    assert!(
        event["remedy"]
            .as_str()
            .unwrap()
            .contains("sync conflicts no longer lists")
    );
    catalog.close().await.unwrap();
}
