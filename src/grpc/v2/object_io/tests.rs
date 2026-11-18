use super::*;
use super::{
    object_mask::{ObjectMask, UnselectablePath},
    read::read_range,
    resumable::{Call, Persisted, Refusal},
    write::WriteRpc,
};
use crate::grpc::test_support::{self as fixtures, assert_code, assert_unsupported, registry};
use crate::grpc::{v2::mask::ReadMask, validate::ByteRange};
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::{auth::Auth, storage::Catalog, test_support::TestDir};
use prost::Message;
use serde_json::json;
use std::sync::Arc;

/// A decoded data message; `finish` sets `finish_write`.
fn data_step(offset: i64, content: bytes::Bytes, finish: bool) -> WriteStep {
    WriteStep::from(pb::BidiWriteObjectRequest {
        write_offset: offset,
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content,
                ..Default::default()
            },
        )),
        finish_write: finish,
        ..Default::default()
    })
}

fn open_session(offset: u64) -> Persisted {
    Persisted {
        offset,
        total: None,
    }
}

/// The status of a refusal that leaves the session usable.
fn message_refusal(result: Result<super::resumable::Piece, Refusal>) -> Status {
    match result {
        Err(Refusal::Message(status)) => status,
        Err(Refusal::Session(status)) => panic!("the refusal ended the session: {status:?}"),
        Ok(_) => panic!("the message was accepted"),
    }
}

#[test]
fn resumable_message_end_must_fit_the_wire_offset() {
    for (offset, accepted) in [(i64::MAX - 1, true), (i64::MAX, false)] {
        let step = data_step(offset, bytes::Bytes::from_static(b"x"), true);
        let result = Call::default().admit(step, open_session(offset.cast_unsigned()), u64::MAX);
        if accepted {
            assert_eq!(result.ok().unwrap().length, 1);
        } else {
            let error = message_refusal(result);
            assert_eq!(error.code(), tonic::Code::OutOfRange);
            assert_eq!(
                error.message(),
                "write_offset plus content length overflows"
            );
        }
    }
}

/// Recorded 2026-11-18: Cloud Storage persists whole 256 KiB granules until `finish_write`,
/// and a call that closes with bytes past the last granule fails.
#[test]
fn resumable_call_persists_whole_granules_until_finish() {
    const GRANULE: usize = 256 * 1024;
    let mut call = Call::default();
    call.close().unwrap();
    let piece = call
        .admit(
            data_step(0, bytes::Bytes::from_static(b"hel"), false),
            open_session(0),
            u64::MAX,
        )
        .ok()
        .unwrap();
    assert_eq!(piece.length, 0);
    let closed = call.close().unwrap_err();
    assert_eq!(closed.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        closed.message(),
        "Exactly 3 bytes were uploaded in this call, which is not a multiple of the 262144 byte write granularity, but finish_write was not set. Some trailing data was not written. Call 'QueryWriteStatus' to see exactly how much data has been recorded."
    );
    // The next message completes the granule: the waiting bytes are persisted ahead of it.
    let rest = bytes::Bytes::from(vec![b'x'; GRANULE]);
    let piece = call
        .admit(data_step(3, rest, false), open_session(0), u64::MAX)
        .ok()
        .unwrap();
    assert_eq!(piece.length, 262_144);
    assert_eq!(piece.parts[0], b"hel"[..]);
    assert_eq!(piece.parts[1].len(), GRANULE - 3);
    assert!(call.close().is_err());
    // A later message may repeat received bytes; they are not written twice. finish_write
    // persists everything that waits.
    let piece = call
        .admit(
            data_step(262_146, bytes::Bytes::from_static(b"xyz"), true),
            open_session(262_144),
            u64::MAX,
        )
        .ok()
        .unwrap();
    assert_eq!(piece.parts[0], b"xxx"[..]);
    assert_eq!(piece.parts[1], b"yz"[..]);
    assert_eq!(piece.length, 5);
    call.close().unwrap();

    // Bytes that wait while another stream persists past them are not written again.
    let mut call = Call::default();
    call.admit(
        data_step(0, bytes::Bytes::from_static(b"hel"), false),
        open_session(0),
        u64::MAX,
    )
    .ok()
    .unwrap();
    let piece = call
        .admit(
            data_step(3, bytes::Bytes::from_static(b"lo"), false),
            open_session(262_144),
            u64::MAX,
        )
        .ok()
        .unwrap();
    assert_eq!(piece.length, 0);
    call.close().unwrap();
}

/// The refusal of `written` bytes against a declared `object_size` of 9.
#[track_caller]
fn assert_size_mismatch(status: &Status, written: usize) {
    assert_eq!(status.code(), tonic::Code::OutOfRange);
    assert_eq!(
        status.message(),
        format!(
            "Invalid request. The final size of the upload is {written} byte(s). This does not match the expected size of 9 byte(s), which was specified in the StartResumableWrite request."
        )
    );
    fixtures::assert_reason(status, "GRPC_MISMATCHED_UPLOAD_SIZE");
}

const SIZED: Persisted = Persisted {
    offset: 0,
    total: Some(9),
};

/// Recorded 2026-11-18: a first offset past the persisted size names both offsets with
/// GRPC_UPLOAD_INVALID_START_OFFSET, and a finishing message below object_size ends the
/// session.
#[test]
fn resumable_call_refuses_offsets_and_sizes_as_recorded() {
    let hello = || bytes::Bytes::from_static(b"hello");
    let past = message_refusal(Call::default().admit(
        data_step(5, hello(), false),
        open_session(0),
        u64::MAX,
    ));
    assert_eq!(past.code(), tonic::Code::OutOfRange);
    assert_eq!(
        past.message(),
        "Upload request started at offset '5', which is past expected offset '0'."
    );
    fixtures::assert_reason(&past, "GRPC_UPLOAD_INVALID_START_OFFSET");
    let Err(Refusal::Session(short)) =
        Call::default().admit(data_step(0, hello(), true), SIZED, u64::MAX)
    else {
        panic!("a finishing message below object_size did not end the session")
    };
    assert_size_mismatch(&short, 5);
}

/// Not recorded; inferred. A first offset below a persisted size above 0 is refused, from the
/// proto's write_offset field (the recording always had nothing persisted). A finishing message
/// above object_size ends the session as one below it does: Cloud Storage's refusal was
/// recorded, but not the session's state after it. Bytes past object_size without
/// finish_write are refused and leave the session usable: no such message was recorded.
#[test]
fn resumable_call_refuses_unrecorded_offsets_and_sizes() {
    let below = message_refusal(Call::default().admit(
        data_step(1, bytes::Bytes::from_static(b"hello"), false),
        open_session(262_144),
        u64::MAX,
    ));
    assert_eq!(below.code(), tonic::Code::OutOfRange);
    assert_eq!(
        below.message(),
        "write_offset does not match persisted data; call QueryWriteStatus and resume at persisted_size"
    );
    let ten = || bytes::Bytes::from_static(b"0123456789");
    let Err(Refusal::Session(long)) =
        Call::default().admit(data_step(0, ten(), true), SIZED, u64::MAX)
    else {
        panic!("a finishing message above object_size did not end the session")
    };
    assert_size_mismatch(&long, 10);
    let open = message_refusal(Call::default().admit(data_step(0, ten(), false), SIZED, u64::MAX));
    assert_size_mismatch(&open, 10);
}

#[test]
fn object_errors_keep_the_v2_unsupported_metadata_wording() {
    let metadata = object_status(ObjectError::UnsupportedMetadata("test_setting".into()));
    let generic = Status::from(ObjectError::UnsupportedMetadata("test_setting".into()));
    assert_eq!(metadata.code(), tonic::Code::Unimplemented);
    assert_eq!(
        metadata.message(),
        "gcs-mimic does not support gRPC unsupported metadata change: test_setting; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
    );
    assert_ne!(metadata.message(), generic.message());
}

pub(super) async fn setup(strict: bool) -> (TestDir, Storage) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let storage = storage_over(catalog, strict).await;
    (dir, storage)
}

/// The fixture bucket and principals over `catalog`, which a test may open with its own clock.
async fn storage_over(catalog: Catalog, strict: bool) -> Storage {
    let definition = json::parse_insert(
        br#"{"name":"grpc-io","versioning":{"enabled":true}}"#,
        "project",
        std::time::SystemTime::now(),
    )
    .unwrap();
    catalog.insert_bucket(definition).await.unwrap();
    let auth = if strict {
        Auth::strict(registry(&[
            ("admin", "project", "roles/storage.admin"),
            ("reader", "project", "roles/storage.objectViewer"),
            ("user", "project", "roles/storage.objectUser"),
        ]))
    } else {
        Auth::permissive()
    };
    Storage {
        compat_profile: crate::config::CompatProfile::Native,
        state: Arc::new(crate::server::State::for_test(catalog, auth)),
        max_object_bytes: 1024 * 1024,
    }
}

pub(super) fn spec(name: &str) -> pb::WriteObjectSpec {
    pb::WriteObjectSpec {
        resource: Some(pb::Object {
            bucket: "projects/_/buckets/grpc-io".into(),
            name: name.into(),
            metadata: [("key".into(), "value".into())].into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(super) fn message(name: &str, data: &[u8]) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        first_message: Some(
            pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec(name)),
        ),
        finish_write: true,
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: bytes::Bytes::copy_from_slice(data),
                crc32c: Some(crc32c::crc32c(data)),
            },
        )),
        ..Default::default()
    }
}

pub(super) async fn write(
    storage: &Storage,
    messages: Vec<pb::BidiWriteObjectRequest>,
) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
    write_as(storage, messages, None).await
}

pub(super) async fn write_as(
    storage: &Storage,
    messages: Vec<pb::BidiWriteObjectRequest>,
    caller: Option<Caller<'_>>,
) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
    storage
        .receive_write(
            &mut tonic::codegen::tokio_stream::iter(messages.into_iter().map(Ok)),
            caller,
            WriteRpc::WriteObject,
        )
        .await
        .map(|answer| {
            answer
                .write_status
                .expect("every write answers with a status")
        })
}

#[tokio::test]
async fn grpc_io_rejects_unrestorable_names_and_accepts_tabs() {
    let (_dir, storage) = setup(false).await;
    for name in [".", "..", ".well-known/acme-challenge/token"] {
        assert_code(
            write(&storage, vec![message(name, b"x")]).await,
            tonic::Code::InvalidArgument,
        );
        assert!(missing(&storage, name).await);
    }
    write(&storage, vec![message("a\tb", b"x")]).await.unwrap();
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_finalized_resume_returns_the_original_resource() {
    let (_dir, storage) = setup(false).await;
    let upload = start(&storage, "replayed", None).await;
    let original = write(&storage, vec![resume(&upload, 0, b"original", true)])
        .await
        .unwrap();
    write(&storage, vec![message("replayed", b"replacement")])
        .await
        .unwrap();
    let replayed = write(&storage, vec![resume(&upload, 0, b"ignored", true)])
        .await
        .unwrap();
    assert_eq!(replayed, original);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_concurrent_finalization_rejects_later_messages() {
    let (_dir, storage) = setup(false).await;
    let upload = start(&storage, "concurrent", None).await;
    let mut first = resume(&upload, 0, &granule(), false);
    first.flush = true;
    let (requests, incoming) = mpsc::channel(1);
    requests.send(Ok(first)).await.unwrap();
    let (responses, mut acknowledgements) =
        mpsc::channel::<Result<pb::BidiWriteObjectResponse, Status>>(1);
    let later = async {
        acknowledgements.recv().await.unwrap().unwrap();
        write(&storage, vec![resume(&upload, GRANULE, b"winner", true)])
            .await
            .unwrap();
        let mut next = resume(&upload, GRANULE, b"discarded", true);
        next.first_message = None;
        requests.send(Ok(next)).await.unwrap();
        drop(requests);
    };
    let mut stream = ReceiverStream::new(incoming);
    let (result, ()) = tokio::join!(
        storage.receive_write(&mut stream, None, WriteRpc::Bidi(&responses)),
        later
    );
    assert_code(result, tonic::Code::FailedPrecondition);
    let record = storage
        .state
        .catalog
        .get_object(
            live("grpc-io", "concurrent"),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(record.size, GRANULE.cast_unsigned() + 6);
    storage.state.catalog.close().await.unwrap();
}

/// The proto: "When the stream is closed, a `BidiWriteObjectResponse` is always sent to the
/// client, regardless of the value of `state_lookup`." A clean close of a resumed stream answers
/// with the persisted size, also after a flush already answered. The stream sends whole
/// granules: bytes past the last one fail the close
/// (`grpc_io_resumable_close_drops_bytes_past_the_last_granule`).
#[tokio::test]
async fn grpc_io_bidi_half_close_reports_unacknowledged_persisted_size() {
    let (_dir, storage) = setup(false).await;
    for (name, data, flush, answers) in [
        ("half-close", granule(), false, 1),
        ("flushed", granule(), true, 2),
        ("empty", Vec::new(), false, 1),
    ] {
        let upload = start(&storage, name, None).await;
        let mut message = resume(&upload, 0, &data, false);
        message.flush = flush;
        let mut stream = tonic::codegen::tokio_stream::iter([Ok(message)]);
        let (tx, mut rx) = mpsc::channel(2);
        let result = storage
            .receive_write(&mut stream, None, WriteRpc::Bidi(&tx))
            .await
            .unwrap();
        assert_eq!(
            result.write_status,
            Some(pb::bidi_write_object_response::WriteStatus::PersistedSize(
                data.len() as i64
            ))
        );
        drop(tx);
        for _ in 0..answers {
            let response = rx.recv().await.unwrap().unwrap();
            assert_eq!(response, result, "{name}");
        }
        assert!(rx.recv().await.is_none(), "{name}");
        assert!(missing(&storage, name).await);
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_read_failures_use_shared_storage_status() {
    let (dir, storage) = setup(false).await;
    write(&storage, vec![message("short-blob", b"abc")])
        .await
        .unwrap();
    let shard = std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let blob = std::fs::read_dir(shard)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::OpenOptions::new()
        .write(true)
        .open(blob)
        .unwrap()
        .set_len(0)
        .unwrap();
    let mut stream = storage
        .read_object(
            Request::new(pb::ReadObjectRequest {
                bucket: "projects/_/buckets/grpc-io".into(),
                object: "short-blob".into(),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner();
    let read_error = stream.next().await.unwrap().unwrap_err();
    assert!(
        read_error
            .message()
            .starts_with("Object storage is unavailable")
    );
    let read_code = read_error.code();
    let (status, _): (_, Vec<pb::BidiReadObjectResponse>) = fixtures::routed_rpc_status(
        "/google.storage.v2.Storage/BidiReadObject",
        None,
        &fixtures::bucket_routing("grpc-io"),
        &[pb::BidiReadObjectRequest {
            read_object_spec: Some(pb::BidiReadObjectSpec {
                bucket: "projects/_/buckets/grpc-io".into(),
                object: "short-blob".into(),
                ..Default::default()
            }),
            read_ranges: [7, 8, 9]
                .into_iter()
                .map(|read_id| pb::ReadRange {
                    read_id,
                    ..Default::default()
                })
                .collect(),
        }],
        async |request| {
            super::super::dispatch(
                super::super::service(storage.state.clone(), 1024, storage.compat_profile),
                request,
            )
            .await
            .unwrap()
        },
    )
    .await;
    assert_eq!([read_code, status.code()], [tonic::Code::Unavailable; 2]);
    let rpc = crate::grpc::generated::google::rpc::Status::decode(status.details()).unwrap();
    assert_eq!(rpc.code, status.code() as i32);
    assert_eq!(rpc.message, status.message());
    let detail = pb::BidiReadObjectError::decode(rpc.details[0].value.as_slice()).unwrap();
    let states = detail
        .read_range_errors
        .into_iter()
        .map(|range| (range.read_id, range.status.unwrap().code))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        states,
        [
            (7, tonic::Code::Unavailable as i32),
            (8, tonic::Code::Aborted as i32),
            (9, tonic::Code::Aborted as i32)
        ]
        .into()
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_disconnected_resumable_stream_releases_its_lease() {
    let (dir, storage) = setup(false).await;
    let upload = start(&storage, "disconnected", None).await;
    let mut stream = tonic::codegen::tokio_stream::iter([
        Ok(resume(&upload, 0, &granule(), false)),
        Err(Status::cancelled("client disconnected")),
    ]);
    assert_code(
        storage
            .receive_write(&mut stream, None, WriteRpc::WriteObject)
            .await,
        tonic::Code::Cancelled,
    );
    assert_eq!(
        query(&storage, &upload, None).await.unwrap(),
        pb::query_write_status_response::WriteStatus::PersistedSize(GRANULE)
    );
    assert!(missing(&storage, "disconnected").await);
    write(&storage, vec![resume(&upload, GRANULE, b"def", true)])
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    storage.state.catalog.close().await.unwrap();
}

/// A single-shot write hands its body to the catalog through a channel and sends the end
/// marker only after `finish_write`. A request stream that fails after a message, or that ends
/// without `finish_write`, closes that channel without the marker, so nothing is published and
/// nothing stays staged.
#[tokio::test]
async fn grpc_io_single_shot_stream_cut_short_publishes_nothing() {
    let (dir, storage) = setup(false).await;
    let opening = |name| {
        let mut opening = message(name, b"abc");
        opening.finish_write = false;
        opening
    };
    let failed = vec![
        Ok(opening("failed-stream")),
        Err(Status::cancelled("client disconnected")),
    ];
    let unfinished = vec![Ok(opening("unfinished-stream"))];
    for (name, messages, code) in [
        ("failed-stream", failed, tonic::Code::Cancelled),
        (
            "unfinished-stream",
            unfinished,
            tonic::Code::InvalidArgument,
        ),
    ] {
        assert_code(
            storage
                .receive_write(
                    &mut tonic::codegen::tokio_stream::iter(messages),
                    None,
                    WriteRpc::WriteObject,
                )
                .await,
            code,
        );
        assert!(missing(&storage, name).await, "{name}");
        assert_eq!(
            std::fs::read_dir(dir.0.join("staging")).unwrap().count(),
            0,
            "{name}"
        );
    }
    storage.state.catalog.close().await.unwrap();
}

/// A single-shot write's first message is admitted, and the catalog's advisory check of its
/// conditions passes, before the body streams. The transaction that publishes the object
/// checks the conditions again: a create-only write that another write overtakes meanwhile
/// fails FAILED_PRECONDITION, publishes nothing and leaves nothing staged.
#[tokio::test]
async fn grpc_io_single_shot_conditions_are_rechecked_at_publication() {
    let (dir, storage) = setup(false).await;
    let mut opening = message("overtaken", b"abc");
    opening.finish_write = false;
    opening.flush = true;
    let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
        opening.first_message.as_mut()
    else {
        unreachable!()
    };
    spec.if_generation_match = Some(0);
    let (requests, incoming) = mpsc::channel(1);
    requests.send(Ok(opening)).await.unwrap();
    let (responses, mut answered) = mpsc::channel::<Result<pb::BidiWriteObjectResponse, Status>>(1);
    let overtake = async {
        // The flush is answered once the opening message was admitted and staged, after the
        // advisory check.
        answered.recv().await.unwrap().unwrap();
        write(&storage, vec![message("overtaken", b"winner")])
            .await
            .unwrap();
        let mut last = message("ignored", b"def");
        last.first_message = None;
        last.write_offset = 3;
        requests.send(Ok(last)).await.unwrap();
        drop(requests);
    };
    let mut stream = ReceiverStream::new(incoming);
    let (result, ()) = tokio::join!(
        storage.receive_write(&mut stream, None, WriteRpc::Bidi(&responses)),
        overtake
    );
    let refused = assert_code(result, tonic::Code::FailedPrecondition);
    assert_eq!(
        refused.message(),
        "At least one of the pre-conditions you specified did not hold."
    );
    let record = storage
        .state
        .catalog
        .get_object(live("grpc-io", "overtaken"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(record.size, 6, "the overtaking write stays live");
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    storage.state.catalog.close().await.unwrap();
}

/// A request stream that yields `items`, then stays open, as a client that never half-closes
/// does, until its [`Hold`] releases it.
pub(super) struct HeldOpen<T> {
    items: std::collections::VecDeque<T>,
    waiting: Option<tokio::sync::oneshot::Sender<()>>,
    release: tokio::sync::oneshot::Receiver<()>,
}

/// The test's end of a [`HeldOpen`] stream.
pub(super) struct Hold {
    /// Fires once a reader asks for more than the stream's items.
    pub(super) waited: tokio::sync::oneshot::Receiver<()>,
    /// Ends the stream.
    pub(super) release: tokio::sync::oneshot::Sender<()>,
}

impl<T> HeldOpen<T> {
    pub(super) fn new(items: impl IntoIterator<Item = T>) -> (Self, Hold) {
        let (waiting, waited) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let stream = Self {
            items: items.into_iter().collect(),
            waiting: Some(waiting),
            release: released,
        };
        (stream, Hold { waited, release })
    }
}

impl<T: Unpin> Stream for HeldOpen<T> {
    type Item = T;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if let Some(item) = self.items.pop_front() {
            return std::task::Poll::Ready(Some(item));
        }
        if let Some(waiting) = self.waiting.take() {
            let _ = waiting.send(());
        }
        std::pin::Pin::new(&mut self.release).poll(cx).map(|_| None)
    }
}

/// Recorded 2026-11-18: Cloud Storage answers WriteObject at finish_write and ignores a message
/// that follows it. So the answer does not wait for the stream to end, a client that keeps its
/// stream open holds no session, and a message after finish_write changes nothing.
#[tokio::test]
async fn grpc_io_write_object_answers_at_finish_write_without_reading_further() {
    let (_dir, storage) = setup(false).await;
    let upload = start(&storage, "held-open", None).await;
    let id = upload.parse::<UploadId>().unwrap();
    let (mut stream, mut hold) = HeldOpen::new([Ok(resume(&upload, 0, b"abc", true))]);
    let result = storage
        .receive_write(&mut stream, None, WriteRpc::WriteObject)
        .await;
    assert!(
        hold.waited.try_recv().is_err(),
        "the write read past finish_write"
    );
    let lease = storage
        .state
        .catalog
        .lock_resumable_session(id.session(), id.bucket())
        .await;
    assert!(lease.is_ok(), "the session stayed leased after the answer");
    drop(lease);
    let Some(pb::bidi_write_object_response::WriteStatus::Resource(object)) =
        result.unwrap().write_status
    else {
        panic!("not finalized")
    };
    assert_eq!(object.size, 3);

    let followed = start(&storage, "followed", None).await;
    let mut later = resume(&followed, 3, b"d", false);
    later.first_message = None;
    let written = write(&storage, vec![resume(&followed, 0, b"abc", true), later])
        .await
        .unwrap();
    let pb::bidi_write_object_response::WriteStatus::Resource(object) = written else {
        panic!("not finalized")
    };
    assert_eq!(object.size, 3);
    let mut later = message("ignored", b"d");
    later.first_message = None;
    later.write_offset = 3;
    later.finish_write = false;
    let written = write(&storage, vec![message("single-followed", b"abc"), later])
        .await
        .unwrap();
    let pb::bidi_write_object_response::WriteStatus::Resource(object) = written else {
        panic!("not written")
    };
    assert_eq!(object.size, 3);
    storage.state.catalog.close().await.unwrap();
}

/// A finishing message that is refused, or that replays a finalized upload, is answered at once
/// as well: WriteObject reads nothing past finish_write (recorded 2026-11-18).
#[tokio::test]
async fn grpc_io_write_object_answers_a_refused_finishing_message_at_once() {
    let (_dir, storage) = setup(false).await;
    let refused = start(&storage, "refused-held-open", None).await;
    let mut bad_crc = resume(&refused, 0, b"abc", true);
    if let Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) = bad_crc.data.as_mut()
    {
        data.crc32c = Some(1);
    }
    let finalized = start(&storage, "replayed-held-open", None).await;
    write(&storage, vec![resume(&finalized, 0, b"abc", true)])
        .await
        .unwrap();
    let crc_mismatch = format!(
        "Checksummed data message crc32c mismatch: server computed: '{:08x}', client provided '00000001'.",
        crc32c::crc32c(b"abc")
    );
    let unknown = format!("grpc-io/{}", "0".repeat(64));
    for (label, message, expected) in [
        (
            "unknown-session",
            resume(&unknown, 0, b"abc", true),
            (tonic::Code::NotFound, UPLOAD_NOT_FOUND),
        ),
        (
            "offset",
            resume(&refused, 5, b"abc", true),
            (
                tonic::Code::OutOfRange,
                "Upload request started at offset '5', which is past expected offset '0'.",
            ),
        ),
        (
            "crc",
            bad_crc,
            (tonic::Code::InvalidArgument, crc_mismatch.as_str()),
        ),
        (
            "replayed",
            resume(&finalized, 0, b"abc", true),
            (tonic::Code::Ok, ""),
        ),
    ] {
        let (mut stream, mut hold) = HeldOpen::new([Ok(message)]);
        let result = storage
            .receive_write(&mut stream, None, WriteRpc::WriteObject)
            .await;
        assert!(
            hold.waited.try_recv().is_err(),
            "{label} read past finish_write"
        );
        assert_eq!(
            result.as_ref().map_or_else(
                |status| (status.code(), status.message()),
                |_| (tonic::Code::Ok, "")
            ),
            expected,
            "{label}"
        );
    }
    storage.state.catalog.close().await.unwrap();
}

#[test]
fn grpc_io_range_and_masks() {
    assert_eq!(
        Status::from(json::JsonError::new(
            hyper::StatusCode::NOT_MODIFIED,
            "notModified",
            "Object condition matched"
        ))
        .code(),
        tonic::Code::FailedPrecondition
    );
    let unsupported = Status::from(json::JsonError::new(
        hyper::StatusCode::NOT_IMPLEMENTED,
        "notImplemented",
        "Unsupported object option",
    ));
    assert_eq!(unsupported.code(), tonic::Code::Unimplemented);
    assert!(!unsupported.details().is_empty());
    let mismatch = WriteStep::from(pb::BidiWriteObjectRequest {
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: b"hello".to_vec().into(),
                crc32c: Some(1),
            },
        )),
        ..Default::default()
    })
    .data
    .verify()
    .map(|_| ())
    .unwrap_err();
    assert_eq!(
        mismatch,
        MessageError::ChunkCrc(upload::CrcMismatch {
            computed: 0x9a71_bb4c,
            client: 1
        })
    );
    let mismatch = Status::from(mismatch);
    assert_eq!(mismatch.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        mismatch.message(),
        "Checksummed data message crc32c mismatch: server computed: '9a71bb4c', client provided '00000001'."
    );
    let object_mask =
        |paths: &[&str]| ObjectMask::try_from(ReadMask::from(fixtures::mask(paths).as_ref()));
    assert_eq!(
        object_mask(&["noSuchField"]),
        Err(UnselectablePath::Unknown("noSuchField".into()))
    );
    assert_eq!(
        read_range(15, -5, 3).unwrap(),
        ByteRange { start: 10, len: 3 }
    );
    assert_eq!(
        read_range(15, i64::MIN, 0).unwrap(),
        ByteRange { start: 0, len: 15 }
    );
    assert_eq!(
        read_range(15, 15, 0).unwrap(),
        ByteRange { start: 15, len: 0 }
    );
    assert_code(read_range(15, 16, 0), tonic::Code::OutOfRange);
    assert_code(read_range(15, 0, -1), tonic::Code::InvalidArgument);
    let object = object_mask(&["name"]).unwrap().apply(pb::Object {
        name: "a".into(),
        size: 12,
        ..Default::default()
    });
    assert_eq!(object.name, "a");
    assert_eq!(object.size, 0);
    // Recorded 2026-11-18: Cloud Storage serves a member of a singular message.
    assert!(object_mask(&["owner.entity"]).is_ok());
}

#[tokio::test]
async fn grpc_listing_uses_proto_errors_and_preserves_page_tokens() {
    let (_dir, storage) = setup(false).await;
    let base = pb::ListObjectsRequest {
        parent: "projects/_/buckets/grpc-io".into(),
        ..Default::default()
    };
    for (request, field) in [
        (
            pb::ListObjectsRequest {
                soft_deleted: true,
                versions: true,
                ..base.clone()
            },
            "soft_deleted",
        ),
        (
            pb::ListObjectsRequest {
                match_glob: "**/*.txt".into(),
                delimiter: "-".into(),
                ..base.clone()
            },
            "match_glob",
        ),
        (
            pb::ListObjectsRequest {
                match_glob: "[".into(),
                ..base.clone()
            },
            "opening '[' at index 0",
        ),
        (
            pb::ListObjectsRequest {
                page_token: "tampered".into(),
                ..base.clone()
            },
            "page_token",
        ),
    ] {
        let error = storage
            .list_objects(Request::new(request), None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains(field), "{error}");
    }
    for name in ["a&=1", "b&=2"] {
        write(&storage, vec![message(name, b"x")]).await.unwrap();
    }
    let token = json::encode_object_page_token(&crate::storage::ObjectListCursor {
        name: "a&=1".into(),
        is_prefix: false,
        generation: None,
    });
    let page = storage
        .list_objects(
            Request::new(pb::ListObjectsRequest {
                page_size: 1,
                page_token: token,
                ..base
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.objects.len(), 1);
    assert_eq!(page.objects[0].name, "b&=2");
    storage.state.catalog.close().await.unwrap();
}

/// Every top-level `Object` field, in proto order: the read mask the Java SDK sends for
/// GetObject and ListObjects.
const ALL_OBJECT_FIELDS: [&str; 33] = [
    "name",
    "bucket",
    "etag",
    "generation",
    "restore_token",
    "metageneration",
    "storage_class",
    "size",
    "content_encoding",
    "content_disposition",
    "cache_control",
    "acl",
    "content_language",
    "delete_time",
    "finalize_time",
    "content_type",
    "create_time",
    "component_count",
    "checksums",
    "update_time",
    "kms_key",
    "update_storage_class_time",
    "temporary_hold",
    "retention_expire_time",
    "metadata",
    "contexts",
    "event_based_hold",
    "owner",
    "customer_encryption",
    "custom_time",
    "soft_delete_time",
    "hard_delete_time",
    "retention",
];

#[tokio::test]
async fn grpc_derived_writes_return_the_recorded_owner_and_acl_views() {
    let (_dir, storage) = setup(true).await;
    let admin = fixtures::caller(&storage, "admin").await;
    let bucket = "projects/_/buckets/grpc-io";
    let pb::bidi_write_object_response::WriteStatus::Resource(source) =
        write_as(&storage, vec![message("source", b"data")], admin)
            .await
            .unwrap()
    else {
        panic!("finished write must return metadata")
    };
    let composed = storage
        .compose_object(
            Request::new(pb::ComposeObjectRequest {
                destination: Some(pb::Object {
                    bucket: bucket.into(),
                    name: "composed".into(),
                    ..Default::default()
                }),
                source_objects: vec![pb::compose_object_request::SourceObject {
                    name: "source".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner();
    let rewritten = storage
        .rewrite_object(
            Request::new(pb::RewriteObjectRequest {
                source_bucket: bucket.into(),
                source_object: "source".into(),
                destination_bucket: bucket.into(),
                destination_name: "rewritten".into(),
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner()
        .resource
        .unwrap();
    let moved = storage
        .move_object(
            Request::new(pb::MoveObjectRequest {
                bucket: bucket.into(),
                source_object: "composed".into(),
                destination_object: "moved".into(),
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner();
    // The catalog refuses a move onto its own source with the message the JSON API answers.
    let onto_source = storage
        .move_object(
            Request::new(pb::MoveObjectRequest {
                bucket: bucket.into(),
                source_object: "moved".into(),
                destination_object: "moved".into(),
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap_err();
    assert_eq!(onto_source.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        onto_source.message(),
        "Source and destination object names must be different."
    );
    storage
        .state
        .catalog
        .patch_bucket(
            "grpc-io",
            crate::bucket::BucketMetadataUpdate {
                versioning: Some(crate::bucket::VersioningUpdate::Disable),
                soft_delete_retention: Some(604800_i64.try_into().unwrap()),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    storage
        .state
        .catalog
        .delete_object(
            ObjectTarget::new("grpc-io", "rewritten", Some(rewritten.generation)),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let restored = storage
        .restore_object(
            Request::new(pb::RestoreObjectRequest {
                bucket: bucket.into(),
                object: "rewritten".into(),
                generation: rewritten.generation,
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner();
    // Cloud Storage answered RewriteObject with the owner and ACL and ComposeObject and
    // MoveObject without them (case grpc-unsupported-options). RestoreObject is unrecorded and
    // keeps the write view.
    for object in [rewritten, restored] {
        assert_eq!(object.owner, source.owner, "{} owner", object.name);
        assert!(!object.acl.is_empty(), "{} ACL", object.name);
    }
    for object in [composed, moved] {
        assert!(object.owner.is_none(), "{} owner", object.name);
        assert!(object.acl.is_empty(), "{} ACL", object.name);
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_derived_write_views_respect_acl_permissions_and_uniform_access() {
    let (_dir, storage) = setup(true).await;
    let admin = fixtures::caller(&storage, "admin").await;
    let user = fixtures::caller(&storage, "user").await;
    let bucket = "projects/_/buckets/grpc-io";
    write_as(&storage, vec![message("source", b"data")], admin)
        .await
        .unwrap();
    let moved = storage
        .move_object(
            Request::new(pb::MoveObjectRequest {
                bucket: bucket.into(),
                source_object: "source".into(),
                destination_object: "moved".into(),
                ..Default::default()
            }),
            user,
        )
        .await
        .unwrap()
        .into_inner();
    assert!(moved.owner.is_none());
    assert!(moved.acl.is_empty());
    storage
        .state
        .catalog
        .patch_bucket(
            "grpc-io",
            crate::bucket::BucketMetadataUpdate {
                uniform_bucket_level_access_enabled: Some(true.into()),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let composed = storage
        .compose_object(
            Request::new(pb::ComposeObjectRequest {
                destination: Some(pb::Object {
                    bucket: bucket.into(),
                    name: "uniform".into(),
                    ..Default::default()
                }),
                source_objects: vec![pb::compose_object_request::SourceObject {
                    name: "moved".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner();
    assert!(composed.owner.is_none());
    assert!(composed.acl.is_empty());
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_write_returns_acl_and_owner_and_read_ends_with_data() {
    let (_dir, storage) = setup(true).await;
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", "Bearer admin".parse().unwrap());
    let admin = storage.authenticate(&request).await.unwrap();
    let written = storage
        .receive_write(
            &mut tonic::codegen::tokio_stream::iter([Ok(message("written", b"hello"))]),
            admin,
            WriteRpc::WriteObject,
        )
        .await
        .unwrap();
    let Some(pb::bidi_write_object_response::WriteStatus::Resource(resource)) =
        written.write_status
    else {
        panic!("a finished write answers with the resource");
    };
    assert!(!resource.acl.is_empty());
    assert!(resource.owner.is_some());
    assert!(
        resource
            .acl
            .iter()
            .any(|entry| entry.entity_alt.starts_with("project-owners-")),
        "a project-team entry repeats its entity under the project ID"
    );

    let mut read = storage
        .read_object(
            Request::new(pb::ReadObjectRequest {
                bucket: "projects/_/buckets/grpc-io".into(),
                object: "written".into(),
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner();
    let first = read.next().await.unwrap().unwrap();
    assert_eq!(first.checksummed_data.unwrap().content.as_ref(), b"hello");
    assert!(first.metadata.is_some());
    assert!(
        read.next().await.is_none(),
        "no empty message follows the last data message"
    );
}

#[tokio::test]
async fn grpc_io_full_read_masks_return_acl_and_owner() {
    let (_dir, storage) = setup(true).await;
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", "Bearer admin".parse().unwrap());
    let admin = storage.authenticate(&request).await.unwrap();
    storage
        .receive_write(
            &mut tonic::codegen::tokio_stream::iter([Ok(message("masked", b"masked"))]),
            admin,
            WriteRpc::WriteObject,
        )
        .await
        .unwrap();
    let get = |mask: Option<prost_types::FieldMask>, caller| {
        storage.get_object(
            Request::new(pb::GetObjectRequest {
                bucket: "projects/_/buckets/grpc-io".into(),
                object: "masked".into(),
                read_mask: mask,
                ..Default::default()
            }),
            caller,
        )
    };
    let full = get(fixtures::mask(&ALL_OBJECT_FIELDS), admin)
        .await
        .unwrap()
        .into_inner();
    assert_eq!((full.name.as_str(), full.size), ("masked", 6));
    assert!(!full.etag.is_empty());
    assert!(full.update_storage_class_time.is_some());
    // Google reports finalize_time on every ordinary object.
    assert!(full.finalize_time.is_some());
    // create_time is the finalize time cut to milliseconds.
    let (finalized, created) = (full.finalize_time.unwrap(), full.create_time.unwrap());
    assert_eq!(finalized.seconds, created.seconds);
    assert_eq!(finalized.nanos / 1_000_000 * 1_000_000, created.nanos);
    assert!(!full.acl.is_empty());
    for entry in &full.acl {
        assert!(!entry.entity.is_empty() && !entry.role.is_empty());
        assert!(!entry.id.is_empty() && !entry.etag.is_empty());
    }
    assert!(full.acl.iter().any(|entry| {
        entry
            .project_team
            .as_ref()
            .is_some_and(|team| team.team == "owners")
    }));
    assert!(
        full.owner
            .as_ref()
            .is_some_and(|owner| !owner.entity.is_empty())
    );
    // No local object has a KMS key, contexts or a restore token, so none is invented.
    assert!(full.kms_key.is_empty() && full.contexts.is_none());
    assert!(full.restore_token.is_none());

    let star = get(fixtures::mask(&["*"]), admin)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(star, full);

    let acl_only = get(fixtures::mask(&["name", "acl"]), admin)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(acl_only.acl, full.acl);
    assert!(acl_only.owner.is_none() && acl_only.etag.is_empty());
    assert_eq!(acl_only.size, 0);

    // The default mask omits ACL and owner.
    let default = get(None, admin).await.unwrap().into_inner();
    assert!(default.acl.is_empty() && default.owner.is_none());
    assert_eq!(default.etag, full.etag);

    // A viewer may read the object but not its ACL; the request still succeeds, as JSON
    // projection=full does.
    request
        .metadata_mut()
        .insert("authorization", "Bearer reader".parse().unwrap());
    let reader = storage.authenticate(&request).await.unwrap();
    let viewed = get(fixtures::mask(&ALL_OBJECT_FIELDS), reader)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(viewed.size, 6);
    assert!(viewed.acl.is_empty() && viewed.owner.is_none());

    let listed = storage
        .list_objects(
            Request::new(pb::ListObjectsRequest {
                parent: "projects/_/buckets/grpc-io".into(),
                read_mask: fixtures::mask(&ALL_OBJECT_FIELDS),
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.objects, std::slice::from_ref(&full));

    let mut read = storage
        .read_object(
            Request::new(pb::ReadObjectRequest {
                bucket: "projects/_/buckets/grpc-io".into(),
                object: "masked".into(),
                read_mask: fixtures::mask(&[
                    "metadata.name",
                    "metadata.acl",
                    "checksummed_data.content",
                ]),
                ..Default::default()
            }),
            admin,
        )
        .await
        .unwrap()
        .into_inner();
    let metadata = read.next().await.unwrap().unwrap().metadata.unwrap();
    assert_eq!(metadata.name, "masked");
    assert_eq!(metadata.acl, full.acl);
    assert!(metadata.owner.is_none());
    let read_with = |paths: &'static [&'static str]| {
        storage.read_object(
            Request::new(pb::ReadObjectRequest {
                bucket: "projects/_/buckets/grpc-io".into(),
                object: "masked".into(),
                read_mask: fixtures::mask(paths),
                ..Default::default()
            }),
            admin,
        )
    };
    for paths in [&["checksummed_data", "metadata.name"][..], &["*"], &[]] {
        let mut read = read_with(paths).await.unwrap().into_inner();
        let first = read.next().await.unwrap().unwrap();
        assert!(first.checksummed_data.is_some(), "{paths:?}");
    }
    // Cloud Storage rejects a mask without the object bytes (recorded).
    for (paths, shown) in [
        (
            &["metadata.checksums.md5_hash"][..],
            "metadata.checksums.md5_hash",
        ),
        (
            &["metadata", "checksummed_data.crc32c"],
            "metadata,checksummed_data.crc32c",
        ),
    ] {
        let Err(error) = read_with(paths).await else {
            panic!("{paths:?} was accepted");
        };
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            error.message(),
            format!(
                "The 'read_mask' field must include checksummed_data.content, but was '{shown}'."
            )
        );
        let details = crate::grpc::generated::google::rpc::Status::decode(error.details()).unwrap();
        let info = crate::grpc::generated::google::rpc::ErrorInfo::decode(
            details.details[0].value.as_slice(),
        )
        .unwrap();
        assert_eq!(
            (info.reason.as_str(), info.domain.as_str()),
            (
                "GRPC_READ_MASK_MISSING_CHECKSUMMED_DATA",
                "storage.googleapis.com"
            )
        );
    }
    // Recorded 2026-11-18: a member of the owner is accepted and answered without an owner,
    // which only a path naming the whole field returns.
    let nested = get(fixtures::mask(&["owner.entity"]), admin).await.unwrap();
    assert_eq!(nested.into_inner(), pb::Object::default());
    // A checksum sub-path keeps only that digest, as the Java SDK lists objects.
    let sums = full.checksums.clone().unwrap();
    assert!(sums.crc32c.is_some() && !sums.md5_hash.is_empty());
    let md5_only = get(fixtures::mask(&["name", "checksums.md5_hash"]), admin)
        .await
        .unwrap()
        .into_inner();
    let md5_only = md5_only.checksums.unwrap();
    assert_eq!(
        (md5_only.crc32c, md5_only.md5_hash),
        (None, sums.md5_hash.clone())
    );
    let crc_only = get(fixtures::mask(&["checksums.crc32c"]), admin)
        .await
        .unwrap()
        .into_inner()
        .checksums
        .unwrap();
    assert_eq!(
        (crc_only.crc32c, crc_only.md5_hash.is_empty()),
        (sums.crc32c, true)
    );
    storage.state.catalog.close().await.unwrap();
}

/// Cloud Storage stored WriteObject values of 8192 and 40,000 bytes under a one-byte key (case
/// grpc-custom-metadata-limit, recorded 2026-11-15); only the local bound on encoded metadata,
/// shared with JSON and XML, refuses a write.
#[tokio::test]
async fn grpc_io_write_keeps_custom_metadata_past_8_kib_up_to_the_local_bound() {
    let (_dir, storage) = setup(false).await;
    let request = |name: &str, length: usize| {
        let mut request = message(name, b"v1");
        let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
            &mut request.first_message
        else {
            unreachable!()
        };
        spec.resource.as_mut().unwrap().metadata = [("k".into(), "v".repeat(length))].into();
        request
    };
    for (name, length) in [
        ("at-8-kib", 8191),
        ("past-8-kib", 8192),
        ("under-40-kib", 40_000),
    ] {
        let pb::bidi_write_object_response::WriteStatus::Resource(object) =
            write(&storage, vec![request(name, length)]).await.unwrap()
        else {
            panic!("write did not finish");
        };
        assert_eq!(object.metadata["k"].len(), length, "{name}");
    }
    let refused = write(&storage, vec![request("past-40-kib", 41_000)])
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        refused.message(),
        "Object metadata exceeds 40 KiB once encoded as JSON; shorten its fixed and custom metadata."
    );
    assert!(matches!(
        storage
            .state
            .catalog
            .get_object(
                live("grpc-io", "past-40-kib"),
                Default::default(),
                PERMISSIVE
            )
            .await,
        Err(ObjectError::ObjectNotFound)
    ));
}

#[tokio::test]
async fn grpc_io_write_spec_storage_class() {
    let (_dir, storage) = setup(false).await;
    let mut request = message("cold", b"x");
    let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
        &mut request.first_message
    else {
        unreachable!()
    };
    spec.resource.as_mut().unwrap().storage_class = "COLDLINE".into();
    let pb::bidi_write_object_response::WriteStatus::Resource(object) =
        write(&storage, vec![request.clone()]).await.unwrap()
    else {
        panic!("write did not finish");
    };
    assert_eq!(object.storage_class, "COLDLINE");
    let stored = storage
        .state
        .catalog
        .get_object(live("grpc-io", "cold"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(stored.storage_class.as_str(), "COLDLINE");
    for (class, code) in [
        ("GLACIER", tonic::Code::InvalidArgument),
        ("RAPID", tonic::Code::Unimplemented),
        // The multi-region bucket cannot hold the legacy REGIONAL class (recorded 2026-11-17).
        ("REGIONAL", tonic::Code::InvalidArgument),
    ] {
        let mut bad = request.clone();
        let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
            &mut bad.first_message
        else {
            unreachable!()
        };
        spec.resource.as_mut().unwrap().storage_class = class.into();
        assert_eq!(
            write(&storage, vec![bad]).await.unwrap_err().code(),
            code,
            "{class}"
        );
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_class_and_stale_conditions_keep_error_precedence() {
    let (_dir, storage) = setup(false).await;
    write(&storage, vec![message("source", b"source")])
        .await
        .unwrap();
    write(&storage, vec![message("held", b"preserved")])
        .await
        .unwrap();
    let before = storage
        .state
        .catalog
        .get_object(live("grpc-io", "held"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let mut snapshots = Vec::new();
    for class in ["RAPID", "standard", "UNKNOWN"] {
        let mut request = message("held", b"x");
        let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
            &mut request.first_message
        else {
            unreachable!()
        };
        spec.resource.as_mut().unwrap().storage_class = class.into();
        spec.if_generation_match = Some(0);
        let resumable_spec = spec.clone();
        let status = write(&storage, vec![request]).await.unwrap_err();
        snapshots.push(json!([
            "write",
            class,
            format!("{:?}", status.code()),
            status.message()
        ]));
        let status = storage
            .start_resumable_write(
                Request::new(pb::StartResumableWriteRequest {
                    write_object_spec: Some(resumable_spec),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap_err();
        snapshots.push(json!([
            "start-resumable",
            class,
            format!("{:?}", status.code()),
            status.message()
        ]));
        for (label, source) in [("rewrite-destination", None), ("rewrite-source", Some(0))] {
            let status = storage
                .rewrite_object(
                    Request::new(pb::RewriteObjectRequest {
                        source_bucket: "projects/_/buckets/grpc-io".into(),
                        source_object: "source".into(),
                        destination_bucket: "projects/_/buckets/grpc-io".into(),
                        destination_name: "held".into(),
                        destination: Some(pb::Object {
                            storage_class: class.into(),
                            ..Default::default()
                        }),
                        if_generation_match: Some(0),
                        if_source_generation_match: source,
                        ..Default::default()
                    }),
                    None,
                )
                .await
                .unwrap_err();
            snapshots.push(json!([
                label,
                class,
                format!("{:?}", status.code()),
                status.message()
            ]));
        }
        let status = storage
            .update_bucket(
                Request::new(pb::UpdateBucketRequest {
                    bucket: Some(pb::Bucket {
                        name: "projects/_/buckets/grpc-io".into(),
                        storage_class: class.into(),
                        ..Default::default()
                    }),
                    update_mask: fixtures::mask(&["storage_class"]),
                    if_metageneration_match: Some(999),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap_err();
        snapshots.push(json!([
            "bucket-update",
            class,
            format!("{:?}", status.code()),
            status.message()
        ]));
        assert_eq!(
            storage
                .state
                .catalog
                .get_object(live("grpc-io", "held"), Default::default(), PERMISSIVE)
                .await
                .unwrap(),
            before,
            "{class}"
        );
    }
    let mut expected = Vec::new();
    for class in ["RAPID", "standard", "UNKNOWN"] {
        for label in [
            "write",
            "start-resumable",
            "rewrite-destination",
            "rewrite-source",
            "bucket-update",
        ] {
            let (code, message) = if label == "rewrite-source" {
                (
                    "FailedPrecondition",
                    "At least one of the pre-conditions you specified did not hold.".to_owned(),
                )
            } else if class == "RAPID" {
                ("Unimplemented", if matches!(label, "write" | "start-resumable") {
                    "gcs-mimic does not support gRPC unsupported metadata change: storage_class=RAPID: storage_class requires a zonal bucket; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
                } else {
                    "gcs-mimic does not support bucket API setting storage_class RAPID requires a zonal bucket; omit this setting (for example, create a bucket with only name)."
                }.to_owned())
            } else {
                ("InvalidArgument", match label {
                    "write" | "start-resumable" => "Invalid object storage_class; use STANDARD, NEARLINE, COLDLINE or ARCHIVE".into(),
                    "bucket-update" => format!("Invalid storage class \"{class}\""),
                    _ => "The storage class you specified is not valid.".into(),
                })
            };
            expected.push(json!([label, class, code, message]));
        }
    }
    assert_eq!(snapshots, expected);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_resumes_durable_offsets_and_queries_completion() {
    let (dir, storage) = setup(false).await;
    let upload = storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(spec("resume")),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner()
        .upload_id;
    let mut first = message("ignored", &granule());
    first.finish_write = false;
    first.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload.clone(),
    ));
    assert!(matches!(
        write(&storage, vec![first.clone()]).await.unwrap(),
        pb::bidi_write_object_response::WriteStatus::PersistedSize(GRANULE)
    ));
    let query = || {
        Request::new(pb::QueryWriteStatusRequest {
            upload_id: upload.clone(),
            ..Default::default()
        })
    };
    assert!(matches!(
        storage
            .query_write_status(query(), None)
            .await
            .unwrap()
            .into_inner()
            .write_status,
        Some(pb::query_write_status_response::WriteStatus::PersistedSize(
            GRANULE
        ))
    ));
    storage.state.catalog.close().await.unwrap();
    let mut storage = storage;
    Arc::get_mut(&mut storage.state).unwrap().catalog = Catalog::open(&dir.0).await.unwrap();
    assert_code(write(&storage, vec![first]).await, tonic::Code::OutOfRange);
    let mut final_message = message("ignored", b"def");
    final_message.write_offset = GRANULE;
    final_message.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload.clone(),
    ));
    let result = write(&storage, vec![final_message]).await.unwrap();
    let pb::bidi_write_object_response::WriteStatus::Resource(object) = result else {
        panic!("not complete")
    };
    assert_eq!(object.size, GRANULE + 3);
    assert_eq!(
        object.checksums.unwrap().crc32c,
        Some(crc32c::crc32c(&[granule().as_slice(), b"def"].concat()))
    );
    assert!(matches!(
        storage
            .query_write_status(
                Request::new(pb::QueryWriteStatusRequest {
                    upload_id: upload,
                    ..Default::default()
                }),
                None
            )
            .await
            .unwrap()
            .into_inner()
            .write_status,
        Some(pb::query_write_status_response::WriteStatus::Resource(_))
    ));
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_checksums_conditions_and_generation_preserve_data() {
    let (_dir, storage) = setup(false).await;
    let first = write(&storage, vec![message("object", b"original")])
        .await
        .unwrap();
    let pb::bidi_write_object_response::WriteStatus::Resource(first) = first else {
        panic!("not complete")
    };
    for checksum in [
        pb::ObjectChecksums {
            crc32c: Some(1),
            ..Default::default()
        },
        pb::ObjectChecksums {
            md5_hash: vec![0; 16],
            ..Default::default()
        },
    ] {
        let mut bad = message("object", b"bad");
        bad.object_checksums = Some(checksum);
        assert_code(
            write(&storage, vec![bad]).await,
            tonic::Code::InvalidArgument,
        );
        assert_eq!(
            storage
                .state
                .catalog
                .get_object(live("grpc-io", "object"), Default::default(), PERMISSIVE)
                .await
                .unwrap()
                .generation,
            first.generation
        );
    }
    let mut absent = spec("atomic");
    absent.if_generation_match = Some(0);
    let upload = storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(absent),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner()
        .upload_id;
    write(&storage, vec![message("atomic", b"winner")])
        .await
        .unwrap();
    let mut losing = message("ignored", b"loser");
    losing.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload,
    ));
    assert_code(
        write(&storage, vec![losing]).await,
        tonic::Code::FailedPrecondition,
    );
    write(&storage, vec![message("object", b"new")])
        .await
        .unwrap();
    let get = pb::GetObjectRequest {
        bucket: "projects/_/buckets/grpc-io".into(),
        object: "object".into(),
        generation: first.generation,
        read_mask: fixtures::mask(&["generation", "size"]),
        ..Default::default()
    };
    let selected = storage
        .get_object(Request::new(get.clone()), None)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(selected.generation, first.generation);
    assert_eq!(selected.size, 8);
    assert!(selected.name.is_empty());
    let mut reader = storage
        .read_object(
            Request::new(pb::ReadObjectRequest {
                bucket: get.bucket.clone(),
                object: get.object.clone(),
                generation: first.generation,
                read_offset: -5,
                read_limit: 3,
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner();
    let response = reader.next().await.unwrap().unwrap();
    let data = response.checksummed_data.unwrap();
    assert_eq!(data.content, b"gin".as_slice());
    assert_eq!(data.crc32c, Some(crc32c::crc32c(b"gin")));
    assert_eq!(
        response.object_checksums.unwrap().crc32c,
        Some(crc32c::crc32c(b"original"))
    );
    storage
        .delete_object(
            Request::new(pb::DeleteObjectRequest {
                bucket: get.bucket.clone(),
                object: get.object.clone(),
                generation: first.generation,
                if_generation_match: Some(first.generation),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
    assert_code(
        storage.get_object(Request::new(get), None).await,
        tonic::Code::NotFound,
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_delete_ignores_a_customer_key_as_cloud_storage_does() {
    let (_dir, storage) = setup(false).await;
    write(&storage, vec![message("keyed-delete", b"data")])
        .await
        .unwrap();
    // A key for an object that has none: Cloud Storage still deletes it.
    storage
        .delete_object(
            Request::new(pb::DeleteObjectRequest {
                bucket: "projects/_/buckets/grpc-io".into(),
                object: "keyed-delete".into(),
                common_object_request_params: fixtures::key_params(&[7; 32]),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
    assert!(
        storage
            .state
            .catalog
            .get_object(
                live("grpc-io", "keyed-delete"),
                Default::default(),
                PERMISSIVE
            )
            .await
            .is_err()
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_strict_uses_shared_permissions_and_rejects_unsupported_values() {
    let (_dir, storage) = setup(true).await;
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", "Bearer admin".parse().unwrap());
    let admin = storage.authenticate(&request).await.unwrap();
    storage
        .receive_write(
            &mut tonic::codegen::tokio_stream::iter([Ok(message("private", b"private"))]),
            admin,
            WriteRpc::WriteObject,
        )
        .await
        .unwrap();
    request
        .metadata_mut()
        .insert("authorization", "Bearer reader".parse().unwrap());
    let reader = storage.authenticate(&request).await.unwrap();
    assert_code(
        storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter([Ok(message("private", b"denied"))]),
                reader,
                WriteRpc::WriteObject,
            )
            .await,
        tonic::Code::PermissionDenied,
    );
    let get = pb::GetObjectRequest {
        bucket: "projects/_/buckets/grpc-io".into(),
        object: "private".into(),
        ..Default::default()
    };
    assert_eq!(
        storage
            .get_object(Request::new(get.clone()), reader)
            .await
            .unwrap()
            .into_inner()
            .size,
        7
    );
    assert!(
        storage
            .get_object(Request::new(get), Some(Caller::Anonymous))
            .await
            .is_err()
    );
    // No protocol models Cloud KMS keys or object contexts, so every write path refuses them
    // before it stages anything.
    type UnsupportedResource = (&'static str, fn(&mut pb::Object));
    let unsupported: [UnsupportedResource; 2] = [
        ("kms", |resource| resource.kms_key = "key".into()),
        ("contexts", |resource| {
            resource.contexts = Some(pb::ObjectContexts {
                custom: [(
                    "k".into(),
                    pb::ObjectCustomContextPayload {
                        value: "v".into(),
                        ..Default::default()
                    },
                )]
                .into(),
            })
        }),
    ];
    const REFUSED: &str = "write_object_spec.resource fields";
    for (name, set) in unsupported {
        let mut unsupported_spec = spec(name);
        set(unsupported_spec.resource.as_mut().unwrap());
        assert_unsupported(
            storage
                .write_spec(
                    unsupported_spec.clone(),
                    &DeclaredChecksums::default(),
                    admin,
                )
                .await,
            REFUSED,
        );
        let mut first = message(name, b"data");
        first.first_message = Some(
            pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(unsupported_spec.clone()),
        );
        assert_unsupported(write_as(&storage, vec![first], admin).await, REFUSED);
        assert!(missing(&storage, name).await, "{name}");
        assert_unsupported(
            storage
                .start_resumable_write(
                    Request::new(pb::StartResumableWriteRequest {
                        write_object_spec: Some(unsupported_spec),
                        ..Default::default()
                    }),
                    admin,
                )
                .await,
            REFUSED,
        );
    }
    storage.state.catalog.close().await.unwrap();
}

async fn start(storage: &Storage, name: &str, caller: Option<Caller<'_>>) -> String {
    storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(spec(name)),
                ..Default::default()
            }),
            caller,
        )
        .await
        .unwrap()
        .into_inner()
        .upload_id
}

async fn cancel(
    storage: &Storage,
    upload_id: &str,
    caller: Option<Caller<'_>>,
) -> Result<(), tonic::Code> {
    storage
        .cancel_resumable_write(
            Request::new(pb::CancelResumableWriteRequest {
                upload_id: upload_id.into(),
            }),
            caller,
        )
        .await
        .map(|_| ())
        .map_err(|status| status.code())
}

async fn query(
    storage: &Storage,
    upload_id: &str,
    caller: Option<Caller<'_>>,
) -> Result<pb::query_write_status_response::WriteStatus, tonic::Code> {
    storage
        .query_write_status(
            Request::new(pb::QueryWriteStatusRequest {
                upload_id: upload_id.into(),
                ..Default::default()
            }),
            caller,
        )
        .await
        .map(|response| response.into_inner().write_status.unwrap())
        .map_err(|status| status.code())
}

/// The size of one write granule: a resumable stream persists whole granules until
/// finish_write (recorded 2026-11-18).
const GRANULE: i64 = 256 * 1024;

/// One granule of data, the least a resumable stream persists without finish_write.
fn granule() -> Vec<u8> {
    vec![b'a'; 256 * 1024]
}

fn resume(upload_id: &str, offset: i64, data: &[u8], finish: bool) -> pb::BidiWriteObjectRequest {
    let mut next = message("ignored", data);
    next.write_offset = offset;
    next.finish_write = finish;
    next.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload_id.into(),
    ));
    next
}

async fn missing(storage: &Storage, name: &str) -> bool {
    storage
        .state
        .catalog
        .get_object(live("grpc-io", name), Default::default(), PERMISSIVE)
        .await
        .is_err_and(|error| error == ObjectError::ObjectNotFound)
}

#[tokio::test]
async fn grpc_io_cancel_discards_sessions_and_staged_bytes() {
    let (dir, storage) = setup(false).await;
    let fresh = start(&storage, "fresh", None).await;
    assert_eq!(cancel(&storage, &fresh, None).await, Ok(()));
    // Recorded 2026-11-18: a cancelled upload answers QueryWriteStatus with FAILED_PRECONDITION
    // and a finishing resumed write with INVALID_ARGUMENT, and cancelling it again succeeds.
    // Not recorded: a resumed write without finish_write, answered alike because the session
    // is looked up before the message is read.
    assert_eq!(
        query(&storage, &fresh, None).await,
        Err(tonic::Code::FailedPrecondition)
    );
    for finish in [false, true] {
        assert_code(
            write(&storage, vec![resume(&fresh, 0, b"abc", finish)]).await,
            tonic::Code::InvalidArgument,
        );
    }
    assert_eq!(cancel(&storage, &fresh, None).await, Ok(()));

    let partial = start(&storage, "partial", None).await;
    assert!(matches!(
        write(&storage, vec![resume(&partial, 0, &granule(), false)])
            .await
            .unwrap(),
        pb::bidi_write_object_response::WriteStatus::PersistedSize(GRANULE)
    ));
    let partial_id = partial.parse::<UploadId>().unwrap();
    let blob = storage
        .state
        .catalog
        .resumable_session(partial_id.session(), partial_id.bucket())
        .await
        .unwrap()
        .blob_id;
    let staged = blob.staging_path(&dir.0);
    assert!(staged.exists());
    assert_eq!(cancel(&storage, &partial, None).await, Ok(()));
    assert!(!staged.exists());
    assert!(!blob.blob_path(&dir.0).exists());
    assert_code(
        write(&storage, vec![resume(&partial, GRANULE, b"def", true)]).await,
        tonic::Code::InvalidArgument,
    );
    for name in ["fresh", "partial"] {
        assert!(missing(&storage, name).await);
    }

    let done = start(&storage, "done", None).await;
    write(&storage, vec![resume(&done, 0, b"done", true)])
        .await
        .unwrap();
    assert_eq!(
        cancel(&storage, &done, None).await,
        Err(tonic::Code::FailedPrecondition)
    );
    assert!(matches!(
        query(&storage, &done, None).await,
        Ok(pb::query_write_status_response::WriteStatus::Resource(_))
    ));
    assert!(!missing(&storage, "done").await);

    // Recorded 2026-11-18: text that is no upload ID is INVALID_ARGUMENT, and an ID the service
    // cannot read fails as INTERNAL.
    for (id, code) in [
        ("grpc-io/not-hex", tonic::Code::Internal),
        ("", tonic::Code::InvalidArgument),
        ("grpc-io", tonic::Code::InvalidArgument),
        (
            "/0000000000000000000000000000000000000000000000000000000000000000",
            tonic::Code::InvalidArgument,
        ),
    ] {
        assert_eq!(cancel(&storage, id, None).await, Err(code), "{id}");
        assert_eq!(query(&storage, id, None).await, Err(code), "{id}");
    }
    // Not recorded; the emulator's departure from the INTERNAL above: an ID of the issued shape
    // that names no session is NOT_FOUND, which SDKs do not retry.
    let unknown = format!("grpc-io/{}", "0".repeat(64));
    assert_eq!(
        cancel(&storage, &unknown, None).await,
        Err(tonic::Code::NotFound)
    );
    assert_eq!(
        query(&storage, &unknown, None).await,
        Err(tonic::Code::NotFound)
    );
    // An upload_id is scoped to its bucket; reusing it under another bucket finds nothing.
    let scoped = start(&storage, "scoped", None).await;
    let scoped_id = scoped.parse::<UploadId>().unwrap();
    let id = scoped_id.session();
    assert_eq!(
        cancel(&storage, &format!("other-bucket/{id}"), None).await,
        Err(tonic::Code::NotFound)
    );
    assert!(query(&storage, &scoped, None).await.is_ok());

    // An in-flight writer holds the session lease; cancellation reports a retryable
    // conflict and leaves the session intact instead of racing the writer.
    let lease = storage
        .state
        .catalog
        .lock_resumable_session(id, "grpc-io")
        .await
        .unwrap();
    assert_eq!(
        cancel(&storage, &scoped, None).await,
        Err(tonic::Code::Unavailable)
    );
    drop(lease);
    assert_eq!(cancel(&storage, &scoped, None).await, Ok(()));
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_strict_cancel_requires_create_permission() {
    let (_dir, storage) = setup(true).await;
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", "Bearer admin".parse().unwrap());
    let admin = storage.authenticate(&request).await.unwrap();
    request
        .metadata_mut()
        .insert("authorization", "Bearer reader".parse().unwrap());
    let reader = storage.authenticate(&request).await.unwrap();
    let upload = start(&storage, "strict", admin).await;
    storage
        .receive_write(
            &mut tonic::codegen::tokio_stream::iter([Ok(resume(&upload, 0, &granule(), false))]),
            admin,
            WriteRpc::WriteObject,
        )
        .await
        .unwrap();
    assert_eq!(
        cancel(&storage, &upload, reader).await,
        Err(tonic::Code::PermissionDenied)
    );
    assert_eq!(
        cancel(&storage, &upload, Some(Caller::Anonymous)).await,
        Err(tonic::Code::Unauthenticated)
    );
    assert!(matches!(
        query(&storage, &upload, admin).await,
        Ok(pb::query_write_status_response::WriteStatus::PersistedSize(
            GRANULE
        ))
    ));
    assert_eq!(cancel(&storage, &upload, admin).await, Ok(()));
    assert_eq!(
        query(&storage, &upload, admin).await,
        Err(tonic::Code::FailedPrecondition)
    );
    // The upload_id binds no creator: a different caller that holds storage.objects.create on
    // the bucket cancels a session the admin started.
    request
        .metadata_mut()
        .insert("authorization", "Bearer user".parse().unwrap());
    let user = storage.authenticate(&request).await.unwrap();
    let started_by_admin = start(&storage, "strict-other", admin).await;
    assert_eq!(cancel(&storage, &started_by_admin, user).await, Ok(()));
    assert_eq!(
        query(&storage, &started_by_admin, admin).await,
        Err(tonic::Code::FailedPrecondition)
    );
    storage.state.catalog.close().await.unwrap();
}

/// A session lives seven days from StartResumableWrite on the catalog clock. Past that, every
/// call that names its upload_id answers as for an upload_id that names no session: NOT_FOUND.
/// Not recorded; the emulator departs from the INTERNAL Cloud Storage gave IDs it could not
/// read, which SDKs would retry.
#[tokio::test]
async fn grpc_io_resumable_sessions_expire_seven_days_after_start() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let dir = TestDir::new();
    // The catalog stores the start in whole milliseconds; a start that has no fraction puts the
    // deadline exactly seven days later, so the test pins the boundary itself.
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let start_time = UNIX_EPOCH + Duration::from_secs(since_epoch.as_secs());
    let clock = Arc::new(crate::clock::ManualClock::new(start_time));
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    let storage = storage_over(catalog, false).await;
    let upload = start(&storage, "expiring", None).await;
    assert!(matches!(
        write(&storage, vec![resume(&upload, 0, &granule(), false)])
            .await
            .unwrap(),
        pb::bidi_write_object_response::WriteStatus::PersistedSize(GRANULE)
    ));
    let lifetime = Duration::from_secs(7 * 24 * 60 * 60);
    clock.advance(lifetime - Duration::from_millis(1));
    assert!(matches!(
        query(&storage, &upload, None).await,
        Ok(pb::query_write_status_response::WriteStatus::PersistedSize(
            GRANULE
        ))
    ));
    clock.advance(Duration::from_millis(1));
    assert_eq!(
        query(&storage, &upload, None).await,
        Err(tonic::Code::NotFound)
    );
    let refused = assert_code(
        write(&storage, vec![resume(&upload, GRANULE, b"def", true)]).await,
        tonic::Code::NotFound,
    );
    assert_eq!(refused.message(), UPLOAD_NOT_FOUND);
    assert_eq!(
        cancel(&storage, &upload, None).await,
        Err(tonic::Code::NotFound)
    );
    assert!(missing(&storage, "expiring").await);
    storage.state.catalog.close().await.unwrap();
}

/// An upload ID of the issued shape names its session only in the catalog, which runs after the
/// permission check: a caller without access is denied whatever the ID's case, and one with
/// access gets the answer for an ID that names no session (NOT_FOUND; not recorded).
#[tokio::test]
async fn grpc_io_upload_ids_are_looked_up_after_the_permission_check() {
    let (_dir, storage) = setup(true).await;
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", "Bearer admin".parse().unwrap());
    let admin = storage.authenticate(&request).await.unwrap();
    request
        .metadata_mut()
        .insert("authorization", "Bearer reader".parse().unwrap());
    let reader = storage.authenticate(&request).await.unwrap();
    let upload = format!("grpc-io/{}", "AB".repeat(32));
    let status = |caller| {
        let storage = &storage;
        let upload = upload.as_str();
        async move {
            let query = storage
                .query_write_status(
                    Request::new(pb::QueryWriteStatusRequest {
                        upload_id: upload.into(),
                        ..Default::default()
                    }),
                    caller,
                )
                .await
                .unwrap_err();
            let cancel = storage
                .cancel_resumable_write(
                    Request::new(pb::CancelResumableWriteRequest {
                        upload_id: upload.into(),
                    }),
                    caller,
                )
                .await
                .unwrap_err();
            let resumed = write_as(storage, vec![resume(upload, 0, b"abc", true)], caller)
                .await
                .unwrap_err();
            [query, cancel, resumed].map(|status| (status.code(), status.message().to_owned()))
        }
    };
    for (code, _) in status(reader).await {
        assert_eq!(code, tonic::Code::PermissionDenied);
    }
    for (code, _) in status(Some(Caller::Anonymous)).await {
        assert_eq!(code, tonic::Code::Unauthenticated);
    }
    for (code, message) in status(admin).await {
        assert_eq!(code, tonic::Code::NotFound);
        assert_eq!(message, UPLOAD_NOT_FOUND);
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_resumable_checksums_survive_reconnect_and_failed_completion() {
    let (dir, mut storage) = setup(false).await;
    let mut hasher = crate::hashing::Hasher::default();
    hasher.update(&granule());
    hasher.update(b"def");
    let correct = hasher.finish();
    for (name, checksum) in [
        (
            "resume-crc",
            pb::ObjectChecksums {
                crc32c: Some(correct.crc32c),
                ..Default::default()
            },
        ),
        (
            "resume-md5",
            pb::ObjectChecksums {
                md5_hash: correct.md5.to_vec(),
                ..Default::default()
            },
        ),
    ] {
        let upload = storage
            .start_resumable_write(
                Request::new(pb::StartResumableWriteRequest {
                    write_object_spec: Some(spec(name)),
                    object_checksums: Some(checksum.clone()),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner()
            .upload_id;
        // Recorded 2026-11-18: object_checksums is accepted on a message that does not finish
        // the upload; here it repeats the checksum the session started with.
        let mut prefix = message("ignored", &granule());
        prefix.finish_write = false;
        prefix.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload.clone(),
        ));
        prefix.object_checksums = Some(checksum);
        assert!(matches!(
            write(&storage, vec![prefix]).await.unwrap(),
            pb::bidi_write_object_response::WriteStatus::PersistedSize(GRANULE)
        ));
        storage.state.catalog.close().await.unwrap();
        Arc::get_mut(&mut storage.state).unwrap().catalog = Catalog::open(&dir.0).await.unwrap();
        let mut bad = message("ignored", b"xxx");
        bad.write_offset = GRANULE;
        bad.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload.clone(),
        ));
        assert_code(
            write(&storage, vec![bad.clone()]).await,
            tonic::Code::InvalidArgument,
        );
        assert_eq!(
            storage
                .state
                .catalog
                .get_object(live("grpc-io", name), Default::default(), PERMISSIVE)
                .await
                .unwrap_err(),
            ObjectError::ObjectNotFound
        );
        assert_code(
            storage
                .query_write_status(
                    Request::new(pb::QueryWriteStatusRequest {
                        upload_id: upload,
                        ..Default::default()
                    }),
                    None,
                )
                .await,
            tonic::Code::FailedPrecondition,
        );
        assert_code(
            write(&storage, vec![bad]).await,
            tonic::Code::InvalidArgument,
        );
    }
    let upload = storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(spec("final-crc")),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner()
        .upload_id;
    let mut prefix = message("ignored", &granule());
    prefix.finish_write = false;
    prefix.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload.clone(),
    ));
    write(&storage, vec![prefix]).await.unwrap();
    let mut bad = message("ignored", b"def");
    bad.write_offset = GRANULE;
    bad.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload,
    ));
    bad.object_checksums = Some(pb::ObjectChecksums {
        crc32c: Some(1),
        ..Default::default()
    });
    assert_code(
        write(&storage, vec![bad]).await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(
        storage
            .state
            .catalog
            .get_object(live("grpc-io", "final-crc"), Default::default(), PERMISSIVE)
            .await
            .unwrap_err(),
        ObjectError::ObjectNotFound
    );
    storage.state.catalog.close().await.unwrap();
}

/// Recorded 2026-11-18: three bytes sent to a session without finish_write are not persisted.
/// Closing the stream there fails, QueryWriteStatus still reports 0, and a stream that resumes
/// past 0 is refused with both offsets.
#[tokio::test]
async fn grpc_io_resumable_close_drops_bytes_past_the_last_granule() {
    let (_dir, storage) = setup(false).await;
    let upload = start(&storage, "granules", None).await;
    let dropped = "Exactly 3 bytes were uploaded in this call, which is not a multiple of the 262144 byte write granularity, but finish_write was not set. Some trailing data was not written. Call 'QueryWriteStatus' to see exactly how much data has been recorded.";
    let closed = assert_code(
        write(&storage, vec![resume(&upload, 0, b"hel", false)]).await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(closed.message(), dropped);
    assert!(closed.details().is_empty());
    assert_eq!(
        query(&storage, &upload, None).await,
        Ok(pb::query_write_status_response::WriteStatus::PersistedSize(
            0
        ))
    );
    let past = assert_code(
        write(&storage, vec![resume(&upload, 3, b"lo", true)]).await,
        tonic::Code::OutOfRange,
    );
    assert_eq!(
        past.message(),
        "Upload request started at offset '3', which is past expected offset '0'."
    );
    fixtures::assert_reason(&past, "GRPC_UPLOAD_INVALID_START_OFFSET");

    // Over BidiWriteObject the flush is answered first, with the CRC32C of the persisted
    // bytes and no MD5, and the close then fails the same way.
    let mut flushed = resume(&upload, 0, b"hel", false);
    flushed.flush = true;
    flushed.state_lookup = true;
    let (tx, mut rx) = mpsc::channel(4);
    let closed = assert_code(
        storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter([Ok(flushed)]),
                None,
                WriteRpc::Bidi(&tx),
            )
            .await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(closed.message(), dropped);
    let nothing_persisted = pb::BidiWriteObjectResponse {
        write_status: Some(pb::bidi_write_object_response::WriteStatus::PersistedSize(
            0,
        )),
        persisted_data_checksums: Some(pb::ObjectChecksums {
            crc32c: Some(0),
            md5_hash: Vec::new(),
        }),
        write_handle: None,
    };
    assert_eq!(rx.recv().await.unwrap().unwrap(), nothing_persisted);
    // A lookup without data is answered, and so is the close after it.
    let mut lookup = resume(&upload, 0, b"", false);
    lookup.state_lookup = true;
    storage
        .receive_write(
            &mut tonic::codegen::tokio_stream::iter([Ok(lookup)]),
            None,
            WriteRpc::Bidi(&tx),
        )
        .await
        .unwrap();
    drop(tx);
    for _ in 0..2 {
        assert_eq!(rx.recv().await.unwrap().unwrap(), nothing_persisted);
    }
    assert!(rx.recv().await.is_none());

    // The bytes that wait for a whole granule are persisted once later messages complete it.
    let mut rest = resume(&upload, 3, &granule(), false);
    rest.first_message = None;
    let closed = assert_code(
        write(&storage, vec![resume(&upload, 0, b"hel", false), rest]).await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(
        closed.message(),
        dropped.replace("Exactly 3 bytes", "Exactly 262147 bytes")
    );
    assert_eq!(
        query(&storage, &upload, None).await,
        Ok(pb::query_write_status_response::WriteStatus::PersistedSize(
            GRANULE
        ))
    );
    storage.state.catalog.close().await.unwrap();
}

/// Starts a session for `name` that declares five bytes and finishes it with three, which
/// Cloud Storage refused as recorded 2026-11-18, ending the session.
async fn terminated_session(storage: &Storage, name: &str) -> String {
    let mut sized = spec(name);
    sized.object_size = Some(5);
    let terminated = storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(sized),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner()
        .upload_id;
    let mismatch = assert_code(
        write(storage, vec![resume(&terminated, 0, b"abc", true)]).await,
        tonic::Code::OutOfRange,
    );
    assert_eq!(
        mismatch.message(),
        "Invalid request. The final size of the upload is 3 byte(s). This does not match the expected size of 5 byte(s), which was specified in the StartResumableWrite request."
    );
    fixtures::assert_reason(&mismatch, "GRPC_MISMATCHED_UPLOAD_SIZE");
    terminated
}

/// Asserts the FAILED_PRECONDITION that QueryWriteStatus answered a terminated session with.
#[track_caller]
fn assert_terminated(status: &Status) {
    assert_eq!(
        (status.code(), status.message()),
        (
            tonic::Code::FailedPrecondition,
            "Upload has already been terminated."
        )
    );
    fixtures::assert_reason(status, "GRPC_UPLOAD_ALREADY_TERMINATED");
}

/// Recorded 2026-11-18: how a session that ended answers. A cancelled one fails
/// QueryWriteStatus with FAILED_PRECONDITION and a write with INVALID_ARGUMENT, and cancelling
/// it again succeeds; a finalized one refuses cancellation and still reports its resource, with
/// the caller's ACL and owner view; a finishing message below object_size ends the session,
/// and QueryWriteStatus then fails with FAILED_PRECONDITION.
#[tokio::test]
async fn grpc_io_ended_sessions_answer_as_recorded() {
    let (_dir, storage) = setup(false).await;
    let cancelled = start(&storage, "ended-cancelled", None).await;
    assert_eq!(cancel(&storage, &cancelled, None).await, Ok(()));
    assert_eq!(cancel(&storage, &cancelled, None).await, Ok(()));
    let queried = storage
        .query_write_status(
            Request::new(pb::QueryWriteStatusRequest {
                upload_id: cancelled.clone(),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        (queried.code(), queried.message()),
        (
            tonic::Code::FailedPrecondition,
            "Upload has already been cancelled."
        )
    );
    fixtures::assert_reason(&queried, "GRPC_UPLOAD_ALREADY_CANCELLED");
    let written = assert_code(
        write(&storage, vec![resume(&cancelled, 0, b"hello", true)]).await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(written.message(), "Upload has already been cancelled.");
    assert!(written.details().is_empty());

    let finalized = start(&storage, "ended-finalized", None).await;
    write(&storage, vec![resume(&finalized, 0, b"hello", true)])
        .await
        .unwrap();
    let refused = storage
        .cancel_resumable_write(
            Request::new(pb::CancelResumableWriteRequest {
                upload_id: finalized.clone(),
            }),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        (refused.code(), refused.message()),
        (
            tonic::Code::FailedPrecondition,
            "Upload has already been completed."
        )
    );
    fixtures::assert_reason(&refused, "GRPC_UPLOAD_ALREADY_COMPLETED");
    let Ok(pb::query_write_status_response::WriteStatus::Resource(resource)) =
        query(&storage, &finalized, None).await
    else {
        panic!("a finalized upload reports its resource")
    };
    assert!(!resource.acl.is_empty());
    assert!(resource.owner.is_some());

    let terminated = terminated_session(&storage, "ended-terminated").await;
    let queried = storage
        .query_write_status(
            Request::new(pb::QueryWriteStatusRequest {
                upload_id: terminated,
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap_err();
    assert_terminated(&queried);
    assert!(missing(&storage, "ended-terminated").await);
    storage.state.catalog.close().await.unwrap();
}

/// Not recorded; inferred. On a session that a finishing message below object_size ended,
/// CancelResumableWrite answers as QueryWriteStatus was recorded answering it, and a write
/// answers as a write to a cancelled session was recorded answering, in the terminated wording.
/// The session stays ended and publishes nothing.
#[tokio::test]
async fn grpc_io_terminated_sessions_refuse_cancels_and_writes() {
    let (_dir, storage) = setup(false).await;
    let terminated = terminated_session(&storage, "terminated").await;
    let cancelled = storage
        .cancel_resumable_write(
            Request::new(pb::CancelResumableWriteRequest {
                upload_id: terminated.clone(),
            }),
            None,
        )
        .await
        .unwrap_err();
    assert_terminated(&cancelled);
    let written = assert_code(
        write(&storage, vec![resume(&terminated, 0, b"hello", true)]).await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(written.message(), "Upload has already been terminated.");
    assert!(written.details().is_empty());
    assert_eq!(
        query(&storage, &terminated, None).await,
        Err(tonic::Code::FailedPrecondition)
    );
    assert!(missing(&storage, "terminated").await);
    storage.state.catalog.close().await.unwrap();
}
