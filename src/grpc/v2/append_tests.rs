use super::*;
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    json,
    storage::Catalog,
    test_support::TestDir,
};
use bytes::Bytes;
use pb::bidi_write_object_request::{Data, FirstMessage};
use pb::bidi_write_object_response::WriteStatus;
use std::sync::{Arc, atomic::AtomicBool};
use tonic::{Code, Request};

const BUCKET: &str = "projects/_/buckets/grpc-append";

async fn setup(strict: bool) -> (TestDir, Storage) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let definition = json::parse_insert(
        br#"{"name":"grpc-append"}"#,
        "project",
        std::time::SystemTime::now(),
    )
    .unwrap();
    catalog.insert_bucket(definition).await.unwrap();
    let auth = if strict {
        Auth::strict(Registry::parse(br#"{"identities":[{"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},{"id":"reader@example.invalid","tokens":["reader"],"project_roles":{"project":["roles/storage.objectViewer"]}}]}"#).unwrap())
    } else {
        Auth::permissive()
    };
    let storage = Storage {
        state: Arc::new(crate::server::State {
            ready: AtomicBool::new(true),
            clock: Arc::new(SystemClock),
            catalog,
            api_base_url: "http://localhost:4443".into(),
            default_location: "US".into(),
            auth,
        }),
        max_object_bytes: 1024 * 1024,
    };
    (dir, storage)
}

fn data(content: &[u8]) -> Option<Data> {
    Some(Data::ChecksummedData(pb::ChecksummedData {
        content: Bytes::copy_from_slice(content),
        crc32c: Some(crc32c::crc32c(content)),
    }))
}

fn create(name: &str, content: &[u8]) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        first_message: Some(FirstMessage::WriteObjectSpec(pb::WriteObjectSpec {
            resource: Some(pb::Object {
                bucket: BUCKET.into(),
                name: name.into(),
                content_type: "text/plain".into(),
                metadata: [("key".into(), "value".into())].into(),
                ..Default::default()
            }),
            appendable: Some(true),
            ..Default::default()
        })),
        data: data(content),
        flush: true,
        state_lookup: true,
        ..Default::default()
    }
}

fn take_over(name: &str, generation: i64, handle: Option<Vec<u8>>) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        first_message: Some(FirstMessage::AppendObjectSpec(pb::AppendObjectSpec {
            bucket: BUCKET.into(),
            object: name.into(),
            generation,
            write_handle: handle.map(|handle| pb::BidiWriteHandle { handle }),
            ..Default::default()
        })),
        ..Default::default()
    }
}

fn chunk(offset: i64, content: &[u8], flush: bool) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        write_offset: offset,
        data: data(content),
        flush,
        state_lookup: flush,
        ..Default::default()
    }
}

async fn append(
    storage: &Storage,
    messages: Vec<Result<pb::BidiWriteObjectRequest, Status>>,
    caller: Option<Caller<'_>>,
) -> (Result<(), Status>, Vec<pb::BidiWriteObjectResponse>) {
    let mut messages = messages.into_iter();
    let first = messages.next().unwrap().unwrap();
    let mut stream = tonic::codegen::tokio_stream::iter(messages);
    let (tx, mut rx) = mpsc::channel(16);
    let result = storage
        .receive_append(first, &mut stream, caller, &tx)
        .await;
    drop(tx);
    let mut responses = Vec::new();
    while let Some(response) = rx.recv().await {
        responses.push(response.unwrap());
    }
    (result, responses)
}

async fn read(storage: &Storage, name: &str) -> Result<(pb::Object, Vec<u8>), Status> {
    let mut stream = storage
        .read_object(
            Request::new(pb::ReadObjectRequest {
                bucket: BUCKET.into(),
                object: name.into(),
                ..Default::default()
            }),
            None,
        )
        .await?
        .into_inner();
    let mut metadata = None;
    let mut content = Vec::new();
    while let Some(message) = stream.next().await {
        let message = message?;
        if let Some(object) = message.metadata {
            metadata = Some(object);
        }
        if let Some(data) = message.checksummed_data {
            content.extend_from_slice(&data.content);
        }
    }
    Ok((metadata.unwrap(), content))
}

async fn get(storage: &Storage, name: &str) -> Result<pb::Object, Status> {
    storage
        .get_object(
            Request::new(pb::GetObjectRequest {
                bucket: BUCKET.into(),
                object: name.into(),
                ..Default::default()
            }),
            None,
        )
        .await
        .map(tonic::Response::into_inner)
}

fn persisted(response: &pb::BidiWriteObjectResponse) -> i64 {
    match response.write_status.as_ref().unwrap() {
        WriteStatus::PersistedSize(size) => *size,
        WriteStatus::Resource(object) => object.size,
    }
}

#[tokio::test]
async fn append_creates_flushes_reads_and_finalizes_with_reserved_generation() {
    let (_dir, storage) = setup(false).await;
    let (result, responses) = append(&storage, vec![Ok(create("log", b"hello"))], None).await;
    result.unwrap();
    // The flush acknowledgement carries the unfinalized resource; the clean close repeats the size.
    assert_eq!(responses.len(), 2);
    let Some(WriteStatus::Resource(created)) = &responses[0].write_status else {
        panic!("first acknowledgement must carry the resource")
    };
    assert_eq!((created.size, created.metageneration), (5, 1));
    assert!(created.finalize_time.is_none());
    assert!(created.checksums.as_ref().unwrap().md5_hash.is_empty());
    assert_eq!(created.metadata["key"], "value");
    assert_eq!(persisted(&responses[1]), 5);
    assert_eq!(
        responses[1]
            .persisted_data_checksums
            .as_ref()
            .unwrap()
            .crc32c,
        Some(crc32c::crc32c(b"hello"))
    );
    let handle = responses[1].write_handle.clone().unwrap().handle;
    let generation = created.generation;

    let (object, content) = read(&storage, "log").await.unwrap();
    assert_eq!(content, b"hello");
    assert_eq!(object.generation, generation);
    assert!(object.checksums.unwrap().md5_hash.is_empty());
    assert_eq!(get(&storage, "log").await.unwrap().size, 5);

    // Resuming with the writer's own handle continues at the persisted offset.
    let mut finish = chunk(5, b" world", false);
    finish.finish_write = true;
    finish.object_checksums = Some(pb::ObjectChecksums {
        crc32c: Some(crc32c::crc32c(b"hello world")),
        md5_hash: Vec::new(),
    });
    let (result, responses) = append(
        &storage,
        vec![Ok(take_over("log", generation, Some(handle))), Ok(finish)],
        None,
    )
    .await;
    result.unwrap();
    assert_eq!(persisted(&responses[0]), 5);
    let Some(WriteStatus::Resource(finalized)) = &responses[1].write_status else {
        panic!("finish_write must return the finalized resource")
    };
    assert_eq!((finalized.generation, finalized.size), (generation, 11));
    let (object, content) = read(&storage, "log").await.unwrap();
    assert_eq!(content, b"hello world");
    assert_eq!(object.generation, generation);
    // Finalized objects are ordinary catalog generations, visible to every protocol.
    let record = storage
        .state
        .catalog
        .get_object_authorized(
            "grpc-append",
            "log",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!((record.generation, record.size), (generation, 11));

    let (result, _) = append(&storage, vec![Ok(take_over("log", generation, None))], None).await;
    assert_eq!(result.unwrap_err().code(), Code::FailedPrecondition);
}

#[tokio::test]
async fn append_hides_unflushed_bytes_and_truncates_them_on_takeover() {
    let (_dir, storage) = setup(false).await;
    let (result, responses) = append(
        &storage,
        vec![
            Ok(create("tail", b"abc")),
            Ok(chunk(3, b"def", false)),
            Err(Status::cancelled("client went away")),
        ],
        None,
    )
    .await;
    assert_eq!(result.unwrap_err().code(), Code::Cancelled);
    let generation = match responses[0].write_status.as_ref().unwrap() {
        WriteStatus::Resource(object) => object.generation,
        WriteStatus::PersistedSize(_) => panic!("missing resource"),
    };
    assert_eq!(read(&storage, "tail").await.unwrap().1, b"abc");

    // Overlapping resends are skipped; gaps are rejected.
    let (result, responses) = append(
        &storage,
        vec![
            Ok(take_over("tail", generation, None)),
            Ok(chunk(1, b"bcXY", true)),
            Ok(chunk(9, b"gap", true)),
        ],
        None,
    )
    .await;
    assert_eq!(result.unwrap_err().code(), Code::OutOfRange);
    assert_eq!(persisted(&responses[0]), 3);
    assert_eq!(persisted(&responses[1]), 5);
    assert_eq!(read(&storage, "tail").await.unwrap().1, b"abcXY");
}

#[tokio::test]
async fn append_takeover_supersedes_the_previous_writer() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    let mut options = crate::storage::ResumableOptions::new(BucketAccess::Permissive);
    options.content_type = "text/plain".into();
    let mut first = catalog
        .create_appendable("grpc-append", "single", options)
        .await
        .unwrap();
    first
        .append(0, Bytes::from_static(b"one"), 1024)
        .await
        .unwrap();
    first.flush().await.unwrap();
    let generation = first.record().generation;
    let first_handle = first.write_handle().to_vec();
    let mut second = catalog
        .take_over_appendable(
            "grpc-append",
            "single",
            generation,
            None,
            Default::default(),
            BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(second.persisted_size(), 3);
    assert_eq!(
        first.append(3, Bytes::from_static(b"two"), 1024).await,
        Err(AppendError::Superseded)
    );
    assert_eq!(first.flush().await, Err(AppendError::Superseded));
    // The superseded writer's handle cannot steal the object back.
    assert_eq!(
        catalog
            .take_over_appendable(
                "grpc-append",
                "single",
                generation,
                Some(&first_handle),
                Default::default(),
                BucketAccess::Permissive,
            )
            .await
            .err(),
        Some(AppendError::Superseded)
    );
    second
        .append(3, Bytes::from_static(b"two"), 1024)
        .await
        .unwrap();
    assert_eq!(second.flush().await, Ok(6));
    // Metageneration preconditions apply to takeovers without a write handle.
    assert_eq!(
        catalog
            .take_over_appendable(
                "grpc-append",
                "single",
                generation,
                None,
                ObjectConditions {
                    if_metageneration_match: Some(2),
                    ..Default::default()
                },
                BucketAccess::Permissive,
            )
            .await
            .err(),
        Some(AppendError::Object(ObjectError::PreconditionFailed))
    );
    assert_eq!(read(&storage, "single").await.unwrap().1, b"onetwo");
}

#[tokio::test]
async fn append_unfinalized_object_survives_restart() {
    let (dir, storage) = setup(false).await;
    let (result, responses) = append(&storage, vec![Ok(create("durable", b"persist"))], None).await;
    result.unwrap();
    let generation = match responses[0].write_status.as_ref().unwrap() {
        WriteStatus::Resource(object) => object.generation,
        WriteStatus::PersistedSize(_) => panic!("missing resource"),
    };
    storage.state.catalog.close().await.unwrap();
    let mut storage = storage;
    Arc::get_mut(&mut storage.state).unwrap().catalog = Catalog::open(&dir.0).await.unwrap();
    let (object, content) = read(&storage, "durable").await.unwrap();
    assert_eq!(
        (object.generation, content.as_slice()),
        (generation, &b"persist"[..])
    );
    let mut finish = chunk(7, b"ed", false);
    finish.finish_write = true;
    let (result, responses) = append(
        &storage,
        vec![Ok(take_over("durable", generation, None)), Ok(finish)],
        None,
    )
    .await;
    result.unwrap();
    assert_eq!(persisted(&responses[1]), 9);
    assert_eq!(read(&storage, "durable").await.unwrap().1, b"persisted");
}

#[tokio::test]
async fn append_rejects_replacement_and_invalid_messages_without_false_success() {
    let (_dir, storage) = setup(false).await;
    let (result, _) = append(&storage, vec![Ok(create("existing", b"first"))], None).await;
    result.unwrap();
    // Another appendable creation over a live name is not silently merged or replaced.
    let (result, _) = append(&storage, vec![Ok(create("existing", b"second"))], None).await;
    let status = result.unwrap_err();
    assert_eq!(status.code(), Code::Unimplemented);
    assert!(!status.details().is_empty());
    let mut create_only = create("existing", b"second");
    if let Some(FirstMessage::WriteObjectSpec(spec)) = create_only.first_message.as_mut() {
        spec.if_generation_match = Some(0);
    }
    let (result, _) = append(&storage, vec![Ok(create_only)], None).await;
    assert_eq!(result.unwrap_err().code(), Code::FailedPrecondition);
    assert_eq!(read(&storage, "existing").await.unwrap().1, b"first");

    let mut sized = create("sized", b"x");
    if let Some(FirstMessage::WriteObjectSpec(spec)) = sized.first_message.as_mut() {
        spec.object_size = Some(1);
    }
    let mut early_checksum = create("early", b"x");
    early_checksum.object_checksums = Some(pb::ObjectChecksums {
        crc32c: Some(1),
        md5_hash: Vec::new(),
    });
    let mut encrypted = create("encrypted", b"x");
    encrypted.common_object_request_params = Some(pb::CommonObjectRequestParams {
        encryption_algorithm: "AES256".into(),
        ..Default::default()
    });
    let mut md5_finish = chunk(1, b"", false);
    md5_finish.finish_write = true;
    md5_finish.object_checksums = Some(pb::ObjectChecksums {
        crc32c: None,
        md5_hash: vec![0; 16],
    });
    let mut wrong_finish = chunk(1, b"", false);
    wrong_finish.finish_write = true;
    wrong_finish.object_checksums = Some(pb::ObjectChecksums {
        crc32c: Some(7),
        md5_hash: Vec::new(),
    });
    let mut bad_crc = chunk(1, b"y", true);
    if let Some(Data::ChecksummedData(data)) = bad_crc.data.as_mut() {
        data.crc32c = Some(1);
    }
    for (messages, code) in [
        (vec![Ok(sized)], Code::Unimplemented),
        (vec![Ok(early_checksum)], Code::InvalidArgument),
        (vec![Ok(encrypted)], Code::Unimplemented),
        (
            vec![Ok(create("md5", b"x")), Ok(md5_finish)],
            Code::InvalidArgument,
        ),
        (
            vec![Ok(create("crc", b"x")), Ok(wrong_finish)],
            Code::InvalidArgument,
        ),
        (
            vec![Ok(create("chunk", b"x")), Ok(bad_crc)],
            Code::InvalidArgument,
        ),
        (
            vec![Ok(take_over("missing", 0, None))],
            Code::InvalidArgument,
        ),
        (vec![Ok(take_over("missing", 42, None))], Code::NotFound),
        (
            vec![Ok(create("again", b"x")), Ok(create("again", b"y"))],
            Code::InvalidArgument,
        ),
    ] {
        let (result, _) = append(&storage, messages, None).await;
        assert_eq!(result.unwrap_err().code(), code);
    }
    // A failed finalization keeps the unfinalized object readable.
    assert_eq!(read(&storage, "crc").await.unwrap().1, b"x");
    assert!(get(&storage, "crc").await.unwrap().finalize_time.is_none());

    // Deleting an unfinalized object removes its namespace entry.
    storage
        .delete_object(
            Request::new(pb::DeleteObjectRequest {
                bucket: BUCKET.into(),
                object: "crc".into(),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        get(&storage, "crc").await.unwrap_err().code(),
        Code::NotFound
    );
    let (result, _) = append(&storage, vec![Ok(create("crc", b"new"))], None).await;
    result.unwrap();
    assert_eq!(read(&storage, "crc").await.unwrap().1, b"new");
}

#[tokio::test]
async fn append_strict_requires_create_and_allows_viewer_reads() {
    let (_dir, storage) = setup(true).await;
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", "Bearer admin".parse().unwrap());
    let admin = storage.authenticate(&request).unwrap();
    request
        .metadata_mut()
        .insert("authorization", "Bearer reader".parse().unwrap());
    let reader = storage.authenticate(&request).unwrap();
    let (result, responses) = append(&storage, vec![Ok(create("strict", b"secret"))], admin).await;
    result.unwrap();
    let generation = match responses[0].write_status.as_ref().unwrap() {
        WriteStatus::Resource(object) => object.generation,
        WriteStatus::PersistedSize(_) => panic!("missing resource"),
    };
    let (result, _) = append(&storage, vec![Ok(create("denied", b"x"))], reader).await;
    assert_eq!(result.unwrap_err().code(), Code::PermissionDenied);
    let (result, _) = append(
        &storage,
        vec![Ok(take_over("strict", generation, None))],
        reader,
    )
    .await;
    assert_eq!(result.unwrap_err().code(), Code::PermissionDenied);
    let object = storage
        .get_object(
            Request::new(pb::GetObjectRequest {
                bucket: BUCKET.into(),
                object: "strict".into(),
                ..Default::default()
            }),
            reader,
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(object.size, 6);
}
