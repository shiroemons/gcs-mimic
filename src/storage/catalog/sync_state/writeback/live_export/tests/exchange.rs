//! What reaches a tracked target after installation checked it and before the exchange moves it
//! into the private entry. The body the exchange displaced is judged once, after the exchange, by
//! its bytes: the tracked body is unlinked whatever its inode number, modification time and
//! ctime say, as restart recovery unlinks it after a crash at the same point, and any other bytes
//! stay protected.

use super::*;
use crate::storage::catalog::fault::{Fault, Step};
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

/// The tracked target's path in the fixture's tree.
const TARGET: &str = "test-bucket/object";

/// Exports `api` and then `later api` over it, and returns the second export before its first
/// step: its target holds the tracked body `api`.
async fn begin_over_exported(fixture: &Fixture) -> LiveExport {
    fixture.publish("object", b"api").await;
    drain(&fixture.writeback, fixture.begin("object").await).await;
    fixture.publish("object", b"later api").await;
    fixture.begin_over_tracked("object").await
}

/// Advances `operation` until it ends, running `change` on its target while the installation
/// attempt waits after every check of the target and before the rename that exchanges the target
/// with the copy or, for a removal, moves it into the private entry.
pub(super) async fn race_exchange(
    fixture: &Fixture,
    operation: &LiveExport,
    change: impl FnOnce(&Path),
) -> Result<InstallOutcome, Error> {
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    fixture.catalog.inject(
        Step::SyncCopyRename,
        0,
        Fault::Block {
            entered: Arc::new(std::sync::Mutex::new(Some(entered))),
            release: Arc::new(std::sync::Mutex::new(blocked)),
        },
    );
    let writeback = fixture.writeback.clone();
    let selected = operation.clone();
    let work = crate::task::spawn_supervised("exchange-race-test", async move {
        for _ in 0..8 {
            let result = writeback.advance_live_export(selected.clone()).await?;
            if result != InstallOutcome::Pending {
                return Ok(result);
            }
        }
        panic!("the export did not end")
    });
    started.await.unwrap();
    change(&fixture.tree.0.join(TARGET));
    release.send(()).unwrap();
    work.await.unwrap()
}

/// The private entry of `operation`'s row.
pub(super) async fn private_entry(fixture: &Fixture, operation: &LiveExport) -> std::path::PathBuf {
    let item = pending(operation).await;
    fixture
        .tree
        .0
        .join(mapping::WORKSPACE)
        .join(item.token.id.to_string())
}

/// What happens to the tracked body between the check and the exchange without changing its
/// bytes.
#[derive(Clone, Copy, Debug)]
enum Unchanged {
    /// Its permissions change, as a permission repair makes them, which changes its ctime.
    Permissions,
    /// It comes back under another inode number with the same bytes and modification time, as
    /// Docker Desktop's shared folders can report an unchanged file.
    Renumbered,
    /// Only its modification time moves, as a `touch` moves it.
    Touched,
}

impl Unchanged {
    const ALL: [Self; 3] = [Self::Permissions, Self::Renumbered, Self::Touched];

    fn apply(self, path: &Path) {
        let before = fs::metadata(path).unwrap();
        match self {
            Self::Permissions => {
                let mode = before.permissions().mode() ^ 0o040;
                fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
            }
            Self::Renumbered => {
                let copy = path.with_file_name("object.renumbered");
                fs::write(&copy, fs::read(path).unwrap()).unwrap();
                File::options()
                    .write(true)
                    .open(&copy)
                    .unwrap()
                    .set_modified(before.modified().unwrap())
                    .unwrap();
                fs::rename(&copy, path).unwrap();
            }
            Self::Touched => File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(before.modified().unwrap() + std::time::Duration::from_secs(1))
                .unwrap(),
        }
        let after = fs::metadata(path).unwrap();
        let (before, after) = (
            fsutil::FileIdentity::from(&before),
            fsutil::FileIdentity::from(&after),
        );
        let changes = before.changed_fields(&after);
        let has = |wanted: fsutil::IdentityField| changes.iter().any(|field| field == wanted);
        assert!(
            !changes.is_empty()
                && !has(fsutil::IdentityField::Size)
                && has(fsutil::IdentityField::Mtime) == matches!(self, Self::Touched),
            "{self:?} changed {changes:?}"
        );
    }
}

/// A tracked target whose permissions change, that comes back under another inode number or
/// whose modification time alone moves after installation checked it and before the exchange
/// still holds the tracked body. The export completes over it rather than stopping the root: the
/// exchange moved the tracked body into the private entry, its bytes prove it, and it is
/// unlinked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tracked_target_with_its_bytes_kept_before_the_exchange_is_replaced() {
    for change in Unchanged::ALL {
        let fixture = Fixture::new().await;
        let operation = begin_over_exported(&fixture).await;
        let result = race_exchange(&fixture, &operation, |path| change.apply(path)).await;
        assert!(
            matches!(result, Ok(InstallOutcome::Complete)),
            "{change:?}: {result:?}"
        );
        assert_eq!(
            fs::read(fixture.tree.0.join(TARGET)).unwrap(),
            b"later api",
            "{change:?}"
        );
        assert!(
            !private_entry(&fixture, &operation).await.exists(),
            "{change:?}"
        );
        let usage = fixture.writeback.status().await.unwrap();
        assert_eq!(
            (usage.entries, usage.protected_entries, usage.stop()),
            (0, 0, Stop::Ready),
            "{change:?}"
        );
        drop(operation);
        fixture.close().await;
    }
}

/// A tracked target that holds other bytes by the exchange, of the same size and modification
/// time, or that a symlink took, is not the tracked body. The exchange already moved it into the
/// private entry; it stays there, protected, and the root stops for an operator. The target holds
/// the API copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tracked_target_edited_before_the_exchange_stays_in_the_private_entry() {
    for symlink in [false, true] {
        let fixture = Fixture::new().await;
        let operation = begin_over_exported(&fixture).await;
        let result = race_exchange(&fixture, &operation, |path| {
            let modified = fs::metadata(path).unwrap().modified().unwrap();
            if symlink {
                fs::remove_file(path).unwrap();
                std::os::unix::fs::symlink("elsewhere", path).unwrap();
            } else {
                fs::write(path, b"API").unwrap();
                File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(modified)
                    .unwrap();
            }
        })
        .await;
        let entry = private_entry(&fixture, &operation).await;
        let usage = fixture.writeback.status().await.unwrap();
        if symlink {
            // Protecting the entry fails to open it as the regular file a body would be, which
            // stops the root as an entry failure rather than counting a protected body.
            assert!(result.is_err(), "{result:?}");
            assert_eq!(fs::read_link(&entry).unwrap(), Path::new("elsewhere"));
            assert_eq!(usage.stop(), Stop::IoFailure);
        } else {
            assert!(matches!(result, Err(Error::Protected)), "{result:?}");
            assert_eq!(fs::read(&entry).unwrap(), b"API");
            assert_eq!(
                (usage.protected_entries, usage.stop()),
                (1, Stop::ProtectedBody)
            );
        }
        assert_eq!(
            fs::read(fixture.tree.0.join(TARGET)).unwrap(),
            b"later api",
            "symlink: {symlink}"
        );
        drop(operation);
        fixture.close().await;
    }
}

/// What takes the tracked target's name after installation checked it and before the exchange.
#[derive(Clone, Copy, Debug)]
pub(super) enum Taker {
    /// A directory with a user's file in it.
    Directory,
    /// A FIFO, which no read of a body may open.
    Fifo,
    /// A file of another size under another inode number.
    LongerFile,
}

impl Taker {
    const ALL: [Self; 3] = [Self::Directory, Self::Fifo, Self::LongerFile];

    pub(super) fn apply(self, path: &Path) {
        fs::remove_file(path).unwrap();
        match self {
            Self::Directory => {
                fs::create_dir(path).unwrap();
                fs::write(path.join("inside"), b"user").unwrap();
            }
            Self::Fifo => crate::test_support::fifo(path),
            Self::LongerFile => fs::write(path, b"a longer user body").unwrap(),
        }
    }
}

/// An entry other than the tracked body that takes the target between installation's check and
/// the exchange is what the exchange moves into the private entry, and it stays there, whole: a
/// directory with a file in it or a FIFO stops the root on the entry failure, and a file of
/// another size stops it as a protected body. The target holds the API copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_that_took_a_tracked_target_before_the_exchange_stays_in_the_private_entry() {
    for taker in Taker::ALL {
        let fixture = Fixture::new().await;
        let operation = begin_over_exported(&fixture).await;
        let result = race_exchange(&fixture, &operation, |path| taker.apply(path)).await;
        let entry = private_entry(&fixture, &operation).await;
        let usage = fixture.writeback.status().await.unwrap();
        match taker {
            Taker::Directory => {
                assert!(result.is_err(), "{taker:?}: {result:?}");
                assert_eq!(fs::read(entry.join("inside")).unwrap(), b"user");
                assert_eq!(usage.stop(), Stop::IoFailure);
            }
            Taker::Fifo => {
                assert!(result.is_err(), "{taker:?}: {result:?}");
                assert!(
                    std::os::unix::fs::FileTypeExt::is_fifo(
                        &fs::symlink_metadata(&entry).unwrap().file_type()
                    ),
                    "{taker:?}"
                );
                assert_eq!(usage.stop(), Stop::IoFailure);
            }
            Taker::LongerFile => {
                assert!(matches!(result, Err(Error::Protected)), "{result:?}");
                assert_eq!(fs::read(&entry).unwrap(), b"a longer user body");
                assert_eq!(
                    (usage.protected_entries, usage.stop()),
                    (1, Stop::ProtectedBody)
                );
            }
        }
        assert_eq!(
            fs::read(fixture.tree.0.join(TARGET)).unwrap(),
            b"later api",
            "{taker:?}"
        );
        drop(operation);
        fixture.close().await;
    }
}
