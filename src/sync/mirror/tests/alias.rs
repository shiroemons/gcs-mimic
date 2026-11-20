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
use super::*;
use crate::storage::sync_state::{
    PageLimit,
    observation::{FindingCategory, ObservationCursor},
};
use crate::sync::control::WaitReason;

/// The pairs of names a folding file system takes for one: by letter case, and by Unicode
/// normalization (NFC and NFD of `é`).
const PAIRS: [(&str, &str); 2] = [("Case", "case"), ("\u{e9}", "e\u{301}")];

/// How a lookup learns the spelling of an entry it opened.
#[derive(Clone, Copy, Debug)]
enum Spelling {
    /// As the platform reports it; on macOS, the name of the open file.
    Native,
    /// From a listing of the directory, as a Linux casefolded directory answers.
    Listed,
}

/// A bucket whose tree holds `first`'s file, exported from the API, and an API object `second`
/// whose name the directory takes for `first`'s, replayed after it.
struct Pair {
    _data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    bucket: BucketName,
    root: RootLease,
    first: &'static str,
    second: &'static str,
    cursor: JournalFence,
    _unchecked: Option<fsutil::UncheckedSpelling>,
}

impl Pair {
    /// `None` where the test directory keeps the two names apart.
    async fn frozen(
        spelling: Spelling,
        (first, second): (&'static str, &'static str),
    ) -> Option<Self> {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        catalog.enable_change_journal().await.unwrap();
        let directory = tree.0.join("test-bucket");
        fs::create_dir(&directory).unwrap();
        if !folds(&directory, first, second) {
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
        let mut pair = Self {
            _data,
            tree,
            catalog,
            bucket,
            root,
            first,
            second,
            cursor,
            _unchecked,
        };
        let result = pair.wait().await;
        assert_eq!(
            (
                result.reason,
                result.findings.map(|counted| counted.unmapped)
            ),
            (WaitReason::Ready, Some(2)),
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

    /// The two alias findings, both naming the file of `first`.
    fn aliases(&self) -> Vec<(String, String, Option<String>)> {
        let path = Some(format!("test-bucket/{}", self.first));
        let mut aliases = vec![
            (self.first.to_owned(), "alias".to_owned(), path.clone()),
            (self.second.to_owned(), "alias".to_owned(), path),
        ];
        aliases.sort();
        aliases
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
/// the API, the wait succeeds and counts them.
#[tokio::test]
async fn both_objects_of_an_alias_are_reported_with_the_spelling_of_their_file() {
    for (spelling, names) in cases() {
        let Some(pair) = Pair::frozen(spelling, names).await else {
            continue;
        };
        let context = format!("{spelling:?} {names:?}");
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
            assert!(pair.findings().await.is_empty(), "{context}");
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
            assert!(pair.findings().await.is_empty(), "{context}");
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
/// deletes the other object, the edit is taken in for the object the file stood for.
#[tokio::test]
async fn an_edit_of_the_frozen_file_stops_the_wait_until_the_alias_ends() {
    for (spelling, names) in cases() {
        let Some(mut pair) = Pair::frozen(spelling, names).await else {
            continue;
        };
        let context = format!("{spelling:?} {names:?}");
        fs::write(pair.directory().join(pair.first), b"edited file").unwrap();
        for _ in 0..2 {
            let result = pair.wait().await;
            assert_eq!(result.reason, WaitReason::FindingsPending, "{context}");
            let mut expected = pair.aliases();
            expected.push((pair.first.to_owned(), "AliasEdited".to_owned(), None));
            expected.sort();
            assert_eq!(pair.findings().await, expected, "{context}");
            assert_eq!(pair.body(pair.first).await, b"first", "{context}");
            assert_eq!(pair.body(pair.second).await, b"second", "{context}");
        }
        pair.delete(pair.second).await;
        let result = pair.wait().await;
        assert_eq!(result.reason, WaitReason::Ready, "{context}");
        assert!(pair.findings().await.is_empty(), "{context}");
        assert_eq!(pair.body(pair.first).await, b"edited file", "{context}");
        pair.close().await;
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

/// Renaming a tracked file to another letter case leaves its old path an alias, which tells
/// nothing about whether the file went, so it neither deletes its object nor holds back the
/// deletion of a file removed from the same directory at the same time.
#[tokio::test]
async fn a_case_only_rename_holds_back_no_other_deletion() {
    for spelling in [Spelling::Native, Spelling::Listed] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
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
        assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
        let cursor = catalog.current_fence().await.unwrap();
        fs::rename(directory.join("Case"), directory.join("case")).unwrap();
        fs::remove_file(directory.join("other")).unwrap();
        let result = wait_from(&catalog, &root, cursor).await;
        assert_ne!(result.reason, WaitReason::DeletionApproval, "{spelling:?}");
        assert!(!exists(&catalog, &bucket, "other").await, "{spelling:?}");
        assert!(exists(&catalog, &bucket, "Case").await, "{spelling:?}");
        drop(root);
        catalog.close().await.unwrap();
    }
}
