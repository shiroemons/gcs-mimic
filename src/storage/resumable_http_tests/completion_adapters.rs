use super::*;
use crate::{
    grpc::{generated::google::storage::v2 as pb, test_support::routed_rpc_status, v2},
    profile::CompatProfile,
};
use prost::Message;
use tonic::{Code, Status};

async fn grpc<M: Message, R: Message + Default>(
    h: &Harness,
    method: &str,
    message: M,
) -> (Status, Vec<R>) {
    routed_rpc_status(
        &format!("/google.storage.v2.Storage/{method}"),
        None,
        "bucket=projects%2F_%2Fbuckets%2Fbucket",
        &[message],
        async |request| {
            v2::dispatch(
                v2::service(h.state.clone(), 1000, CompatProfile::Native),
                request,
            )
            .await
            .unwrap()
        },
    )
    .await
}

async fn query(h: &Harness, upload_id: &str) -> Status {
    let (status, replies): (_, Vec<pb::QueryWriteStatusResponse>) = grpc(
        h,
        "QueryWriteStatus",
        pb::QueryWriteStatusRequest {
            upload_id: upload_id.into(),
            ..Default::default()
        },
    )
    .await;
    assert!(replies.is_empty(), "{replies:?}");
    status
}

async fn complete_json(h: &Harness) -> String {
    let initiated = h.initiate("name=json", "", b"").await;
    assert_eq!(initiated.status, 200);
    let path = h.path(&initiated);
    let reply = h.request("PUT", &path, "", b"abc").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["name"], "json");
    path
}

#[tokio::test]
async fn completed_resumable_sessions_keep_their_cross_adapter_responses() {
    let h = Harness::start(1000, Permissive, None).await;
    let json_path = complete_json(&h).await;
    let (_, json_id) = json_path.split_once("upload_id=").unwrap();
    let status = query(&h, &format!("bucket/{json_id}")).await;
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(status.message(), "Invalid upload completion");
    assert!(status.details().is_empty());

    let (status, started): (_, Vec<pb::StartResumableWriteResponse>) = grpc(
        &h,
        "StartResumableWrite",
        pb::StartResumableWriteRequest {
            write_object_spec: Some(pb::WriteObjectSpec {
                resource: Some(pb::Object {
                    name: "proto".into(),
                    bucket: "projects/_/buckets/bucket".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(status.code(), Code::Ok, "{status}");
    let [started] = started.as_slice() else {
        panic!("{started:?}")
    };
    let write = pb::WriteObjectRequest {
        first_message: Some(pb::write_object_request::FirstMessage::UploadId(
            started.upload_id.clone(),
        )),
        data: Some(pb::write_object_request::Data::ChecksummedData(
            pb::ChecksummedData {
                content: bytes::Bytes::from_static(b"abc"),
                crc32c: None,
            },
        )),
        finish_write: true,
        ..Default::default()
    };
    let (status, finished): (_, Vec<pb::WriteObjectResponse>) =
        grpc(&h, "WriteObject", write.clone()).await;
    assert_eq!(status.code(), Code::Ok, "{status}");
    let [
        pb::WriteObjectResponse {
            write_status: Some(pb::write_object_response::WriteStatus::Resource(resource)),
            ..
        },
    ] = finished.as_slice()
    else {
        panic!("{finished:?}");
    };
    let (status, replayed): (_, Vec<pb::WriteObjectResponse>) =
        grpc(&h, "WriteObject", write).await;
    assert_eq!(status.code(), Code::Ok, "{status}");
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].encode_to_vec(), finished[0].encode_to_vec());

    let (_, proto_id) = started.upload_id.split_once('/').unwrap();
    let path = format!("/upload/storage/v1/b/bucket/o?upload_id={proto_id}");
    let connection = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
    let stored: String = connection
        .query_row(
            "SELECT completion FROM resumable_sessions WHERE id = ?1",
            [proto_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(connection);
    let proto: Vec<u8> = serde_json::from_str(&stored).unwrap();
    let expected = serde_json::to_vec_pretty(&proto).unwrap();
    assert_eq!(
        pb::Object::decode(proto.as_slice()).unwrap().generation,
        resource.generation
    );
    for (method, headers) in [("PUT", "Content-Range: bytes */*\r\n"), ("DELETE", "")] {
        let reply = h.request(method, &path, headers, b"").await;
        assert_eq!(reply.status, 200, "{method}");
        assert_eq!(reply.body, expected, "{method}");
    }
    h.stop().await;
}

#[tokio::test]
async fn unrecognized_resumable_completion_text_keeps_its_responses() {
    let h = Harness::start(1000, Permissive, None).await;
    let path = complete_json(&h).await;
    let (_, id) = path.split_once("upload_id=").unwrap();
    let upload_id = format!("bucket/{id}");
    let connection = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
    let replace = |text: &str| {
        assert_eq!(
            connection
                .execute(
                    "UPDATE resumable_sessions SET completion = ?1 WHERE id = ?2",
                    [text, id],
                )
                .unwrap(),
            1
        );
    };
    for (text, expected) in [
        ("null", "null"),
        ("\"not bytes\"", "\"not bytes\""),
        ("[256]", "[\n  256\n]"),
    ] {
        replace(text);
        let reply = h
            .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
            .await;
        assert_eq!(reply.status, 200, "{text}");
        assert_eq!(reply.body, expected.as_bytes());
        let status = query(&h, &upload_id).await;
        assert_eq!(status.code(), Code::Internal, "{text}");
        assert_eq!(status.message(), "Invalid upload completion");
    }
    replace("{");
    drop(connection);
    let reply = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    let message =
        "Object storage is unavailable: stored data is corrupt; restore a valid data directory";
    assert_eq!(reply.status, 503);
    assert_eq!(reply.json()["error"]["message"], message);
    let status = query(&h, &upload_id).await;
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(status.message(), message);
    h.stop().await;
}
