use super::*;
use prost::Message;

#[derive(Clone, PartialEq, Message)]
struct Handle {
    #[prost(bytes = "vec", tag = "1")]
    handle: Vec<u8>,
}
#[derive(Clone, PartialEq, Message)]
struct Spec {
    #[prost(string, tag = "1")]
    bucket: String,
    #[prost(string, tag = "2")]
    object: String,
    #[prost(message, optional, tag = "13")]
    handle: Option<Handle>,
}
#[derive(Clone, PartialEq, Message)]
struct Range {
    #[prost(int64, tag = "3")]
    id: i64,
}
#[derive(Clone, PartialEq, Message)]
struct Request {
    #[prost(message, optional, tag = "1")]
    spec: Option<Spec>,
    #[prost(message, repeated, tag = "8")]
    ranges: Vec<Range>,
}
#[derive(Clone, PartialEq, Message)]
struct Data {
    #[prost(bytes = "vec", tag = "1")]
    bytes: Vec<u8>,
    #[prost(fixed32, optional, tag = "2")]
    crc32c: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
struct RangeData {
    #[prost(message, optional, tag = "1")]
    data: Option<Data>,
}
#[derive(Clone, PartialEq, Message)]
struct Response {
    #[prost(message, repeated, tag = "6")]
    ranges: Vec<RangeData>,
    #[prost(message, optional, tag = "7")]
    handle: Option<Handle>,
}

async fn read(server: &Server, token: &str, handle: Option<&[u8]>) -> (String, Vec<u8>, Vec<u8>) {
    let message = Request {
        spec: Some(Spec {
            bucket: "projects/_/buckets/test-bucket".into(),
            object: "value".into(),
            handle: handle.map(|value| Handle {
                handle: value.to_vec(),
            }),
        }),
        ranges: vec![Range { id: 1 }],
    }
    .encode_to_vec();
    let (status, bytes) = super::grpc::call(server, "BidiReadObject", message, token).await;
    let mut cursor = 0;
    let mut handle = Vec::new();
    let mut body = Vec::new();
    while cursor < bytes.len() {
        assert_eq!(bytes[cursor], 0);
        let size = u32::from_be_bytes(bytes[cursor + 1..cursor + 5].try_into().unwrap()) as usize;
        let response = Response::decode(&bytes[cursor + 5..cursor + 5 + size]).unwrap();
        cursor += 5 + size;
        if let Some(selected) = response.handle {
            handle = selected.handle;
        }
        for range in response.ranges {
            let data = range.data.unwrap();
            assert_eq!(data.crc32c, Some(crc32c::crc32c(&data.bytes)));
            body.extend_from_slice(&data.bytes);
        }
    }
    (status, handle, body)
}

#[tokio::test]
async fn bidi_read_handle_reauthorizes_fixed_generation_for_each_new_rpc() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        let object = server.put(b"fixed\0\xff");
        let generation = object["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "READER", generation);
        let (status, handle, body) = read(&server, "reader", None).await;
        assert_eq!(status, "0");
        assert!(!handle.is_empty());
        assert_eq!(body, b"fixed\0\xff");
        let denied = read(&server, "other", Some(&handle)).await;
        assert_eq!(denied.0, "7");
        assert!(denied.2.is_empty());
        server.put(b"private replacement");
        let resumed = read(&server, "reader", Some(&handle)).await;
        assert_eq!(resumed.0, "0");
        assert_eq!(resumed.2, body);
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        assert_eq!(read(&server, "reader", Some(&handle)).await.0, "7");
        server.grant("allAuthenticatedUsers", "READER", generation);
        assert_eq!(read(&server, "reader", Some(&handle)).await.0, "0");
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
            )
            .json();
        assert_eq!(read(&server, "reader", Some(&handle)).await.0, "7");
        server.grant("user-reader@example.test", "READER", generation);
        assert_eq!(read(&server, "reader", Some(&handle)).await.0, "0");
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
            )
            .json();
        assert_eq!(read(&server, "reader", Some(&handle)).await.0, "7");
        assert_eq!(read(&server, "admin", Some(&handle)).await.2, body);
    }
}

#[tokio::test]
async fn bidi_read_iam_handle_keeps_its_opened_snapshot_after_generation_deletion() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    let object = server.put(b"opened snapshot");
    let (status, handle, body) = read(&server, "admin", None).await;
    assert_eq!(status, "0");
    assert_eq!(body, b"opened snapshot");
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!(
                    "{OBJECT}?generation={}",
                    object["generation"].as_str().unwrap()
                ),
                ADMIN,
                b""
            )
            .status,
        204
    );
    let denied = read(&server, "other", Some(&handle)).await;
    assert_eq!(denied.0, "7");
    assert!(denied.2.is_empty());
    let resumed = read(&server, "admin", Some(&handle)).await;
    assert_eq!(resumed.0, "0");
    assert_eq!(resumed.2, body);
}
