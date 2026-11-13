use super::*;
use super::{
    read::read_range,
    resource::mask_object,
    resumable::{Persisted, parse_upload_id, resumable_piece},
    write::{EndOfStream, MessagePosition},
};
use crate::grpc::test_support::{self as fixtures, assert_code, registry};
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::{auth::Auth, storage::Catalog, test_support::TestDir};
use serde_json::json;
use std::sync::Arc;

#[test]
fn resumable_message_end_must_fit_the_wire_offset() {
    let max = i64::MAX.cast_unsigned();
    for (offset, accepted) in [(i64::MAX - 1, true), (i64::MAX, false)] {
        let mut message = pb::BidiWriteObjectRequest {
            write_offset: offset,
            data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
                pb::ChecksummedData {
                    content: bytes::Bytes::from_static(b"x"),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let result = resumable_piece(
            &mut message,
            Persisted {
                offset: offset.cast_unsigned(),
                total: None,
            },
            MessagePosition::First,
            u64::MAX,
        );
        if accepted {
            assert_eq!(result.unwrap().written, max);
        } else {
            let error = result.err().unwrap();
            assert_eq!(error.code(), tonic::Code::OutOfRange);
            assert_eq!(
                error.message(),
                "write_offset plus content length overflows"
            );
        }
    }
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

async fn setup(strict: bool) -> (TestDir, Storage) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
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
    let storage = Storage {
        compat_profile: crate::config::CompatProfile::Native,
        state: Arc::new(crate::server::State::for_test(catalog, auth)),
        max_object_bytes: 1024 * 1024,
    };
    (dir, storage)
}

fn spec(name: &str) -> pb::WriteObjectSpec {
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

fn message(name: &str, data: &[u8]) -> pb::BidiWriteObjectRequest {
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

async fn write(
    storage: &Storage,
    messages: Vec<pb::BidiWriteObjectRequest>,
) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
    write_as(storage, messages, None).await
}

async fn write_as(
    storage: &Storage,
    messages: Vec<pb::BidiWriteObjectRequest>,
    caller: Option<Caller<'_>>,
) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
    storage
        .receive_write(
            &mut tonic::codegen::tokio_stream::iter(messages.into_iter().map(Ok)),
            caller,
            None,
            EndOfStream::Required,
        )
        .await
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
    let mut first = resume(&upload, 0, b"abc", false);
    first.flush = true;
    let (requests, incoming) = mpsc::channel(1);
    requests.send(Ok(first)).await.unwrap();
    let (responses, mut acknowledgements) =
        mpsc::channel::<Result<pb::BidiWriteObjectResponse, Status>>(1);
    let later = async {
        acknowledgements.recv().await.unwrap().unwrap();
        write(&storage, vec![resume(&upload, 3, b"winner", true)])
            .await
            .unwrap();
        let mut next = resume(&upload, 3, b"discarded", true);
        next.first_message = None;
        requests.send(Ok(next)).await.unwrap();
        drop(requests);
    };
    let mut stream = ReceiverStream::new(incoming);
    let (result, ()) = tokio::join!(
        storage.receive_write(&mut stream, None, Some(&responses), EndOfStream::Required),
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
    assert_eq!(record.size, 9);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_bidi_half_close_reports_unacknowledged_persisted_size() {
    let (_dir, storage) = setup(false).await;
    for (name, data, flush) in [
        ("half-close", b"abc".as_slice(), false),
        ("flushed", b"abc".as_slice(), true),
        ("empty", b"".as_slice(), false),
    ] {
        let upload = start(&storage, name, None).await;
        let mut message = resume(&upload, 0, data, false);
        message.flush = flush;
        let mut stream = tonic::codegen::tokio_stream::iter([Ok(message)]);
        let (tx, mut rx) = mpsc::channel(2);
        let result = storage
            .receive_write(&mut stream, None, Some(&tx), EndOfStream::NotChecked)
            .await
            .unwrap();
        assert_eq!(
            result,
            pb::bidi_write_object_response::WriteStatus::PersistedSize(data.len() as i64)
        );
        drop(tx);
        let response = rx.recv().await.unwrap().unwrap();
        assert_eq!(response.write_status, Some(result));
        assert!(rx.recv().await.is_none());
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
    let (status, _): (_, Vec<pb::BidiReadObjectResponse>) = fixtures::rpc_status(
        "/google.storage.v2.Storage/BidiReadObject",
        None,
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
        Ok(resume(&upload, 0, b"abc", false)),
        Err(Status::cancelled("client disconnected")),
    ]);
    assert_code(
        storage
            .receive_write(&mut stream, None, None, EndOfStream::Required)
            .await,
        tonic::Code::Cancelled,
    );
    assert_eq!(
        query(&storage, &upload, None).await.unwrap(),
        pb::query_write_status_response::WriteStatus::PersistedSize(3)
    );
    write(&storage, vec![resume(&upload, 3, b"def", true)])
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
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
    let mismatch = verify_crc32c(&pb::ChecksummedData {
        content: b"hello".to_vec().into(),
        crc32c: Some(1),
    })
    .unwrap_err();
    assert_eq!(mismatch.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        mismatch.message(),
        "Checksummed data message crc32c mismatch: server computed: '9a71bb4c', client provided '00000001'."
    );
    let unknown = mask_object(
        pb::Object::default(),
        fixtures::mask(&["noSuchField"]).as_ref(),
    )
    .unwrap_err();
    assert_eq!(unknown.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        unknown.message(),
        "There is no field for this resource named 'noSuchField'."
    );
    assert_eq!(read_range(15, -5, 3).unwrap(), (10, 3));
    assert_eq!(read_range(15, i64::MIN, 0).unwrap(), (0, 15));
    assert_eq!(read_range(15, 15, 0).unwrap(), (15, 0));
    assert_code(read_range(15, 16, 0), tonic::Code::OutOfRange);
    assert_code(read_range(15, 0, -1), tonic::Code::InvalidArgument);
    let object = mask_object(
        pb::Object {
            name: "a".into(),
            size: 12,
            ..Default::default()
        },
        fixtures::mask(&["name"]).as_ref(),
    )
    .unwrap();
    assert_eq!(object.name, "a");
    assert_eq!(object.size, 0);
    assert_code(
        mask_object(
            pb::Object::default(),
            fixtures::mask(&["owner.entity"]).as_ref(),
        ),
        tonic::Code::Unimplemented,
    );
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
    storage
        .state
        .catalog
        .patch_bucket(
            "grpc-io",
            crate::bucket::BucketMetadataUpdate {
                versioning_enabled: Some(false),
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
            None,
            EndOfStream::Required,
        )
        .await
        .unwrap();
    let pb::bidi_write_object_response::WriteStatus::Resource(resource) = written else {
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
            None,
            EndOfStream::Required,
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
    assert_code(
        get(fixtures::mask(&["owner.entity"]), admin).await,
        tonic::Code::Unimplemented,
    );
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
        ("REGIONAL", tonic::Code::Unimplemented),
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
    for class in ["REGIONAL", "standard", "UNKNOWN"] {
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
    for class in ["REGIONAL", "standard", "UNKNOWN"] {
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
            } else if class == "REGIONAL" {
                ("Unimplemented", if matches!(label, "write" | "start-resumable") {
                    "gcs-mimic does not support gRPC unsupported metadata change: storage_class=REGIONAL: storage_class requires a region or zone; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
                } else {
                    "gcs-mimic does not support bucket API setting storage_class requires a region or zone; omit this setting (for example, create a bucket with only name)."
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
    let mut first = message("ignored", b"abc");
    first.finish_write = false;
    first.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload.clone(),
    ));
    assert!(matches!(
        write(&storage, vec![first.clone()]).await.unwrap(),
        pb::bidi_write_object_response::WriteStatus::PersistedSize(3)
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
            3
        ))
    ));
    storage.state.catalog.close().await.unwrap();
    let mut storage = storage;
    Arc::get_mut(&mut storage.state).unwrap().catalog = Catalog::open(&dir.0).await.unwrap();
    assert_code(write(&storage, vec![first]).await, tonic::Code::OutOfRange);
    let mut final_message = message("ignored", b"def");
    final_message.write_offset = 3;
    final_message.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload.clone(),
    ));
    let result = write(&storage, vec![final_message]).await.unwrap();
    let pb::bidi_write_object_response::WriteStatus::Resource(object) = result else {
        panic!("not complete")
    };
    assert_eq!(object.size, 6);
    assert_eq!(
        object.checksums.unwrap().crc32c,
        Some(crc32c::crc32c(b"abcdef"))
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
            None,
            EndOfStream::Required,
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
                None,
                EndOfStream::Required,
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
    let mut unsupported_spec = spec("kms");
    unsupported_spec.resource.as_mut().unwrap().kms_key = "key".into();
    assert_code(
        storage.write_spec(unsupported_spec, None, admin).await,
        tonic::Code::Unimplemented,
    );
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
    assert_eq!(
        query(&storage, &fresh, None).await,
        Err(tonic::Code::NotFound)
    );
    for finish in [false, true] {
        assert_code(
            write(&storage, vec![resume(&fresh, 0, b"abc", finish)]).await,
            tonic::Code::NotFound,
        );
    }
    // Repeating a successful cancellation reports the session as gone rather than
    // succeeding again, so callers cannot mistake it for a still-live upload.
    assert_eq!(
        cancel(&storage, &fresh, None).await,
        Err(tonic::Code::NotFound)
    );

    let partial = start(&storage, "partial", None).await;
    assert!(matches!(
        write(&storage, vec![resume(&partial, 0, b"abc", false)])
            .await
            .unwrap(),
        pb::bidi_write_object_response::WriteStatus::PersistedSize(3)
    ));
    let (bucket, id) = parse_upload_id(&partial).unwrap();
    let blob = storage
        .state
        .catalog
        .resumable_session(id, bucket)
        .await
        .unwrap()
        .blob_id;
    let staged = blob.staging_path(&dir.0);
    assert!(staged.exists());
    assert_eq!(cancel(&storage, &partial, None).await, Ok(()));
    assert!(!staged.exists());
    assert!(!blob.blob_path(&dir.0).exists());
    assert_code(
        write(&storage, vec![resume(&partial, 3, b"def", true)]).await,
        tonic::Code::NotFound,
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

    let unknown = format!("grpc-io/{}", "0".repeat(64));
    for id in [
        unknown.as_str(),
        "",
        "grpc-io",
        "grpc-io/not-hex",
        "/0000000000000000000000000000000000000000000000000000000000000000",
    ] {
        assert_eq!(cancel(&storage, id, None).await, Err(tonic::Code::NotFound));
    }
    // An upload_id is scoped to its bucket; reusing it under another bucket finds nothing.
    let scoped = start(&storage, "scoped", None).await;
    let (_, id) = parse_upload_id(&scoped).unwrap();
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
            &mut tonic::codegen::tokio_stream::iter([Ok(resume(&upload, 0, b"abc", false))]),
            admin,
            None,
            EndOfStream::Required,
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
            3
        ))
    ));
    assert_eq!(cancel(&storage, &upload, admin).await, Ok(()));
    assert_eq!(
        query(&storage, &upload, admin).await,
        Err(tonic::Code::NotFound)
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_io_resumable_checksums_survive_reconnect_and_failed_completion() {
    let (dir, mut storage) = setup(false).await;
    let mut hasher = crate::hashing::Hasher::default();
    hasher.update(b"abcdef");
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
        let mut prefix = message("ignored", b"abc");
        prefix.finish_write = false;
        prefix.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload.clone(),
        ));
        let mut rejected = prefix.clone();
        rejected.object_checksums = Some(checksum);
        assert_code(
            write(&storage, vec![rejected]).await,
            tonic::Code::Unimplemented,
        );
        assert!(matches!(
            storage
                .query_write_status(
                    Request::new(pb::QueryWriteStatusRequest {
                        upload_id: upload.clone(),
                        ..Default::default()
                    }),
                    None
                )
                .await
                .unwrap()
                .into_inner()
                .write_status,
            Some(pb::query_write_status_response::WriteStatus::PersistedSize(
                0
            ))
        ));
        write(&storage, vec![prefix]).await.unwrap();
        storage.state.catalog.close().await.unwrap();
        Arc::get_mut(&mut storage.state).unwrap().catalog = Catalog::open(&dir.0).await.unwrap();
        let mut bad = message("ignored", b"xxx");
        bad.write_offset = 3;
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
            tonic::Code::NotFound,
        );
        assert_code(write(&storage, vec![bad]).await, tonic::Code::NotFound);
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
    let mut prefix = message("ignored", b"abc");
    prefix.finish_write = false;
    prefix.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
        upload.clone(),
    ));
    write(&storage, vec![prefix]).await.unwrap();
    let mut bad = message("ignored", b"def");
    bad.write_offset = 3;
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
