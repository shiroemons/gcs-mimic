use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;

// Field numbers are pinned by google.storage.v2 in the committed protobuf sources.
#[derive(Clone, PartialEq, Message)]
struct GetObject {
    #[prost(string, tag = "1")]
    bucket: String,
    #[prost(string, tag = "2")]
    object: String,
    #[prost(int64, tag = "3")]
    generation: i64,
}
#[derive(Clone, PartialEq, Message)]
struct Object {
    #[prost(int64, tag = "3")]
    generation: i64,
}

async fn get(server: &Server, generation: i64, token: &str) -> (String, Bytes) {
    let message = GetObject {
        bucket: "projects/_/buckets/test-bucket".into(),
        object: "value".into(),
        generation,
    }
    .encode_to_vec();
    call(server, "GetObject", message, token).await
}

pub(super) async fn call(
    server: &Server,
    method: &str,
    message: Vec<u8>,
    token: &str,
) -> (String, Bytes) {
    let mut frame = vec![0];
    frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
    frame.extend_from_slice(&message);
    let stream = tokio::net::TcpStream::connect(&server.grpc_address)
        .await
        .unwrap();
    let (mut client, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
            .await
            .unwrap();
    let driver = tokio::spawn(connection);
    let response = client
        .send_request(
            hyper::Request::builder()
                .method("POST")
                .uri(format!(
                    "http://{}/google.storage.v2.Storage/{method}",
                    server.grpc_address
                ))
                .header("content-type", "application/grpc")
                .header("te", "trailers")
                .header("authorization", format!("Bearer {token}"))
                .body(Full::new(Bytes::from(frame)))
                .unwrap(),
        )
        .await
        .unwrap();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap();
    let status = body
        .trailers()
        .and_then(|trailers| trailers.get("grpc-status"))
        .or_else(|| headers.get("grpc-status"))
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = body.to_bytes();
    drop(client);
    driver.await.unwrap().unwrap();
    (status, bytes)
}

#[tokio::test]
async fn grpc_object_acl_read_checks_selected_generation_and_revocation() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        let object = server.put(b"first");
        let generation = object["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "READER", generation);
        let (status, bytes) = get(&server, generation.parse().unwrap(), "reader").await;
        assert_eq!(status, "0");
        assert_eq!(
            Object::decode(&bytes[5..]).unwrap().generation,
            generation.parse::<i64>().unwrap()
        );
        assert_eq!(
            get(&server, generation.parse().unwrap(), "invalid").await.0,
            "16"
        );
        assert_eq!(
            get(&server, generation.parse().unwrap(), "other").await.0,
            "7"
        );
        server.put(b"second");
        assert_eq!(get(&server, 0, "reader").await.0, "7");
        assert_eq!(
            get(&server, generation.parse().unwrap(), "reader").await.0,
            "0"
        );
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
        assert_eq!(
            get(&server, generation.parse().unwrap(), "reader").await.0,
            "7"
        );
    }
}
