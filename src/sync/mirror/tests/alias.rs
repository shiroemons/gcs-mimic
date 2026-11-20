//! Objects whose names the root's file system takes for one name, in letter case or Unicode
//! normalization: the tree holds one file for both, which both findings name and which stays
//! frozen until an API change leaves one object. Each test runs on the platform's own spelling
//! answer and on the directory listing a Linux casefolded directory answers from, and only where
//! the test directory folds the names.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::baseline::exists;
use super::install_faults::Started;
use super::*;
use crate::storage::sync_state::{
    PageLimit,
    findings::alias::FrozenFile,
    observation::{FindingCategory, ListedStop, ObservationCursor, PathStop},
};
use crate::sync::control::WaitReason;

/// The pairs of names a folding file system takes for one: by letter case, and by Unicode
/// normalization (NFC and NFD of `é`).
const PAIRS: [(&str, &str); 2] = [("Case", "case"), ("\u{e9}", "e\u{301}")];

/// A third name that a folding file system takes for both of [`PAIRS`]' first pair.
const THIRD: &str = "CASE";

/// How a lookup learns the spelling of an entry it opened.
#[derive(Clone, Copy, Debug)]
enum Spelling {
    /// As the platform reports it; on macOS, the name of the open file.
    Native,
    /// From a listing of the directory, as a Linux casefolded directory answers.
    Listed,
}

/// A bucket whose tree holds `first`'s file, exported from the API, and an API object `second`
/// whose name the directory takes for `first`'s, replayed after it, with any `extra` objects
/// of names it takes for both.
struct Pair {
    _data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    bucket: BucketName,
    root: RootLease,
    first: &'static str,
    second: &'static str,
    extra: &'static [&'static str],
    cursor: JournalFence,
    _unchecked: Option<fsutil::UncheckedSpelling>,
}

impl Pair {
    /// `None` where the test directory keeps the two names apart.
    async fn frozen(spelling: Spelling, names: (&'static str, &'static str)) -> Option<Self> {
        Self::frozen_with(spelling, names, &[]).await
    }

    /// Like [`Self::frozen`], with `extra` objects published after `second`, each with its
    /// name as its bytes. `None` where the test directory keeps any of the names apart.
    async fn frozen_with(
        spelling: Spelling,
        (first, second): (&'static str, &'static str),
        extra: &'static [&'static str],
    ) -> Option<Self> {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        catalog.enable_change_journal().await.unwrap();
        let directory = tree.0.join("test-bucket");
        fs::create_dir(&directory).unwrap();
        if !std::iter::once(second)
            .chain(extra.iter().copied())
            .all(|name| folds(&directory, first, name))
        {
            drop(root);
            catalog.close().await.unwrap();
            return None;
        }
        let _unchecked = match spelling {
            Spelling::Native => None,
            Spelling::Listed => Some(fsutil::UncheckedSpelling::of(&directory)),
        };
        publish_named(&catalog, &bucket, first, b"first", Origin::Api).await;
        assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
        let cursor = catalog.current_fence().await.unwrap();
        publish_named(&catalog, &bucket, second, b"second", Origin::Api).await;
        for name in extra {
            publish_named(&catalog, &bucket, name, name.as_bytes(), Origin::Api).await;
        }
        let mut pair = Self {
            _data,
            tree,
            catalog,
            bucket,
            root,
            first,
            second,
            extra,
            cursor,
            _unchecked,
        };
        let result = pair.wait().await;
        assert_eq!(
            (
                result.reason,
                result.findings.map(|counted| counted.unmapped)
            ),
            (
                WaitReason::Ready,
                Some(2 + u64::try_from(extra.len()).unwrap())
            ),
            "{spelling:?} {first:?}"
        );
        Some(pair)
    }

    /// Runs a wait over what changed since the last one.
    async fn wait(&mut self) -> crate::sync::control::WaitResult {
        let result = wait_from(&self.catalog, &self.root, self.cursor).await;
        self.cursor = self.catalog.current_fence().await.unwrap();
        result
    }

    /// Like [`Self::wait`], returning the error of a pass that failed.
    async fn try_wait(&mut self) -> Result<crate::sync::control::WaitResult, SyncError> {
        let result = try_wait_from(&self.catalog, &self.root, self.cursor).await;
        self.cursor = self.catalog.current_fence().await.unwrap();
        result
    }

    /// Stops the root and closes the catalog as a stopping server does, then starts the mirror
    /// on them again the way the server starts it, with the same spelling answer.
    async fn restart(self) -> Restarted {
        drop(self.root);
        self.catalog.close().await.unwrap();
        let started = Started::open(&self._data.0, &self.tree.0).await;
        Restarted {
            started,
            directory: self.tree.0.join("test-bucket"),
            bucket: self.bucket,
            _dirs: [self._data, self.tree],
            _unchecked: self._unchecked,
        }
    }

    /// Replays the API changes since the last wait or replay, as the running mirror does
    /// between ingestions, without looking at the tree's own changes.
    async fn replay(&mut self) {
        let writeback = self.root.default_writeback().await.unwrap();
        let (_stop, stopping) = tokio::sync::watch::channel(false);
        let mut skipped = SkippedExports::default();
        let fence = self.catalog.current_fence().await.unwrap();
        while self.cursor != fence {
            let ChangeRead::Page(page) = self
                .catalog
                .read_changes(self.cursor, Some(fence), 256)
                .await
                .unwrap()
            else {
                panic!("the journal kept every change since the cursor");
            };
            let next = page.next;
            replay_page(
                &self.catalog,
                &self.root,
                &writeback,
                page,
                &stopping,
                &mut skipped,
            )
            .await
            .unwrap();
            self.cursor = next;
        }
    }

    fn directory(&self) -> std::path::PathBuf {
        self.tree.0.join("test-bucket")
    }

    /// The names the bucket's directory lists, sorted.
    fn listed(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.directory())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// The mapping findings and path stops of the root as (object, reason, tree path), sorted.
    async fn findings(&self) -> Vec<(String, String, Option<String>)> {
        let observation = self.root.observation().await.unwrap();
        let mut items: Vec<_> = self
            .root
            .observation_page(
                observation.revision,
                ObservationCursor::START,
                PageLimit::LARGEST,
            )
            .await
            .unwrap()
            .into_iter()
            .filter(|item| {
                matches!(
                    item.category,
                    FindingCategory::Mapping | FindingCategory::PathStop
                )
            })
            .map(|item| {
                (
                    item.object.unwrap(),
                    item.reason.as_str().to_owned(),
                    item.tree_path,
                )
            })
            .collect();
        items.sort();
        items
    }

    /// The alias findings of every object, all naming the file of `first`.
    fn aliases(&self) -> Vec<(String, String, Option<String>)> {
        self.aliases_of(&[])
    }

    /// The alias findings of the objects other than `deleted`, all naming the file of `first`.
    fn aliases_of(&self, deleted: &[&str]) -> Vec<(String, String, Option<String>)> {
        let path = Some(format!("test-bucket/{}", self.first));
        let mut aliases: Vec<_> = [self.first, self.second]
            .iter()
            .chain(self.extra)
            .filter(|name| !deleted.contains(name))
            .map(|name| ((*name).to_owned(), "alias".to_owned(), path.clone()))
            .collect();
        aliases.sort();
        aliases
    }

    /// The path stop of an edit of the frozen file, which names the file the edit is in.
    fn edited(&self) -> (String, String, Option<String>) {
        (
            self.first.to_owned(),
            "AliasEdited".to_owned(),
            Some(format!("test-bucket/{}", self.first)),
        )
    }

    async fn body(&self, name: &str) -> Vec<u8> {
        let (_, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&self.bucket, &ObjectName::new(name).unwrap(), None)
            .await
            .unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut reader.into_file().unwrap(), &mut bytes).unwrap();
        bytes
    }

    async fn delete(&self, name: &str) {
        let admin = self.catalog.admin(Origin::Api);
        let name = ObjectName::new(name).unwrap();
        let record = admin.get_object(&self.bucket, &name, None).await.unwrap();
        admin
            .delete_object(
                &self.bucket,
                &name,
                GenerationCondition::Is(Generation::new(record.generation).unwrap()),
            )
            .await
            .unwrap();
    }

    async fn close(self) {
        drop(self.root);
        self.catalog.close().await.unwrap();
    }
}

/// A [`Pair`]'s mirror started again on its catalog and tree.
struct Restarted {
    started: Started,
    directory: std::path::PathBuf,
    bucket: BucketName,
    _dirs: [TestDir; 2],
    _unchecked: Option<fsutil::UncheckedSpelling>,
}

impl Restarted {
    async fn close(self) {
        self.started.close().await;
    }
}

/// Whether `directory` opens `first`'s file under the name `second`.
fn folds(directory: &std::path::Path, first: &str, second: &str) -> bool {
    let probe = directory.join(first);
    fs::write(&probe, b"").unwrap();
    let folds = directory.join(second).exists();
    fs::remove_file(probe).unwrap();
    folds
}

/// Every combination of spelling answer and pair of names, as `(spelling, pair)`.
fn cases() -> impl Iterator<Item = (Spelling, (&'static str, &'static str))> {
    [Spelling::Native, Spelling::Listed]
        .into_iter()
        .flat_map(|spelling| PAIRS.into_iter().map(move |pair| (spelling, pair)))
}

/// An object exported after another whose name the directory takes for its own reports both,
/// each with the spelling of the one file, and that file keeps its bytes: neither object is
/// written over it, and the second gets no file of its own. Since both stay reachable through
/// the API, the wait succeeds, counts them and names the frozen file.
#[tokio::test]
async fn both_objects_of_an_alias_are_reported_with_the_spelling_of_their_file() {
    for (spelling, names) in cases() {
        let Some(mut pair) = Pair::frozen(spelling, names).await else {
            continue;
        };
        let context = format!("{spelling:?} {names:?}");
        let found = pair.wait().await.findings.unwrap();
        assert_eq!(pair.findings().await, pair.aliases(), "{context}");
        assert_eq!(
            (found.unmapped, found.frozen, found.stops),
            (
                2,
                crate::sync::control::Listed::of(
                    vec![FrozenFile {
                        path: format!("test-bucket/{}", pair.first),
                        objects: 2
                    }],
                    1
                ),
                crate::sync::control::Listed::default()
            ),
            "{context}"
        );
        assert_eq!(pair.findings().await, pair.aliases(), "{context}");
        assert_eq!(pair.listed(), [pair.first], "{context}");
        assert_eq!(
            fs::read(pair.directory().join(pair.first)).unwrap(),
            b"first",
            "{context}"
        );
        assert_eq!(pair.body(pair.second).await, b"second", "{context}");
        pair.close().await;
    }
}

/// Deleting either object through the API ends the alias: the replay of that deletion observes
/// the object left again and releases it, with no ingestion and no restart. When the object
/// left is the one the file stood for, the file stays as it is; when it is the other, the file
/// goes with its deleted object and the object left gets its own.
#[tokio::test]
async fn deleting_either_object_through_the_api_releases_the_other() {
    for (spelling, names) in cases() {
        for deleted_first in [false, true] {
            let Some(mut pair) = Pair::frozen(spelling, names).await else {
                continue;
            };
            let context = format!("{spelling:?} {names:?} deleted first: {deleted_first}");
            let (deleted, left, body) = if deleted_first {
                (pair.first, pair.second, b"second".as_slice())
            } else {
                (pair.second, pair.first, b"first".as_slice())
            };
            pair.delete(deleted).await;
            pair.replay().await;
            assert_eq!(pair.findings().await, [], "{context}");
            assert_eq!(pair.listed(), [left], "{context}");
            let result = pair.wait().await;
            assert_eq!(
                (
                    result.reason,
                    result.findings.map(|counted| counted.unmapped)
                ),
                (WaitReason::Ready, Some(0)),
                "{context}"
            );
            assert_eq!(pair.findings().await, [], "{context}");
            assert_eq!(pair.listed(), [left], "{context}");
            assert_eq!(
                fs::read(pair.directory().join(left)).unwrap(),
                body,
                "{context}"
            );
            assert!(
                !exists(&pair.catalog, &pair.bucket, deleted).await,
                "{context}"
            );
            pair.close().await;
        }
    }
}

/// An edit of the frozen file may be meant for either object, so ingestion leaves it out and
/// stops the path as a tree-side condition, which ends the wait for an operator. Once the API
/// deletes either object, the edit is taken in for the one left: for the object the file
/// stood for in place, and for the other object, whose name the file is not spelled as, before
/// the file goes with the deleted object and the one left gets its own.
#[tokio::test]
async fn an_edit_of_the_frozen_file_stops_the_wait_until_the_alias_ends() {
    for (spelling, names) in cases() {
        for deleted_first in [false, true] {
            let Some(mut pair) = Pair::frozen(spelling, names).await else {
                continue;
            };
            let context = format!("{spelling:?} {names:?} deleted first: {deleted_first}");
            fs::write(pair.directory().join(pair.first), b"edited file").unwrap();
            for _ in 0..2 {
                let result = pair.wait().await;
                assert_eq!(result.reason, WaitReason::FindingsPending, "{context}");
                assert_eq!(
                    result.findings.unwrap().stops,
                    crate::sync::control::Listed::of(
                        vec![ListedStop {
                            path: format!("test-bucket/{}", pair.first),
                            reason: PathStop::AliasEdited
                        }],
                        1
                    ),
                    "{context}"
                );
                let mut expected = pair.aliases();
                expected.push(pair.edited());
                expected.sort();
                assert_eq!(pair.findings().await, expected, "{context}");
                assert_eq!(pair.body(pair.first).await, b"first", "{context}");
                assert_eq!(pair.body(pair.second).await, b"second", "{context}");
            }
            let (deleted, left) = if deleted_first {
                (pair.first, pair.second)
            } else {
                (pair.second, pair.first)
            };
            pair.delete(deleted).await;
            let result = pair.wait().await;
            assert_eq!(result.reason, WaitReason::Ready, "{context}");
            assert_eq!(pair.findings().await, [], "{context}");
            assert_eq!(pair.body(left).await, b"edited file", "{context}");
            assert_eq!(pair.listed(), [left], "{context}");
            assert_eq!(
                fs::read(pair.directory().join(left)).unwrap(),
                b"edited file",
                "{context}"
            );
            assert!(
                !exists(&pair.catalog, &pair.bucket, deleted).await,
                "{context}"
            );
            pair.close().await;
        }
    }
}

/// With three objects for one file, deleting the one the file stood for leaves it frozen for
/// the other two, and an edit made before keeps the path stopped. Once one more goes, the edit
/// is taken in for the last, which gets its own file.
#[tokio::test]
async fn an_edit_of_a_file_three_objects_share_waits_for_the_last_of_them() {
    for spelling in [Spelling::Native, Spelling::Listed] {
        let Some(mut pair) = Pair::frozen_with(spelling, PAIRS[0], &[THIRD]).await else {
            continue;
        };
        let context = format!("{spelling:?}");
        fs::write(pair.directory().join(pair.first), b"edited file").unwrap();
        assert_eq!(
            pair.wait().await.reason,
            WaitReason::FindingsPending,
            "{context}"
        );
        pair.delete(pair.first).await;
        let result = pair.wait().await;
        assert_eq!(result.reason, WaitReason::FindingsPending, "{context}");
        let mut expected = pair.aliases_of(&[pair.first]);
        expected.push(pair.edited());
        expected.sort();
        assert_eq!(pair.findings().await, expected, "{context}");
        assert_eq!(pair.listed(), [pair.first], "{context}");
        assert_eq!(pair.body(pair.second).await, b"second", "{context}");
        assert_eq!(pair.body(THIRD).await, THIRD.as_bytes(), "{context}");
        pair.delete(pair.second).await;
        let result = pair.wait().await;
        assert_eq!(result.reason, WaitReason::Ready, "{context}");
        assert_eq!(pair.findings().await, [], "{context}");
        assert_eq!(pair.body(THIRD).await, b"edited file", "{context}");
        assert_eq!(pair.listed(), [THIRD], "{context}");
        pair.close().await;
    }
}

/// The end of an alias settles the file with the export that writes it back, in one
/// transaction. When that export fails after the frozen file was deleted and the other object
/// went through the API, the object left keeps its finding, so neither the failed pass nor a
/// restart takes the missing file for its deletion, and the restart writes the file back. A
/// failure before the export recorded any operation keeps the finding without failing the
/// pass; one that left an operation for recovery fails it, as any export's would.
#[tokio::test]
async fn an_export_failing_at_the_end_of_an_alias_deletes_nothing_at_restart() {
    for (spelling, names) in cases() {
        for phase in ["prepare", "copy"] {
            let Some(mut pair) = Pair::frozen(spelling, names).await else {
                continue;
            };
            let context = format!("{spelling:?} {names:?} {phase}");
            fs::remove_file(pair.directory().join(pair.first)).unwrap();
            assert_eq!(pair.wait().await.reason, WaitReason::Ready, "{context}");
            let attempts = usize::try_from(EXPORT_RETRIES).unwrap() + 1;
            crate::storage::sync_state::writeback::inject_live_export_failure(
                pair.root.status().root_id,
                phase,
                attempts,
            );
            pair.delete(pair.second).await;
            let failed = pair.try_wait().await.map(|result| result.reason);
            assert_eq!(failed.is_ok(), phase == "prepare", "{context}: {failed:?}");
            assert!(pair.listed().is_empty(), "{context}");
            assert_eq!(
                pair.findings().await,
                pair.aliases_of(&[pair.second]),
                "{context}"
            );
            pair.restarts_with_the_file_of_first(&context).await;
        }
    }
}

impl Pair {
    /// Restarts and checks that `first` is left, with its file back, and nothing reported.
    async fn restarts_with_the_file_of_first(self, context: &str) {
        let first = self.first;
        let mut restarted = self.restart().await;
        let result = restarted.started.wait().await.unwrap();
        assert_eq!(
            (
                result.reason,
                result.findings.map(|counted| counted.unmapped)
            ),
            (WaitReason::Ready, Some(0)),
            "{context}"
        );
        assert!(
            exists(&restarted.started.catalog, &restarted.bucket, first).await,
            "{context}"
        );
        assert_eq!(
            fs::read(restarted.directory.join(first)).unwrap(),
            b"first",
            "{context}"
        );
        restarted.close().await;
    }
}

/// Deleting the frozen file deletes neither object, since it stood for both, and writes
/// neither back while both live. Once the API deletes one, the other gets its file back.
#[tokio::test]
async fn deleting_the_frozen_file_deletes_no_object() {
    for (spelling, names) in cases() {
        let Some(mut pair) = Pair::frozen(spelling, names).await else {
            continue;
        };
        let context = format!("{spelling:?} {names:?}");
        fs::remove_file(pair.directory().join(pair.first)).unwrap();
        let result = pair.wait().await;
        assert_eq!(
            (
                result.reason,
                result.findings.map(|counted| counted.unmapped)
            ),
            (WaitReason::Ready, Some(2)),
            "{context}"
        );
        assert!(
            exists(&pair.catalog, &pair.bucket, pair.first).await,
            "{context}"
        );
        assert!(
            exists(&pair.catalog, &pair.bucket, pair.second).await,
            "{context}"
        );
        assert!(pair.listed().is_empty(), "{context}");
        assert_eq!(pair.findings().await, pair.aliases(), "{context}");
        pair.delete(pair.second).await;
        let result = pair.wait().await;
        assert_eq!(result.reason, WaitReason::Ready, "{context}");
        assert_eq!(pair.listed(), [pair.first], "{context}");
        assert_eq!(
            fs::read(pair.directory().join(pair.first)).unwrap(),
            b"first",
            "{context}"
        );
        pair.close().await;
    }
}

/// Renaming a tracked file to another letter case is a rename: ingestion takes the new spelling
/// in as its own object, and the old path, which the directory now lists under that tracked
/// spelling, is the rename's deletion side. It holds back no deletion of a file removed from the
/// same directory at the same time, and neither the pass after the rename nor a restart takes
/// the two names for an alias.
#[tokio::test]
async fn a_case_only_rename_is_a_rename_and_never_an_alias() {
    for spelling in [Spelling::Native, Spelling::Listed] {
        for restart in [false, true] {
            let context = format!("{spelling:?} restart: {restart}");
            let (data, tree, catalog, bucket, root) = live_fixture().await;
            catalog.enable_change_journal().await.unwrap();
            let directory = tree.0.join("test-bucket");
            fs::create_dir(&directory).unwrap();
            if !folds(&directory, "Case", "case") {
                drop(root);
                catalog.close().await.unwrap();
                return;
            }
            let _unchecked = match spelling {
                Spelling::Native => None,
                Spelling::Listed => Some(fsutil::UncheckedSpelling::of(&directory)),
            };
            publish_named(&catalog, &bucket, "Case", b"renamed", Origin::Api).await;
            publish_named(&catalog, &bucket, "other", b"deleted", Origin::Api).await;
            publish_named(&catalog, &bucket, "kept", b"kept", Origin::Api).await;
            assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
            let cursor = catalog.current_fence().await.unwrap();
            fs::rename(directory.join("Case"), directory.join("case")).unwrap();
            fs::remove_file(directory.join("other")).unwrap();
            let result = wait_from(&catalog, &root, cursor).await;
            assert_eq!(result.reason, WaitReason::Ready, "{context}");
            assert_renamed(&catalog, &root, &bucket, &context).await;
            drop(root);
            catalog.close().await.unwrap();
            if restart {
                // The restart exports every object again, which must find no other object's
                // file under `case`.
                let mut started = Started::open(&data.0, &tree.0).await;
                assert_eq!(
                    started.wait().await.unwrap().reason,
                    WaitReason::Ready,
                    "{context}"
                );
                assert_renamed(&started.catalog, &started.root, &bucket, &context).await;
                let mut listed: Vec<_> = fs::read_dir(&directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect();
                listed.sort();
                assert_eq!(listed, ["case", "kept"], "{context}");
                started.close().await;
            }
        }
    }
}

/// Checks that `Case` went as renamed to `case`, which holds its bytes, that `other` went as
/// deleted, and that the root reports nothing.
async fn assert_renamed(catalog: &Catalog, root: &RootLease, bucket: &BucketName, context: &str) {
    assert!(!exists(catalog, bucket, "other").await, "{context}");
    assert!(!exists(catalog, bucket, "Case").await, "{context}");
    assert!(exists(catalog, bucket, "kept").await, "{context}");
    let (_, reader) = catalog
        .admin(Origin::Api)
        .open_object(bucket, &ObjectName::new("case").unwrap(), None)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader.into_file().unwrap(), &mut bytes).unwrap();
    assert_eq!(bytes, b"renamed", "{context}");
    let observation = root.observation().await.unwrap();
    let reported: Vec<_> = root
        .observation_page(
            observation.revision,
            ObservationCursor::START,
            PageLimit::LARGEST,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|item| (item.object, item.reason.as_str().to_owned()))
        .collect();
    assert!(reported.is_empty(), "{context}: {reported:?}");
}
