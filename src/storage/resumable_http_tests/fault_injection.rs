//! Injected storage failures and crash snapshots on every durable write path.
//!
//! Each case arms one fault at a durable step boundary, drives the write through the HTTP
//! router and checks what the client sees and what remains on disk: a failed write never
//! publishes, the previous object stays readable, and staging and blob files do not leak.
//! Crash cases snapshot the data directory at a boundary and reopen the snapshot, which is
//! the state a process abort at that boundary leaves for startup recovery. Power-loss cases
//! keep only what the write path synced, so they also fail when an fsync goes missing.

use super::*;
use crate::object::ObjectConditions;
use crate::storage::catalog::fault::{Fault, Step};
use std::{io::ErrorKind, path::Path};

const CORRUPT: &str =
    "Object storage is unavailable: stored data is corrupt; restore a valid data directory";

fn io_message(kind: ErrorKind) -> String {
    if kind == ErrorKind::StorageFull {
        return concat!(
            "Storage capacity exhausted; for backend=memory release objects/readers or restart ",
            "with larger memory_max_bytes and memory_catalog_max_bytes (restart loses RAM data), ",
            "and for backend=disk free disk space; retry with generation preconditions after capacity is available"
        ).into();
    }
    format!(
        "Object storage is unavailable: storage I/O error ({kind}); check data directory permissions and available disk space"
    )
}

/// The client-visible outcome a fault must produce.
enum Expect {
    /// 503 backendError with exactly this message.
    Message(String),
    /// 503 backendError; the message is the generic backend text.
    Backend,
}

fn expect_for(fault: &Fault) -> Expect {
    match fault {
        Fault::Io(kind) => Expect::Message(io_message(*kind)),
        Fault::Corrupt => Expect::Message(CORRUPT.into()),
        Fault::Busy | Fault::Snapshot(_) => Expect::Backend,
        #[cfg(unix)]
        Fault::PowerLoss(_) => Expect::Backend,
    }
}

fn assert_backend_failure(reply: &Reply, expect: &Expect, case: &str) {
    reply.error(503, "backendError");
    let message = reply.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    match expect {
        Expect::Message(expected) => assert_eq!(&message, expected, "{case}"),
        Expect::Backend => assert!(
            message.starts_with("Object storage is unavailable: "),
            "{case}: {message}"
        ),
    }
}

fn staged(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir.join("staging"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn blob_count(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("blobs"))
        .unwrap()
        .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

fn count(dir: &Path, sql: &str) -> usize {
    let connection = rusqlite::Connection::open(dir.join("catalog.sqlite3")).unwrap();
    let count: i64 = connection.query_row(sql, [], |row| row.get(0)).unwrap();
    usize::try_from(count).unwrap()
}

/// Staged bodies that no active upload session or derived transfer owns.
fn unowned_staging(dir: &Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(dir.join("catalog.sqlite3")).unwrap();
    staged(dir)
        .into_iter()
        .filter(|name| {
            !connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM resumable_sessions WHERE blob_id = ?1 AND state = 'ACTIVE') OR EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1) OR EXISTS(SELECT 1 FROM derived_inputs WHERE blob_id = ?1)",
                    [name],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap()
        })
        .collect()
}

/// No unowned staged body and no blob without a live row or a journaled deletion.
fn assert_no_leaks(dir: &Path, case: &str) {
    assert_eq!(
        unowned_staging(dir),
        Vec::<String>::new(),
        "{case}: staging leaked"
    );
    let referenced = count(dir, "SELECT count(*) FROM objects")
        + count(dir, "SELECT count(*) FROM pending_blob_deletions");
    assert_eq!(blob_count(dir), referenced, "{case}: blob leaked");
}

impl Harness {
    async fn media(&self, name: &str, body: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
            "",
            body,
        )
        .await
    }

    async fn multipart(&self, name: &str, body: &[u8]) -> Reply {
        let mut payload = format!(
            "--b\r\nContent-Type: application/json\r\n\r\n{{\"name\":\"{name}\"}}\r\n--b\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        payload.extend_from_slice(body);
        payload.extend_from_slice(b"\r\n--b--\r\n");
        self.request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=multipart",
            "Content-Type: multipart/related; boundary=b\r\n",
            &payload,
        )
        .await
    }

    async fn media_bytes(&self, name: &str) -> Reply {
        self.request(
            "GET",
            &format!("/storage/v1/b/bucket/o/{name}?alt=media"),
            "",
            b"",
        )
        .await
    }

    async fn resource(&self, name: &str) -> Reply {
        self.request("GET", &format!("/storage/v1/b/bucket/o/{name}"), "", b"")
            .await
    }

    fn inject(&self, step: Step, skip: usize, fault: Fault) {
        self.state.catalog.inject(step, skip, fault);
    }

    fn assert_fired(&self, case: &str) {
        assert_eq!(
            self.state.catalog.pending_faults(),
            0,
            "{case}: the write never reached the armed step"
        );
    }
}

/// Faults at every step of a single-request upload, from the first body write to the
/// publication commit. Each one fails the request, keeps the previous generation and
/// leaves no staged or orphaned file.
fn upload_failures() -> Vec<(&'static str, Step, usize, Fault)> {
    vec![
        (
            "first body write",
            Step::BodyWrite,
            0,
            Fault::Io(ErrorKind::Other),
        ),
        (
            "mid-body write, disk full",
            Step::BodyWrite,
            2,
            Fault::Io(ErrorKind::StorageFull),
        ),
        (
            "final tail write, disk full",
            Step::BodyFlush,
            0,
            Fault::Io(ErrorKind::StorageFull),
        ),
        ("file fsync", Step::BodySync, 0, Fault::Io(ErrorKind::Other)),
        (
            "rename",
            Step::BeforePublish,
            0,
            Fault::Io(ErrorKind::PermissionDenied),
        ),
        (
            "after rename",
            Step::AfterPublish,
            0,
            Fault::Io(ErrorKind::Other),
        ),
        ("commit, busy", Step::BeforeCommit, 0, Fault::Busy),
        ("commit, corrupt", Step::BeforeCommit, 0, Fault::Corrupt),
        (
            "commit, disk full",
            Step::BeforeCommit,
            0,
            Fault::Io(ErrorKind::StorageFull),
        ),
    ]
}

#[tokio::test]
async fn single_request_upload_failures_never_publish_and_keep_the_previous_object() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let original = h.media("target", b"old").await;
    assert_eq!(original.status, 200);
    let original = original.json();
    let body = vec![b'n'; 256 * 1024];
    for multipart in [false, true] {
        for (case, step, skip, fault) in upload_failures() {
            let case = format!("{case} (multipart: {multipart})");
            let expect = expect_for(&fault);
            h.inject(step, skip, fault);
            let reply = if multipart {
                h.multipart("target", &body).await
            } else {
                h.media("target", &body).await
            };
            assert_backend_failure(&reply, &expect, &case);
            h.assert_fired(&case);
            assert_eq!(h.resource("target").await.json(), original, "{case}");
            assert_eq!(h.media_bytes("target").await.body, b"old", "{case}");
            assert_no_leaks(&h.dir.0, &case);
        }
    }
    // The same request succeeds once the fault is gone.
    let replaced = h.media("target", &body).await;
    assert_eq!(replaced.status, 200);
    assert_eq!(h.media_bytes("target").await.body, body);
    assert_no_leaks(&h.dir.0, "after the fault is gone");
    h.stop().await;
}

/// A commit that succeeded but reported an error is ambiguous. Cleanup must probe the
/// committed reference instead of deleting the live blob as though the write failed.
#[tokio::test]
async fn ambiguous_upload_commit_error_keeps_the_committed_object_readable() {
    let h = Harness::start(1024 * 1024, false, None).await;
    assert_eq!(h.media("target", b"old").await.status, 200);
    h.inject(Step::AfterCommit, 0, Fault::Io(ErrorKind::Other));
    let reply = h.media("target", b"committed").await;
    assert_backend_failure(
        &reply,
        &Expect::Message(io_message(ErrorKind::Other)),
        "after commit",
    );
    h.assert_fired("after commit");
    assert_eq!(h.media_bytes("target").await.body, b"committed");
    assert_eq!(staged(&h.dir.0), Vec::<String>::new());
    assert_eq!(count(&h.dir.0, "SELECT count(*) FROM objects"), 1);
    h.stop().await;
}

async fn read_all(catalog: &Catalog, name: &str) -> Option<Vec<u8>> {
    let (_, mut reader) = match catalog
        .open_object("bucket", name, None, ObjectConditions::default())
        .await
    {
        Ok(opened) => opened,
        Err(crate::object::ObjectError::ObjectNotFound) => return None,
        Err(error) => panic!("{name}: {error}"),
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}

/// Reopens a crash snapshot, which runs startup recovery, and checks that it holds
/// exactly `expected` for `name` and no staged or unreferenced file.
async fn assert_recovers(snapshot: &Path, name: &str, expected: Option<&[u8]>, case: &str) {
    assert_recovers_objects(snapshot, &[(name, expected)], case).await;
}

/// [`assert_recovers`] for several objects of one snapshot.
async fn assert_recovers_objects(snapshot: &Path, objects: &[(&str, Option<&[u8]>)], case: &str) {
    let catalog = Catalog::open(snapshot)
        .await
        .unwrap_or_else(|error| panic!("{case}: recovery failed: {error}"));
    for (name, expected) in objects {
        assert_eq!(
            read_all(&catalog, name).await.as_deref(),
            *expected,
            "{case}: {name}"
        );
    }
    catalog.close().await.unwrap();
    assert_no_leaks(snapshot, case);
}

#[tokio::test]
async fn upload_crash_at_each_durable_step_recovers_the_old_or_new_object() {
    let crashes = TestDir::new();
    let h = Harness::start(1024 * 1024, false, None).await;
    assert_eq!(h.media("target", b"old").await.status, 200);
    let body = vec![b'n'; 256 * 1024];
    for (index, (case, step, new)) in [
        ("staged body written, not synced", Step::BodySync, false),
        ("staged body durable", Step::BeforePublish, false),
        ("blob renamed, no catalog row", Step::AfterPublish, false),
        ("publication transaction open", Step::BeforeCommit, false),
        ("committed, old blob not reclaimed", Step::AfterCommit, true),
    ]
    .into_iter()
    .enumerate()
    {
        let snapshot = crashes.0.join(index.to_string());
        h.inject(step, 0, Fault::Snapshot(snapshot.clone()));
        assert_eq!(h.media("target", &body).await.status, 200, "{case}");
        h.assert_fired(case);
        let expected: &[u8] = if new { &body } else { b"old" };
        assert_recovers(&snapshot, "target", Some(expected), case).await;
        // The next iteration replaces the object again from the original bytes.
        assert_eq!(h.media("target", b"old").await.status, 200);
    }
    h.stop().await;
}

/// A process-abort snapshot keeps unsynced bytes and renames, so the crash test above passes
/// without the blob fsync or the shard fsync. Power loss keeps only synced state: without
/// the blob's `sync_all` the committed object comes back truncated, and without the shard
/// sync after the rename its blob is back in staging, so the committed case fails.
#[cfg(unix)]
#[tokio::test]
async fn upload_power_loss_at_each_durable_step_keeps_the_old_or_new_object() {
    let crashes = TestDir::new();
    let h = Harness::start(1024 * 1024, false, None).await;
    assert_eq!(h.media("target", b"old").await.status, 200);
    let body = vec![b'n'; 256 * 1024];
    for (index, (case, step, new)) in [
        ("staged body written, not synced", Step::BodySync, false),
        ("staged body durable", Step::BeforePublish, false),
        ("blob renamed, no catalog row", Step::AfterPublish, false),
        ("publication transaction open", Step::BeforeCommit, false),
        ("committed, old blob not reclaimed", Step::AfterCommit, true),
    ]
    .into_iter()
    .enumerate()
    {
        let snapshot = crashes.0.join(index.to_string());
        h.inject(step, 0, Fault::PowerLoss(snapshot.clone()));
        assert_eq!(h.media("target", &body).await.status, 200, "{case}");
        h.assert_fired(case);
        let expected: &[u8] = if new { &body } else { b"old" };
        assert_recovers(&snapshot, "target", Some(expected), case).await;
        assert_eq!(h.media("target", b"old").await.status, 200);
    }
    h.stop().await;
}

/// Uploads a first chunk of a resumable session and returns its path.
async fn resumable_prefix(h: &Harness, name: &str, chunk: &[u8]) -> String {
    let init = h.initiate(&format!("name={name}"), "", b"").await;
    assert_eq!(init.status, 200);
    let path = h.path(&init);
    let range = format!("Content-Range: bytes 0-{}/*\r\n", chunk.len() - 1);
    assert_eq!(h.request("PUT", &path, &range, chunk).await.status, 308);
    path
}

async fn assert_prefix(h: &Harness, path: &str, length: usize, case: &str) {
    let status = h
        .request("PUT", path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(status.status, 308, "{case}");
    assert_eq!(
        status.headers["range"],
        format!("bytes=0-{}", length - 1),
        "{case}"
    );
}

#[tokio::test]
async fn resumable_chunk_and_finalization_failures_keep_the_acknowledged_prefix() {
    let h = Harness::start(4 * 1024 * 1024, false, None).await;
    let chunk = vec![b'a'; 256 * 1024];
    let path = resumable_prefix(&h, "resumable", &chunk).await;
    let id = path.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    let stage = h.dir.0.join("staging").join(&session.blob_id);
    let next = "Content-Range: bytes 262144-524287/*\r\n";
    for (case, step, skip, fault) in [
        (
            "first chunk write, disk full",
            Step::BodyWrite,
            0,
            Fault::Io(ErrorKind::StorageFull),
        ),
        (
            "mid-chunk write",
            Step::BodyWrite,
            2,
            Fault::Io(ErrorKind::Other),
        ),
        (
            "final tail write, disk full",
            Step::BodyFlush,
            0,
            Fault::Io(ErrorKind::StorageFull),
        ),
        (
            "chunk fsync",
            Step::BodySync,
            0,
            Fault::Io(ErrorKind::Other),
        ),
    ] {
        let expect = expect_for(&fault);
        h.inject(step, skip, fault);
        let reply = h.request("PUT", &path, next, &vec![b'x'; 256 * 1024]).await;
        assert_backend_failure(&reply, &expect, case);
        h.assert_fired(case);
        assert_prefix(&h, &path, chunk.len(), case).await;
        assert_eq!(
            std::fs::metadata(&stage).unwrap().len(),
            256 * 1024,
            "{case}"
        );
    }
    let last = "Content-Range: bytes 262144-262147/262148\r\n";
    for (case, step, fault) in [
        (
            "final chunk write, disk full",
            Step::BodyWrite,
            Fault::Io(ErrorKind::StorageFull),
        ),
        (
            "rename",
            Step::BeforePublish,
            Fault::Io(ErrorKind::PermissionDenied),
        ),
        (
            "after rename",
            Step::AfterPublish,
            Fault::Io(ErrorKind::Other),
        ),
        ("commit, busy", Step::BeforeCommit, Fault::Busy),
        ("commit, corrupt", Step::BeforeCommit, Fault::Corrupt),
        (
            "commit, disk full",
            Step::BeforeCommit,
            Fault::Io(ErrorKind::StorageFull),
        ),
    ] {
        let expect = expect_for(&fault);
        h.inject(step, 0, fault);
        let reply = h.request("PUT", &path, last, b"tail").await;
        assert_backend_failure(&reply, &expect, case);
        h.assert_fired(case);
        assert_eq!(h.resource("resumable").await.status, 404, "{case}");
        // The final bytes were checkpointed before publication failed, or discarded
        // with the failed write; either way the acknowledged range stays readable.
        let status = h
            .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
            .await;
        assert_eq!(status.status, 308, "{case}");
        assert!(
            stage.is_file(),
            "{case}: staging must be restored for a retry"
        );
        assert_eq!(count(&h.dir.0, "SELECT count(*) FROM objects"), 0, "{case}");
        assert_eq!(blob_count(&h.dir.0), 0, "{case}: blob leaked");
    }
    // An error reported after the commit resolves to the stored completion.
    h.inject(Step::AfterCommit, 0, Fault::Io(ErrorKind::Other));
    let completed = h.request("PUT", &path, last, b"tail").await;
    h.assert_fired("after commit");
    assert_eq!(completed.status, 200);
    let mut expected = chunk.clone();
    expected.extend_from_slice(b"tail");
    assert_eq!(h.media_bytes("resumable").await.body, expected);
    assert_eq!(h.resource("resumable").await.json(), completed.json());
    h.stop().await;
}

/// Sends the rest of the object after a 256 KiB prefix as an open-ended chunked final
/// request (`bytes 262144-*/*`), whose length the server learns only at EOF.
async fn put_open_tail(h: &Harness, path: &str, tail: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    let mut request = format!(
        "PUT {path} HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\nContent-Range: bytes 262144-*/*\r\n\r\n{:x}\r\n",
        tail.len()
    )
    .into_bytes();
    request.extend_from_slice(tail);
    request.extend_from_slice(b"\r\n0\r\n\r\n");
    stream.write_all(&request).await.unwrap();
    Harness::read_response(stream).await
}

#[tokio::test]
async fn open_ended_final_chunk_failures_keep_the_acknowledged_prefix() {
    let h = Harness::start(4 * 1024 * 1024, false, None).await;
    let chunk = vec![b'a'; 256 * 1024];
    let path = resumable_prefix(&h, "open", &chunk).await;
    for (case, step, fault) in [
        (
            "open tail write, disk full",
            Step::BodyWrite,
            Fault::Io(ErrorKind::StorageFull),
        ),
        (
            "open tail flush",
            Step::BodyFlush,
            Fault::Io(ErrorKind::Other),
        ),
        (
            "open tail fsync",
            Step::BodySync,
            Fault::Io(ErrorKind::Other),
        ),
        ("open checkpoint, busy", Step::Checkpoint, Fault::Busy),
    ] {
        let expect = expect_for(&fault);
        h.inject(step, 0, fault);
        let reply = put_open_tail(&h, &path, b"tail").await;
        assert_backend_failure(&reply, &expect, case);
        h.assert_fired(case);
        assert_prefix(&h, &path, chunk.len(), case).await;
        assert_eq!(h.resource("open").await.status, 404, "{case}");
    }
    let completed = put_open_tail(&h, &path, b"tail").await;
    assert_eq!(completed.status, 200);
    let mut expected = chunk.clone();
    expected.extend_from_slice(b"tail");
    assert_eq!(h.media_bytes("open").await.body, expected);
    h.stop().await;
}

/// Power loss while an open-ended final chunk is unsynced or not yet checkpointed resumes
/// from the earlier prefix; after the commit the whole object is durable.
#[cfg(unix)]
#[tokio::test]
async fn open_ended_final_chunk_power_loss_keeps_the_old_or_new_object() {
    let crashes = TestDir::new();
    let h = Harness::start(4 * 1024 * 1024, false, None).await;
    assert_eq!(h.media("target", b"old").await.status, 200);
    let chunk = vec![b'a'; 256 * 1024];
    let mut body = chunk.clone();
    body.extend_from_slice(b"tail");
    for (index, (case, step, committed)) in [
        ("open tail written, not synced", Step::BodySync, false),
        ("open tail durable, no checkpoint", Step::Checkpoint, false),
        ("open upload committed", Step::AfterCommit, true),
    ]
    .into_iter()
    .enumerate()
    {
        let path = resumable_prefix(&h, "target", &chunk).await;
        let blob = session_blob(&h, &path).await;
        let snapshot = crashes.0.join(index.to_string());
        h.inject(step, 0, Fault::PowerLoss(snapshot.clone()));
        assert_eq!(
            put_open_tail(&h, &path, b"tail").await.status,
            200,
            "{case}"
        );
        h.assert_fired(case);
        let expected: &[u8] = if committed { &body } else { b"old" };
        assert_recovers(&snapshot, "target", Some(expected), case).await;
        if !committed {
            assert_resumable_prefix(&snapshot, &blob, &chunk, case);
        }
        assert_eq!(h.media("target", b"old").await.status, 200);
    }
    h.stop().await;
}

/// The staged blob ID of the session at `path`.
#[cfg(unix)]
async fn session_blob(h: &Harness, path: &str) -> String {
    let id = path.split("upload_id=").nth(1).unwrap();
    h.state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap()
        .blob_id
}

/// After recovery an uncommitted session is still active, and its staged body holds exactly
/// the acknowledged prefix, so the client can query the offset and resume.
#[cfg(unix)]
fn assert_resumable_prefix(snapshot: &Path, blob_id: &str, prefix: &[u8], case: &str) {
    let connection = rusqlite::Connection::open(snapshot.join("catalog.sqlite3")).unwrap();
    let (state, offset): (String, i64) = connection
        .query_row(
            "SELECT state, offset FROM resumable_sessions WHERE blob_id = ?1",
            [blob_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "ACTIVE", "{case}");
    assert_eq!(usize::try_from(offset).unwrap(), prefix.len(), "{case}");
    let staged = std::fs::read(snapshot.join("staging").join(blob_id)).unwrap();
    assert!(staged == prefix, "{case}: staged prefix differs");
}

/// Power loss at each durable step of a resumable finalization: before the commit the old
/// object stays and the session resumes from its last checkpoint; after it the new object is
/// complete. Without the final chunk's `sync_all` the committed blob comes back short, and
/// without the shard sync it is back in staging, so the committed case fails.
#[cfg(unix)]
#[tokio::test]
async fn resumable_power_loss_at_each_durable_step_keeps_the_old_or_new_object() {
    let crashes = TestDir::new();
    let h = Harness::start(4 * 1024 * 1024, false, None).await;
    assert_eq!(h.media("target", b"old").await.status, 200);
    let chunk = vec![b'a'; 256 * 1024];
    let mut body = chunk.clone();
    body.extend_from_slice(b"tail");
    let last = "Content-Range: bytes 262144-262147/262148\r\n";
    for (index, (case, step, acknowledged)) in [
        (
            "final chunk written, not synced",
            Step::BodySync,
            Some(chunk.len()),
        ),
        (
            "final chunk durable, no checkpoint",
            Step::Checkpoint,
            Some(chunk.len()),
        ),
        ("staged body durable", Step::BeforePublish, Some(body.len())),
        (
            "blob renamed, no catalog row",
            Step::AfterPublish,
            Some(body.len()),
        ),
        (
            "publication transaction open",
            Step::BeforeCommit,
            Some(body.len()),
        ),
        ("committed", Step::AfterCommit, None),
    ]
    .into_iter()
    .enumerate()
    {
        let path = resumable_prefix(&h, "target", &chunk).await;
        let blob = session_blob(&h, &path).await;
        let snapshot = crashes.0.join(index.to_string());
        h.inject(step, 0, Fault::PowerLoss(snapshot.clone()));
        assert_eq!(
            h.request("PUT", &path, last, b"tail").await.status,
            200,
            "{case}"
        );
        h.assert_fired(case);
        let expected: &[u8] = if acknowledged.is_some() {
            b"old"
        } else {
            &body
        };
        assert_recovers(&snapshot, "target", Some(expected), case).await;
        if let Some(length) = acknowledged {
            assert_resumable_prefix(&snapshot, &blob, &body[..length], case);
        }
        assert_eq!(h.media("target", b"old").await.status, 200);
    }
    h.stop().await;
}

/// A chunk that was synced but whose checkpoint failed is cut back and written again by the
/// retry. The bytes it rewrites were durable before the cut, so only the retry's own sync may
/// make them durable again; power loss after the commit must still find the whole object.
#[cfg(unix)]
#[tokio::test]
async fn resumable_retry_after_a_failed_checkpoint_syncs_the_rewritten_chunk() {
    let crashes = TestDir::new();
    let h = Harness::start(4 * 1024 * 1024, false, None).await;
    let chunk = vec![b'a'; 256 * 1024];
    let path = resumable_prefix(&h, "retried", &chunk).await;
    let blob = session_blob(&h, &path).await;
    let last = "Content-Range: bytes 262144-524287/524288\r\n";
    let tail = vec![b'b'; 256 * 1024];
    h.inject(Step::Checkpoint, 0, Fault::Io(ErrorKind::Other));
    let reply = h.request("PUT", &path, last, &tail).await;
    assert_backend_failure(
        &reply,
        &Expect::Message(io_message(ErrorKind::Other)),
        "checkpoint",
    );
    h.assert_fired("checkpoint");
    // The unacknowledged chunk is durable on disk beyond the acknowledged offset. A status
    // query would cut and sync the file first, so the retry follows directly, as a client
    // that retries the same range without querying does.
    assert_eq!(
        std::fs::metadata(h.dir.0.join("staging").join(&blob))
            .unwrap()
            .len(),
        512 * 1024
    );
    let snapshot = crashes.0.join("retry");
    h.inject(Step::AfterCommit, 0, Fault::PowerLoss(snapshot.clone()));
    assert_eq!(h.request("PUT", &path, last, &tail).await.status, 200);
    h.assert_fired("retry");
    let expected = [chunk, tail].concat();
    assert_recovers(&snapshot, "retried", Some(&expected), "retry").await;
    h.stop().await;
}

async fn seed_sources(h: &Harness) {
    assert_eq!(h.media("a", &vec![b'a'; 100 * 1024]).await.status, 200);
    assert_eq!(h.media("b", &vec![b'b'; 100 * 1024]).await.status, 200);
}

/// The derived write paths: compose, copy and rewrite all stream into a derived transfer
/// and publish it through the shared derived publication.
async fn derive(h: &Harness, operation: &str) -> Reply {
    match operation {
        "compose" => {
            h.request(
                "POST",
                "/storage/v1/b/bucket/o/derived/compose",
                "Content-Type: application/json\r\n",
                br#"{"sourceObjects":[{"name":"a"},{"name":"b"}]}"#,
            )
            .await
        }
        "copy" => {
            h.request(
                "POST",
                "/storage/v1/b/bucket/o/a/copyTo/b/bucket/o/derived",
                "",
                b"",
            )
            .await
        }
        "rewrite" => {
            h.request(
                "POST",
                "/storage/v1/b/bucket/o/a/rewriteTo/b/bucket/o/derived",
                "",
                b"",
            )
            .await
        }
        _ => unreachable!("{operation}"),
    }
}

fn derived_failures() -> Vec<(&'static str, Step, usize, Fault)> {
    vec![
        (
            "body write, disk full",
            Step::BodyWrite,
            0,
            Fault::Io(ErrorKind::StorageFull),
        ),
        (
            "mid-body write",
            Step::BodyWrite,
            1,
            Fault::Io(ErrorKind::Other),
        ),
        (
            "final tail write, disk full",
            Step::BodyFlush,
            0,
            Fault::Io(ErrorKind::StorageFull),
        ),
        ("fsync", Step::BodySync, 0, Fault::Io(ErrorKind::Other)),
        (
            "rename",
            Step::BeforePublish,
            0,
            Fault::Io(ErrorKind::PermissionDenied),
        ),
        (
            "after rename",
            Step::AfterPublish,
            0,
            Fault::Io(ErrorKind::Other),
        ),
        ("commit, busy", Step::BeforeCommit, 0, Fault::Busy),
        ("commit, corrupt", Step::BeforeCommit, 0, Fault::Corrupt),
        (
            "commit, disk full",
            Step::BeforeCommit,
            0,
            Fault::Io(ErrorKind::StorageFull),
        ),
    ]
}

#[tokio::test]
async fn compose_copy_and_rewrite_failures_are_classified_and_never_publish() {
    let h = Harness::start(1024 * 1024, false, None).await;
    seed_sources(&h).await;
    for operation in ["compose", "copy", "rewrite"] {
        for (case, step, skip, fault) in derived_failures() {
            let case = format!("{operation}: {case}");
            let expect = expect_for(&fault);
            h.inject(step, skip, fault);
            let reply = derive(&h, operation).await;
            assert_backend_failure(&reply, &expect, &case);
            h.assert_fired(&case);
            assert_eq!(h.resource("derived").await.status, 404, "{case}");
            assert_eq!(
                h.media_bytes("a").await.body,
                vec![b'a'; 100 * 1024],
                "{case}"
            );
            assert_eq!(
                count(&h.dir.0, "SELECT count(*) FROM derived_transfers"),
                0,
                "{case}"
            );
            assert_no_leaks(&h.dir.0, &case);
        }
        let reply = derive(&h, operation).await;
        assert_eq!(reply.status, 200, "{operation}");
        assert_eq!(
            h.request("DELETE", "/storage/v1/b/bucket/o/derived", "", b"")
                .await
                .status,
            204
        );
    }
    h.stop().await;
}

#[tokio::test]
async fn ambiguous_derived_commit_error_keeps_the_committed_destination() {
    let h = Harness::start(1024 * 1024, false, None).await;
    seed_sources(&h).await;
    for operation in ["compose", "copy", "rewrite"] {
        h.inject(Step::AfterCommit, 0, Fault::Io(ErrorKind::Other));
        let reply = derive(&h, operation).await;
        assert_backend_failure(
            &reply,
            &Expect::Message(io_message(ErrorKind::Other)),
            operation,
        );
        h.assert_fired(operation);
        let expected = match operation {
            "compose" => [vec![b'a'; 100 * 1024], vec![b'b'; 100 * 1024]].concat(),
            _ => vec![b'a'; 100 * 1024],
        };
        assert_eq!(h.media_bytes("derived").await.body, expected, "{operation}");
        assert_eq!(
            h.request("DELETE", "/storage/v1/b/bucket/o/derived", "", b"")
                .await
                .status,
            204
        );
    }
    h.stop().await;
}

#[tokio::test]
async fn derived_crash_at_each_durable_step_recovers_without_a_partial_destination() {
    let crashes = TestDir::new();
    let h = Harness::start(1024 * 1024, false, None).await;
    seed_sources(&h).await;
    let mut index = 0;
    for operation in ["compose", "copy", "rewrite"] {
        for (case, step, committed) in [
            ("body written, not synced", Step::BodySync, false),
            ("blob renamed, no catalog row", Step::AfterPublish, false),
            ("publication transaction open", Step::BeforeCommit, false),
            ("committed, sources not cleaned", Step::AfterCommit, true),
        ] {
            let case = format!("{operation}: {case}");
            let snapshot = crashes.0.join(index.to_string());
            index += 1;
            h.inject(step, 0, Fault::Snapshot(snapshot.clone()));
            assert_eq!(derive(&h, operation).await.status, 200, "{case}");
            h.assert_fired(&case);
            let expected = match operation {
                "compose" => [vec![b'a'; 100 * 1024], vec![b'b'; 100 * 1024]].concat(),
                _ => vec![b'a'; 100 * 1024],
            };
            assert_recovers(
                &snapshot,
                "derived",
                committed.then_some(&expected[..]),
                &case,
            )
            .await;
            assert_eq!(
                h.request("DELETE", "/storage/v1/b/bucket/o/derived", "", b"")
                    .await
                    .status,
                204
            );
        }
    }
    h.stop().await;
}

/// Power loss at each durable step of compose, copy and rewrite publication. The destination
/// is absent until the commit and complete after it, and the sources stay intact. Without the
/// derived body's `sync_all` the committed destination comes back short, and without the shard
/// sync its blob is back in staging, so the committed cases fail.
#[cfg(unix)]
#[tokio::test]
async fn derived_power_loss_at_each_durable_step_keeps_no_partial_destination() {
    let crashes = TestDir::new();
    let h = Harness::start(1024 * 1024, false, None).await;
    seed_sources(&h).await;
    let (a, b) = (vec![b'a'; 100 * 1024], vec![b'b'; 100 * 1024]);
    let mut index = 0;
    for operation in ["compose", "copy", "rewrite"] {
        let expected = match operation {
            "compose" => [a.clone(), b.clone()].concat(),
            _ => a.clone(),
        };
        for (case, step, committed) in [
            ("body written, not synced", Step::BodySync, false),
            ("body durable, not renamed", Step::BeforePublish, false),
            ("blob renamed, no catalog row", Step::AfterPublish, false),
            ("publication transaction open", Step::BeforeCommit, false),
            ("committed, sources not cleaned", Step::AfterCommit, true),
        ] {
            let case = format!("{operation}: {case}");
            let snapshot = crashes.0.join(index.to_string());
            index += 1;
            h.inject(step, 0, Fault::PowerLoss(snapshot.clone()));
            assert_eq!(derive(&h, operation).await.status, 200, "{case}");
            h.assert_fired(&case);
            assert_recovers_objects(
                &snapshot,
                &[
                    ("derived", committed.then_some(&expected[..])),
                    ("a", Some(&a)),
                    ("b", Some(&b)),
                ],
                &case,
            )
            .await;
            assert_eq!(
                h.request("DELETE", "/storage/v1/b/bucket/o/derived", "", b"")
                    .await
                    .status,
                204
            );
        }
    }
    h.stop().await;
}
