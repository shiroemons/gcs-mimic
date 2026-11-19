use super::*;
use crate::storage::catalog::fault::{self, Crash, Fault, Step};

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

/// Registers a root at `root`, completes its baseline and tracks a file of it, then moves its
/// directory into `aside` and puts an empty directory in its place. Returns the root and where
/// its directory went.
async fn replaced(catalog: &Catalog, root: &TestDir, aside: &TestDir) -> (RootId, PathBuf) {
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let previous = lease.status().root_id;
    lease.complete_baseline().await.unwrap();
    drop(lease);
    fs::write(root.0.join("tracked"), b"previous").unwrap();
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
    assert_eq!(registered(catalog).await, (vec![(previous, true)], 1));
    let saved = aside.0.join("previous");
    fs::rename(&root.0, &saved).unwrap();
    fs::create_dir(&root.0).unwrap();
    (previous, saved)
}

fn published(root: &Path) -> RootId {
    read_marker(&fsutil::open_directory_nofollow(root).unwrap())
        .unwrap()
        .unwrap()
        .0
        .root_id
}

/// An empty directory in a root's place, or one that holds only `.DS_Store`, becomes a new root
/// whose baseline is still to build. The catalog forgets the previous root and every tracked
/// file, the new directory gets a marker of its own beside its `.DS_Store`, and the previous
/// directory keeps its marker and its files. Reinitialization never switches.
#[tokio::test]
async fn an_empty_directory_in_a_root_place_becomes_a_new_root() {
    for finder in [false, true] {
        let (_data, root, catalog) = setup().await;
        let aside = TestDir::new();
        let (previous, saved) = replaced(&catalog, &root, &aside).await;
        let marker = fs::read(saved.join(MARKER)).unwrap();
        if finder {
            fs::write(root.0.join(".DS_Store"), b"finder").unwrap();
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
        drop(lease);
        assert_eq!(registered(&catalog).await, (vec![(switched, false)], 0));
        assert_eq!(published(&root.0), switched);
        assert!(!root.0.join(MARKER_STAGING).exists());
        assert_eq!(root.0.join(".DS_Store").exists(), finder);
        assert_eq!(fs::read(saved.join(MARKER)).unwrap(), marker);
        assert_eq!(fs::read(saved.join("tracked")).unwrap(), b"previous");

        let lease = state
            .activate_root(&root.0, Activation::Open)
            .await
            .unwrap();
        assert_eq!(lease.status().root_id, switched);
        drop(lease);
        catalog.close().await.unwrap();
    }
}

/// A directory in a root's place that holds anything but a regular `.DS_Store` is refused as
/// before: startup writes nothing into it and keeps the previous registration, so the previous
/// directory, put back, opens as the root it was.
#[tokio::test]
async fn a_directory_that_holds_anything_in_a_root_place_is_refused() {
    type Hold = fn(&Path);
    let held: [(&str, Hold); 4] = [
        ("file", |path| {
            fs::write(path.join("notes.txt"), b"mine").unwrap()
        }),
        ("apple double", |path| {
            fs::write(path.join("._notes.txt"), b"").unwrap();
        }),
        ("directory", |path| {
            fs::create_dir(path.join("test-bucket")).unwrap();
        }),
        ("finder directory", |path| {
            fs::create_dir(path.join(".DS_Store")).unwrap();
        }),
    ];
    for (case, hold) in held {
        let (_data, root, catalog) = setup().await;
        let aside = TestDir::new();
        let (previous, saved) = replaced(&catalog, &root, &aside).await;
        hold(&root.0);
        let state = catalog.sync_state();
        assert!(
            matches!(
                state.activate_root(&root.0, Activation::Open).await,
                Err(RootError::RootChanged)
            ),
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
        drop(lease);
        catalog.close().await.unwrap();
    }
}

/// After a switch, the previous directory put back in its place is refused: it is not empty, and
/// its marker names a root the catalog forgot. Startup leaves it as it was.
#[tokio::test]
async fn the_previous_directory_is_refused_after_a_switch() {
    let (_data, root, catalog) = setup().await;
    let aside = TestDir::new();
    let (_, saved) = replaced(&catalog, &root, &aside).await;
    let marker = fs::read(saved.join(MARKER)).unwrap();
    let state = catalog.sync_state();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let switched = lease.status().root_id;
    drop(lease);

    fs::rename(&root.0, aside.0.join("switched")).unwrap();
    fs::rename(&saved, &root.0).unwrap();
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::RootChanged)
    ));
    assert_eq!(fs::read(root.0.join(MARKER)).unwrap(), marker);
    assert_eq!(fs::read(root.0.join("tracked")).unwrap(), b"previous");
    assert!(!root.0.join(MARKER_STAGING).exists());
    assert_eq!(registered(&catalog).await, (vec![(switched, false)], 0));
    catalog.close().await.unwrap();
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
