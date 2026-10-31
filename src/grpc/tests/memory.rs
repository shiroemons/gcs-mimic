use super::super::{generated::google::storage::v2 as pb, v1, v2};
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    server::State,
    storage::Catalog,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use prost::Message;
use std::{
    convert::Infallible,
    sync::{Arc, atomic::AtomicBool},
};
use tonic::{Code, Status, codegen::Service};

const BUCKET: &str = "projects/_/buckets/memory-grpc";
type V2 = pb::storage_server::StorageServer<v2::Storage>;

async fn state(budget: u64) -> Arc<State> {
    let catalog = Catalog::open_memory_for_server(
        budget,
        1024 * 1024,
        Arc::new(SystemClock),
        crate::storage::admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let bucket = crate::json::parse_insert(
        br#"{"name":"memory-grpc","versioning":{"enabled":true},"softDeletePolicy":{"retentionDurationSeconds":"604800"}}"#,
        "project",
        std::time::SystemTime::now(),
    )
    .unwrap();
    catalog.insert_bucket(bucket).await.unwrap();
    Arc::new(State {
        ready: AtomicBool::new(true), clock: Arc::new(SystemClock), catalog,
        api_base_url: "http://localhost:4443".into(), default_location: "US".into(),
        auth: Auth::strict(Registry::parse(br#"{"identities":[{"id":"admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},{"id":"reader@example.test","tokens":["reader"],"project_roles":{"project":["roles/storage.objectViewer"]}}]}"#).unwrap()),
    })
}

async fn rpc<S, M, R>(
    service: &mut S,
    path: &str,
    token: Option<&str>,
    messages: &[M],
) -> (Code, Vec<R>)
where
    S: Service<
            hyper::Request<Full<Bytes>>,
            Response = hyper::Response<tonic::body::Body>,
            Error = Infallible,
        >,
    M: Message,
    R: Message + Default,
{
    let mut body = Vec::new();
    for message in messages {
        let encoded = message.encode_to_vec();
        body.push(0);
        body.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        body.extend_from_slice(&encoded);
    }
    let mut request = hyper::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = service
        .call(request.body(Full::new(Bytes::from(body))).unwrap())
        .await
        .unwrap();
    let mut code = Status::from_header_map(response.headers()).map(|status| status.code());
    let mut body = response.into_body();
    let mut encoded = Vec::new();
    while let Some(frame) = body.frame().await {
        match frame.unwrap().into_data() {
            Ok(data) => {
                assert!(encoded.len() + data.len() <= 4 * 1024 * 1024);
                encoded.extend_from_slice(&data);
            }
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers() {
                    code = Status::from_header_map(&trailers).map(|status| status.code());
                }
            }
        }
    }
    let mut messages = Vec::new();
    let mut remaining = encoded.as_slice();
    while !remaining.is_empty() {
        assert!(remaining.len() >= 5);
        assert_eq!(remaining[0], 0);
        let size = u32::from_be_bytes(remaining[1..5].try_into().unwrap()) as usize;
        assert!(remaining.len() >= 5 + size);
        messages.push(R::decode(&remaining[5..5 + size]).unwrap());
        remaining = &remaining[5 + size..];
    }
    (code.expect("terminal grpc-status"), messages)
}

async fn call<M: Message, R: Message + Default>(
    service: &mut V2,
    method: &str,
    token: Option<&str>,
    messages: &[M],
) -> (Code, Vec<R>) {
    rpc(
        service,
        &format!("/google.storage.v2.Storage/{method}"),
        token,
        messages,
    )
    .await
}

fn spec(name: &str) -> pb::WriteObjectSpec {
    pb::WriteObjectSpec {
        resource: Some(pb::Object {
            bucket: BUCKET.into(),
            name: name.into(),
            content_type: "text/plain".into(),
            metadata: [("key".into(), "value".into())].into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn write(name: &str, content: &[u8]) -> pb::WriteObjectRequest {
    pb::WriteObjectRequest {
        first_message: Some(pb::write_object_request::FirstMessage::WriteObjectSpec(
            spec(name),
        )),
        finish_write: true,
        data: Some(pb::write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: Bytes::copy_from_slice(content),
                crc32c: Some(crc32c::crc32c(content)),
            },
        )),
        ..Default::default()
    }
}

async fn read(service: &mut V2, name: &str) -> Vec<u8> {
    let (code, messages): (_, Vec<pb::ReadObjectResponse>) = call(
        service,
        "ReadObject",
        Some("reader"),
        &[pb::ReadObjectRequest {
            bucket: BUCKET.into(),
            object: name.into(),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let mut bytes = Vec::new();
    for message in messages {
        if let Some(data) = message.checksummed_data {
            assert!(data.content.len() <= 64 * 1024);
            assert_eq!(data.crc32c, Some(crc32c::crc32c(&data.content)));
            bytes.extend_from_slice(&data.content);
        }
    }
    bytes
}

#[tokio::test]
async fn ram_grpc_v2_framing_shares_io_permissions_history_and_derived_publication() {
    let state = state(8 * 1024 * 1024).await;
    let mut service = v2::service(state.clone(), 4 * 1024 * 1024);
    let content = vec![37; 512 * 1024 + 3];
    let (code, written): (_, Vec<pb::WriteObjectResponse>) = call(
        &mut service,
        "WriteObject",
        Some("admin"),
        &[write("source", &content)],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let Some(pb::write_object_response::WriteStatus::Resource(object)) = &written[0].write_status
    else {
        panic!("missing write resource");
    };
    let generation = object.generation;
    assert_eq!(read(&mut service, "source").await, content);
    for (token, expected) in [
        (None, Code::Unauthenticated),
        (Some("reader"), Code::PermissionDenied),
    ] {
        let (code, _): (_, Vec<pb::WriteObjectResponse>) = call(
            &mut service,
            "WriteObject",
            token,
            &[write("source", b"denied")],
        )
        .await;
        assert_eq!(code, expected);
    }
    let mut corrupt = write("source", b"bad checksum");
    corrupt.object_checksums = Some(pb::ObjectChecksums {
        crc32c: Some(0),
        ..Default::default()
    });
    let (code, _): (_, Vec<pb::WriteObjectResponse>) =
        call(&mut service, "WriteObject", Some("admin"), &[corrupt]).await;
    assert_eq!(code, Code::InvalidArgument);
    let (code, objects): (_, Vec<pb::Object>) = call(
        &mut service,
        "ComposeObject",
        Some("admin"),
        &[pb::ComposeObjectRequest {
            destination: Some(pb::Object {
                bucket: BUCKET.into(),
                name: "composite".into(),
                ..Default::default()
            }),
            source_objects: vec![
                pb::compose_object_request::SourceObject {
                    name: "source".into(),
                    generation,
                    ..Default::default()
                };
                2
            ],
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(objects[0].size, 2 * content.len() as i64);
    assert!(objects[0].checksums.as_ref().unwrap().md5_hash.is_empty());
    let (code, rewritten): (_, Vec<pb::RewriteResponse>) = call(
        &mut service,
        "RewriteObject",
        Some("admin"),
        &[pb::RewriteObjectRequest {
            source_bucket: BUCKET.into(),
            source_object: "source".into(),
            source_generation: generation,
            destination_bucket: BUCKET.into(),
            destination_name: "copied".into(),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert!(rewritten[0].done);
    assert_eq!(read(&mut service, "copied").await, content);
    let (code, _): (_, Vec<pb::Object>) = call(
        &mut service,
        "MoveObject",
        Some("admin"),
        &[pb::MoveObjectRequest {
            bucket: BUCKET.into(),
            source_object: "copied".into(),
            destination_object: "moved".into(),
            if_generation_match: Some(0),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(read(&mut service, "moved").await, content);
    let (code, _): (_, Vec<pb::Object>) = call(
        &mut service,
        "UpdateObject",
        Some("admin"),
        &[pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: BUCKET.into(),
                name: "source".into(),
                content_type: "application/custom".into(),
                ..Default::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["content_type".into()],
            }),
            if_generation_match: Some(generation),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let (code, ranges): (_, Vec<pb::BidiReadObjectResponse>) = call(
        &mut service,
        "BidiReadObject",
        Some("reader"),
        &[pb::BidiReadObjectRequest {
            read_object_spec: Some(pb::BidiReadObjectSpec {
                bucket: BUCKET.into(),
                object: "source".into(),
                generation,
                ..Default::default()
            }),
            read_ranges: vec![
                pb::ReadRange {
                    read_offset: 1,
                    read_length: 7,
                    read_id: 1,
                },
                pb::ReadRange {
                    read_offset: 100,
                    read_length: 3,
                    read_id: 2,
                },
            ],
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let mut received = [Vec::new(), Vec::new()];
    for range in ranges
        .into_iter()
        .flat_map(|response| response.object_data_ranges)
    {
        let selected = range.read_range.unwrap();
        let data = range.checksummed_data.unwrap();
        assert_eq!(data.crc32c, Some(crc32c::crc32c(&data.content)));
        received[selected.read_id as usize - 1].extend_from_slice(&data.content);
    }
    assert_eq!(
        received,
        [content[1..8].to_vec(), content[100..103].to_vec()]
    );
    let (code, listed): (_, Vec<pb::ListObjectsResponse>) = call(
        &mut service,
        "ListObjects",
        Some("reader"),
        &[pb::ListObjectsRequest {
            parent: BUCKET.into(),
            page_size: 2,
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(listed[0].objects.len(), 2);
    assert!(!listed[0].next_page_token.is_empty());
    assert_eq!(
        state
            .catalog
            .get_object("memory-grpc", "source", None, Default::default())
            .await
            .unwrap()
            .generation,
        generation
    );
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_grpc_resumable_capacity_hash_reconnect_and_cancel_keep_acknowledged_offset() {
    let state = state(2048).await;
    let mut service = v2::service(state.clone(), 8192);
    let (code, _): (_, Vec<pb::WriteObjectResponse>) = call(
        &mut service,
        "WriteObject",
        Some("admin"),
        &[write("old", b"kept")],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let (code, started): (_, Vec<pb::StartResumableWriteResponse>) = call(
        &mut service,
        "StartResumableWrite",
        Some("admin"),
        &[pb::StartResumableWriteRequest {
            write_object_spec: Some(spec("session")),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let upload = started[0].upload_id.clone();
    let mut first = write("ignored", b"abc");
    first.first_message = Some(pb::write_object_request::FirstMessage::UploadId(
        upload.clone(),
    ));
    first.finish_write = false;
    let (code, _): (_, Vec<pb::WriteObjectResponse>) =
        call(&mut service, "WriteObject", Some("admin"), &[first.clone()]).await;
    assert_eq!(code, Code::Ok);
    let mut exhausted = write("ignored", &vec![5; 4096]);
    exhausted.first_message = first.first_message.clone();
    exhausted.write_offset = 3;
    let (code, _): (_, Vec<pb::WriteObjectResponse>) =
        call(&mut service, "WriteObject", Some("admin"), &[exhausted]).await;
    assert_eq!(code, Code::Unavailable);
    let (code, status): (_, Vec<pb::QueryWriteStatusResponse>) = call(
        &mut service,
        "QueryWriteStatus",
        Some("admin"),
        &[pb::QueryWriteStatusRequest {
            upload_id: upload.clone(),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(
        status[0].write_status,
        Some(pb::query_write_status_response::WriteStatus::PersistedSize(
            3
        ))
    );
    assert_eq!(read(&mut service, "old").await, b"kept");
    let (code, _): (_, Vec<pb::WriteObjectResponse>) =
        call(&mut service, "WriteObject", Some("admin"), &[first.clone()]).await;
    assert_eq!(code, Code::OutOfRange);
    let final_message = pb::BidiWriteObjectRequest {
        first_message: Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload,
        )),
        write_offset: 3,
        finish_write: true,
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: Bytes::from_static(b"def"),
                crc32c: Some(crc32c::crc32c(b"def")),
            },
        )),
        object_checksums: Some(pb::ObjectChecksums {
            crc32c: Some(crc32c::crc32c(b"abcdef")),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (code, _): (_, Vec<pb::BidiWriteObjectResponse>) = call(
        &mut service,
        "BidiWriteObject",
        Some("admin"),
        &[final_message],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(read(&mut service, "session").await, b"abcdef");
    let (code, started): (_, Vec<pb::StartResumableWriteResponse>) = call(
        &mut service,
        "StartResumableWrite",
        Some("admin"),
        &[pb::StartResumableWriteRequest {
            write_object_spec: Some(spec("cancelled")),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let cancelled = started[0].upload_id.clone();
    let mut partial = write("ignored", b"partial");
    partial.first_message = Some(pb::write_object_request::FirstMessage::UploadId(
        cancelled.clone(),
    ));
    partial.finish_write = false;
    let (code, _): (_, Vec<pb::WriteObjectResponse>) =
        call(&mut service, "WriteObject", Some("admin"), &[partial]).await;
    assert_eq!(code, Code::Ok);
    let (code, _): (_, Vec<pb::CancelResumableWriteResponse>) = call(
        &mut service,
        "CancelResumableWrite",
        Some("reader"),
        &[pb::CancelResumableWriteRequest {
            upload_id: cancelled.clone(),
        }],
    )
    .await;
    assert_eq!(code, Code::PermissionDenied);
    let (code, _): (_, Vec<pb::CancelResumableWriteResponse>) = call(
        &mut service,
        "CancelResumableWrite",
        Some("admin"),
        &[pb::CancelResumableWriteRequest {
            upload_id: cancelled.clone(),
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let (code, _): (_, Vec<pb::QueryWriteStatusResponse>) = call(
        &mut service,
        "QueryWriteStatus",
        Some("admin"),
        &[pb::QueryWriteStatusRequest {
            upload_id: cancelled,
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::NotFound);
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_grpc_restore_reuses_retained_source_permissions_and_live_conditions() {
    let state = state(8192).await;
    let mut service = v2::service(state.clone(), 8192);
    let (code, written): (_, Vec<pb::WriteObjectResponse>) = call(
        &mut service,
        "WriteObject",
        Some("admin"),
        &[write("restore", b"kept")],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let Some(pb::write_object_response::WriteStatus::Resource(source)) = &written[0].write_status
    else {
        panic!("missing write resource");
    };
    let (code, _): (_, Vec<()>) = call(
        &mut service,
        "DeleteObject",
        Some("admin"),
        &[pb::DeleteObjectRequest {
            bucket: BUCKET.into(),
            object: "restore".into(),
            generation: source.generation,
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let restore = pb::RestoreObjectRequest {
        bucket: BUCKET.into(),
        object: "restore".into(),
        generation: source.generation,
        if_generation_match: Some(0),
        ..Default::default()
    };
    let (code, _): (_, Vec<pb::Object>) = call(
        &mut service,
        "RestoreObject",
        Some("reader"),
        std::slice::from_ref(&restore),
    )
    .await;
    assert_eq!(code, Code::PermissionDenied);
    let mut unsupported = restore.clone();
    unsupported.copy_source_acl = Some(true);
    let (code, _): (_, Vec<pb::Object>) =
        call(&mut service, "RestoreObject", Some("admin"), &[unsupported]).await;
    assert_eq!(code, Code::Unimplemented);
    let (code, restored): (_, Vec<pb::Object>) = call(
        &mut service,
        "RestoreObject",
        Some("admin"),
        std::slice::from_ref(&restore),
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_ne!(restored[0].generation, source.generation);
    assert_eq!(restored[0].checksums, source.checksums);
    assert_eq!(read(&mut service, "restore").await, b"kept");
    let (code, _): (_, Vec<pb::Object>) =
        call(&mut service, "RestoreObject", Some("admin"), &[restore]).await;
    assert_eq!(code, Code::FailedPrecondition);
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_grpc_appendable_and_legacy_share_the_same_catalog_and_readers() {
    let state = state(8192).await;
    let mut service = v2::service(state.clone(), 8192);
    let ordinary = pb::BidiWriteObjectRequest {
        first_message: Some(
            pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec("bidi")),
        ),
        finish_write: true,
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: Bytes::from_static(b"ordinary"),
                crc32c: Some(crc32c::crc32c(b"ordinary")),
            },
        )),
        ..Default::default()
    };
    let (code, _): (_, Vec<pb::BidiWriteObjectResponse>) =
        call(&mut service, "BidiWriteObject", Some("admin"), &[ordinary]).await;
    assert_eq!(code, Code::Ok);
    assert_eq!(read(&mut service, "bidi").await, b"ordinary");
    let mut append_spec = spec("growing");
    append_spec.appendable = Some(true);
    let create = pb::BidiWriteObjectRequest {
        first_message: Some(
            pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(append_spec),
        ),
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: Bytes::from_static(b"abc"),
                crc32c: Some(crc32c::crc32c(b"abc")),
            },
        )),
        flush: true,
        state_lookup: true,
        ..Default::default()
    };
    let (code, responses): (_, Vec<pb::BidiWriteObjectResponse>) =
        call(&mut service, "BidiWriteObject", Some("admin"), &[create]).await;
    assert_eq!(code, Code::Ok);
    let generation = responses
        .iter()
        .find_map(|response| match &response.write_status {
            Some(pb::bidi_write_object_response::WriteStatus::Resource(object)) => {
                Some(object.generation)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(read(&mut service, "growing").await, b"abc");
    let finish = pb::BidiWriteObjectRequest {
        first_message: Some(
            pb::bidi_write_object_request::FirstMessage::AppendObjectSpec(pb::AppendObjectSpec {
                bucket: BUCKET.into(),
                object: "growing".into(),
                generation,
                ..Default::default()
            }),
        ),
        write_offset: 3,
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: Bytes::from_static(b"def"),
                crc32c: Some(crc32c::crc32c(b"def")),
            },
        )),
        finish_write: true,
        object_checksums: Some(pb::ObjectChecksums {
            crc32c: Some(crc32c::crc32c(b"abcdef")),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (code, _): (_, Vec<pb::BidiWriteObjectResponse>) =
        call(&mut service, "BidiWriteObject", Some("admin"), &[finish]).await;
    assert_eq!(code, Code::Ok);
    assert_eq!(read(&mut service, "growing").await, b"abcdef");
    let mut legacy = v1::service(
        state.clone(),
        8192,
        crate::config::CompatProfile::LegacyEmulator,
    );
    let insert = v1::pb::InsertObjectRequest {
        first_message: Some(
            v1::pb::insert_object_request::FirstMessage::InsertObjectSpec(
                v1::pb::InsertObjectSpec {
                    resource: Some(v1::pb::Object {
                        bucket: "memory-grpc".into(),
                        name: "legacy".into(),
                        content_type: "text/plain".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ),
        ),
        data: Some(v1::pb::insert_object_request::Data::ChecksummedData(
            v1::pb::ChecksummedData {
                content: Bytes::from_static(b"legacy body"),
                crc32c: Some(crc32c::crc32c(b"legacy body")),
            },
        )),
        finish_write: true,
        ..Default::default()
    };
    let (code, objects): (_, Vec<v1::pb::Object>) = rpc(
        &mut legacy,
        "/google.storage.v1.Storage/InsertObject",
        Some("admin"),
        std::slice::from_ref(&insert),
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(objects[0].size, 11);
    let (code, _): (_, Vec<v1::pb::Object>) = rpc(
        &mut legacy,
        "/google.storage.v1.Storage/InsertObject",
        Some("reader"),
        &[insert],
    )
    .await;
    assert_eq!(code, Code::PermissionDenied);
    let (code, metadata): (_, Vec<v1::pb::Object>) = rpc(
        &mut legacy,
        "/google.storage.v1.Storage/GetObject",
        Some("reader"),
        &[v1::pb::GetObjectRequest {
            bucket: "memory-grpc".into(),
            object: "legacy".into(),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(metadata[0].generation, objects[0].generation);
    assert_eq!(read(&mut service, "legacy").await, b"legacy body");
    state.catalog.close().await.unwrap();
}

/// Object get/update permissions are decided in the catalog after the bucket stage, so the
/// anonymous mapping must still ask for credentials there, as JSON does.
#[tokio::test]
async fn strict_anonymous_object_denials_ask_for_credentials_on_both_grpc_versions() {
    let state = state(1024 * 1024).await;
    let mut service = v2::service(state.clone(), 1024 * 1024);
    let (code, _): (_, Vec<pb::WriteObjectResponse>) = call(
        &mut service,
        "WriteObject",
        Some("admin"),
        &[write("private", b"private")],
    )
    .await;
    assert_eq!(code, Code::Ok);
    let get = pb::GetObjectRequest {
        bucket: BUCKET.into(),
        object: "private".into(),
        ..Default::default()
    };
    let (code, _): (_, Vec<pb::Object>) = call(&mut service, "GetObject", None, &[get]).await;
    assert_eq!(code, Code::Unauthenticated);
    let (code, _): (_, Vec<pb::ReadObjectResponse>) = call(
        &mut service,
        "ReadObject",
        None,
        &[pb::ReadObjectRequest {
            bucket: BUCKET.into(),
            object: "private".into(),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Unauthenticated);
    let update = pb::UpdateObjectRequest {
        object: Some(pb::Object {
            bucket: BUCKET.into(),
            name: "private".into(),
            content_type: "text/csv".into(),
            ..Default::default()
        }),
        update_mask: Some(prost_types::FieldMask {
            paths: vec!["content_type".into()],
        }),
        ..Default::default()
    };
    // A registered principal without the grant keeps PermissionDenied.
    for (token, expected) in [
        (None, Code::Unauthenticated),
        (Some("reader"), Code::PermissionDenied),
    ] {
        let (code, _): (_, Vec<pb::Object>) = call(
            &mut service,
            "UpdateObject",
            token,
            std::slice::from_ref(&update),
        )
        .await;
        assert_eq!(code, expected);
    }
    let mut legacy = v1::service(
        state.clone(),
        8192,
        crate::config::CompatProfile::LegacyEmulator,
    );
    let (code, _): (_, Vec<v1::pb::Object>) = rpc(
        &mut legacy,
        "/google.storage.v1.Storage/GetObject",
        None,
        &[v1::pb::GetObjectRequest {
            bucket: "memory-grpc".into(),
            object: "private".into(),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Unauthenticated);
    for (token, expected) in [
        (None, Code::Unauthenticated),
        (Some("reader"), Code::PermissionDenied),
    ] {
        let (code, _): (_, Vec<v1::pb::Object>) = rpc(
            &mut legacy,
            "/google.storage.v1.Storage/UpdateObject",
            token,
            &[v1::pb::UpdateObjectRequest {
                bucket: "memory-grpc".into(),
                object: "private".into(),
                metadata: Some(v1::pb::Object {
                    content_type: "text/csv".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        )
        .await;
        assert_eq!(code, expected);
    }
    let (code, unchanged): (_, Vec<pb::Object>) = call(
        &mut service,
        "GetObject",
        Some("admin"),
        &[pb::GetObjectRequest {
            bucket: BUCKET.into(),
            object: "private".into(),
            ..Default::default()
        }],
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(unchanged[0].content_type, "text/plain");
    state.catalog.close().await.unwrap();
}
