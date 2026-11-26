//! What the gRPC listener does with the call metadata that gRPC defines beside a Storage v2
//! request: the `grpc-timeout` deadline and message compression (`grpc-encoding` and
//! `grpc-accept-encoding`). Each call goes through the connection driver and the Storage router
//! the listener runs.

use super::*;
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    grpc::generated::google::storage::v2 as pb,
    storage::{
        Catalog,
        test_fixtures::{PERMISSIVE, live},
    },
};
use flate2::{Compression, write::GzEncoder};
use http_body_util::Full;
use hyper::{HeaderMap, client::conn::http2::SendRequest};
use hyper_util::rt::TokioExecutor;
use prost::Message;
use std::io::Write;
use tonic::{Code, Status};

const BUCKET: &str = "projects/_/buckets/call-metadata";

/// What tonic answers for a `grpc-encoding` the server does not accept.
fn unsupported_encoding(encoding: &str) -> String {
    format!("Content is compressed with `{encoding}` which isn't supported")
}

/// The Storage router behind the connection driver the gRPC listener runs, and one client
/// connection to it.
struct Server {
    state: Arc<State>,
    client: SendRequest<tonic::body::Body>,
    _stop: watch::Sender<bool>,
}

impl Server {
    async fn start() -> Self {
        let catalog = Catalog::open_memory_for_server(
            1024 * 1024,
            1024 * 1024,
            Arc::new(SystemClock),
            crate::storage::admin::LocalAdmin::Enabled,
        )
        .await
        .unwrap();
        let bucket = crate::json::parse_insert(
            br#"{"name":"call-metadata"}"#,
            "project",
            std::time::SystemTime::now(),
        )
        .unwrap();
        catalog.insert_bucket(bucket).await.unwrap();
        let registry = Registry::parse(br#"{"identities":[{"id":"admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}}]}"#).unwrap();
        let state = Arc::new(State::for_test(catalog, Auth::strict(registry)));
        let config = crate::config::ConfigDraft {
            header_read_timeout: TimeLimit::from_millis(60_000),
            keep_alive_timeout: TimeLimit::from_millis(60_000),
            ..Default::default()
        }
        .build();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        let (stop, stopping) = watch::channel(false);
        let router = StorageRouter::new(&config, state.clone());
        tokio::spawn(async move {
            let slots = Arc::<SlotReclaim>::default();
            let service = move |request| router.clone().route(request);
            serve_accepted(accepted, &config, stopping, &slots, service).await;
        });
        let (client, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(socket))
                .await
                .unwrap();
        tokio::spawn(connection);
        Self {
            state,
            client,
            _stop: stop,
        }
    }

    async fn send(&mut self, request: hyper::Request<tonic::body::Body>) -> Reply {
        let response = self.client.send_request(request).await.unwrap();
        Reply::read(response).await
    }

    async fn object(&self, name: &str) -> Option<crate::object::ObjectRecord> {
        match self
            .state
            .catalog
            .get_object(live("call-metadata", name), Default::default(), PERMISSIVE)
            .await
        {
            Ok(record) => Some(record),
            Err(crate::object::ObjectError::ObjectNotFound) => None,
            Err(error) => panic!("{name}: {error:?}"),
        }
    }

    async fn bucket_exists(&self, name: &str) -> bool {
        self.state
            .catalog
            .get_bucket(name, &crate::bucket::BucketAccess::Permissive)
            .await
            .unwrap()
            .is_some()
    }
}

/// A finished call: its response headers, its final status and its messages.
struct Reply {
    headers: HeaderMap,
    status: Status,
    messages: Vec<Bytes>,
}

impl Reply {
    /// Reads the whole response. Every message must arrive uncompressed.
    async fn read(response: hyper::Response<hyper::body::Incoming>) -> Self {
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap();
        // A trailers-only response carries its status in the headers.
        let status = match body.trailers() {
            Some(trailers) => Status::from_header_map(trailers),
            None => Status::from_header_map(&headers),
        }
        .expect("grpc-status");
        let mut remaining = body.to_bytes();
        let mut messages = Vec::new();
        while !remaining.is_empty() {
            assert_eq!(remaining[0], 0, "a response message arrived compressed");
            let size = u32::from_be_bytes(remaining[1..5].try_into().unwrap()) as usize;
            messages.push(remaining.slice(5..5 + size));
            remaining = remaining.slice(5 + size..);
        }
        Self {
            headers,
            status,
            messages,
        }
    }

    #[track_caller]
    fn assert_code(&self, code: Code) {
        assert_eq!(self.status.code(), code, "{:?}", self.status);
    }
}

/// A Storage v2 call with `metadata` beside the gRPC headers, from the admin principal and
/// routed to the test bucket unless `metadata` names other credentials or another route.
fn call<B>(method: &str, metadata: &[(&str, &str)], body: B) -> hyper::Request<tonic::body::Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<tonic::codegen::StdError>,
{
    let sent = |header: &str| metadata.iter().any(|(name, _)| *name == header);
    let mut request = hyper::Request::post(format!(
        "http://localhost/google.storage.v2.Storage/{method}"
    ))
    .header("content-type", "application/grpc")
    .header("te", "trailers");
    if !sent("authorization") {
        request = request.header("authorization", "Bearer admin");
    }
    if !sent("x-goog-request-params") {
        request = request.header(
            "x-goog-request-params",
            crate::grpc::test_support::bucket_routing("call-metadata"),
        );
    }
    for (name, value) in metadata {
        request = request.header(*name, *value);
    }
    request.body(tonic::body::Body::new(body)).unwrap()
}

/// A complete request body of length-prefixed messages.
fn messages(messages: &[Bytes]) -> Full<Bytes> {
    Full::new(messages.concat().into())
}

/// One length-prefixed message whose compressed flag is `flag`.
fn framed(flag: u8, payload: &[u8]) -> Bytes {
    let mut frame = vec![flag];
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(payload);
    frame.into()
}

fn plain(message: &impl Message) -> Bytes {
    framed(0, &message.encode_to_vec())
}

/// `message` gzip-compressed and flagged as compressed, as a client sends it with
/// `grpc-encoding: gzip`.
fn gzipped(message: &impl Message) -> Bytes {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&message.encode_to_vec()).unwrap();
    framed(1, &encoder.finish().unwrap())
}

fn spec(name: &str) -> pb::WriteObjectSpec {
    pb::WriteObjectSpec {
        resource: Some(pb::Object {
            bucket: BUCKET.into(),
            name: name.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn data(content: &'static [u8]) -> pb::ChecksummedData {
    pb::ChecksummedData {
        content: Bytes::from_static(content),
        crc32c: Some(crc32c::crc32c(content)),
    }
}

fn write(name: &str, content: &'static [u8]) -> pb::WriteObjectRequest {
    pb::WriteObjectRequest {
        first_message: Some(pb::write_object_request::FirstMessage::WriteObjectSpec(
            spec(name),
        )),
        data: Some(pb::write_object_request::Data::ChecksummedData(data(
            content,
        ))),
        finish_write: true,
        ..Default::default()
    }
}

fn bidi_write(name: &str, content: &'static [u8]) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        first_message: Some(
            pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec(name)),
        ),
        data: Some(pb::bidi_write_object_request::Data::ChecksummedData(data(
            content,
        ))),
        finish_write: true,
        ..Default::default()
    }
}

fn create_bucket(name: &str) -> pb::CreateBucketRequest {
    pb::CreateBucketRequest {
        parent: "projects/_".into(),
        bucket_id: name.into(),
        bucket: Some(pb::Bucket {
            project: "projects/project".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn get_object(name: &str) -> pb::GetObjectRequest {
    pb::GetObjectRequest {
        bucket: BUCKET.into(),
        object: name.into(),
        ..Default::default()
    }
}

fn read_object(name: &str) -> pb::ReadObjectRequest {
    pb::ReadObjectRequest {
        bucket: BUCKET.into(),
        object: name.into(),
        ..Default::default()
    }
}

/// The server never reads `grpc-timeout`: a stream whose deadline passed before its first message
/// arrived, a unary call whose deadline passed on arrival and a deadline it cannot parse all
/// leave the call to complete. Only the client ends a call at its deadline, by resetting the
/// stream.
#[tokio::test]
async fn grpc_timeout_never_ends_a_call_on_the_server() {
    let mut server = Server::start().await;
    let (chunks, body) = chunk_body::channel();
    let response = server
        .client
        .send_request(call("WriteObject", &[("grpc-timeout", "1n")], body));
    let first = pb::WriteObjectRequest {
        finish_write: false,
        ..write("late", b"ab")
    };
    chunks.send(Ok(plain(&first))).await.unwrap();
    // The one-slot channel takes the last message only after the connection took the first, so
    // the call is still open, long past its deadline, when it arrives.
    let last = pb::WriteObjectRequest {
        write_offset: 2,
        data: Some(pb::write_object_request::Data::ChecksummedData(data(b"cd"))),
        finish_write: true,
        ..Default::default()
    };
    chunks.send(Ok(plain(&last))).await.unwrap();
    drop(chunks);
    let reply = Reply::read(response.await.unwrap()).await;
    reply.assert_code(Code::Ok);
    let written = pb::WriteObjectResponse::decode(reply.messages[0].clone()).unwrap();
    let Some(pb::write_object_response::WriteStatus::Resource(object)) = written.write_status
    else {
        panic!("WriteObject answered without the object: {written:?}");
    };
    assert_eq!(object.size, 4);
    assert_eq!(
        server.object("late").await.map(|record| record.size),
        Some(4)
    );

    for deadline in ["1n", "soon"] {
        let reply = server
            .send(call(
                "GetObject",
                &[("grpc-timeout", deadline)],
                messages(&[plain(&get_object("late"))]),
            ))
            .await;
        reply.assert_code(Code::Ok);
        assert_eq!(
            pb::Object::decode(reply.messages[0].clone()).unwrap().size,
            4
        );
    }
    server.state.catalog.close().await.unwrap();
}

/// Storage v2 accepts no message compression. A request whose `grpc-encoding` names one is
/// refused before its message is read, as the gRPC compression specification asks of an
/// encoding the server does not support: UNIMPLEMENTED naming the encoding, with
/// `grpc-accept-encoding: identity`. Unary and streaming calls are refused alike and write
/// nothing, and the refusal comes before authentication and the routing check; only an
/// unknown method is refused earlier, as unsupported.
#[tokio::test]
async fn grpc_compressed_requests_are_refused_before_the_call() {
    let mut server = Server::start().await;
    let calls = [
        ("CreateBucket", gzipped(&create_bucket("compressed-bucket"))),
        ("WriteObject", gzipped(&write("compressed", b"data"))),
        (
            "BidiWriteObject",
            gzipped(&bidi_write("compressed", b"data")),
        ),
        ("ReadObject", gzipped(&read_object("compressed"))),
    ];
    for (method, message) in calls {
        let reply = server
            .send(call(
                method,
                &[("grpc-encoding", "gzip")],
                messages(&[message]),
            ))
            .await;
        reply.assert_code(Code::Unimplemented);
        assert_eq!(
            reply.status.message(),
            unsupported_encoding("gzip"),
            "{method}"
        );
        assert_eq!(
            reply.headers["grpc-accept-encoding"], "identity",
            "{method}"
        );
        assert!(reply.messages.is_empty(), "{method}");
    }
    assert!(!server.bucket_exists("compressed-bucket").await);
    assert!(server.object("compressed").await.is_none());

    // The header alone decides, whatever encoding it names and however the message is framed.
    for encoding in ["deflate", "zstd", "snappy"] {
        let reply = server
            .send(call(
                "CreateBucket",
                &[("grpc-encoding", encoding)],
                messages(&[plain(&create_bucket("compressed-bucket"))]),
            ))
            .await;
        reply.assert_code(Code::Unimplemented);
        assert_eq!(reply.status.message(), unsupported_encoding(encoding));
        assert_eq!(reply.headers["grpc-accept-encoding"], "identity");
    }
    assert!(!server.bucket_exists("compressed-bucket").await);

    // An unknown token and a conflicting routed bucket each refuse the call, yet beside a
    // compressed message the encoding is refused first.
    let refusals = [
        (
            ("authorization", "Bearer unknown-token"),
            Code::Unauthenticated,
        ),
        (
            (
                "x-goog-request-params",
                "bucket=projects%2F_%2Fbuckets%2Fother-bucket",
            ),
            Code::InvalidArgument,
        ),
    ];
    let request = || messages(&[plain(&get_object("compressed"))]);
    for (metadata, code) in refusals {
        let reply = server.send(call("GetObject", &[metadata], request())).await;
        reply.assert_code(code);
        let compressed = [metadata, ("grpc-encoding", "gzip")];
        let reply = server.send(call("GetObject", &compressed, request())).await;
        reply.assert_code(Code::Unimplemented);
        assert_eq!(reply.status.message(), unsupported_encoding("gzip"));
    }

    // An unknown method never reaches tonic, so it is refused as unsupported, with ErrorInfo.
    let reply = server
        .send(call(
            "ComposeEverything",
            &[("grpc-encoding", "gzip")],
            request(),
        ))
        .await;
    reply.assert_code(Code::Unimplemented);
    assert!(
        reply.status.message().contains("unknown service or method"),
        "{:?}",
        reply.status
    );
    assert!(!reply.status.details().is_empty());
    assert!(!reply.headers.contains_key("grpc-accept-encoding"));

    // Uncompressed, the same writes are served, so the encoding alone refused them.
    let writes = [
        ("CreateBucket", plain(&create_bucket("compressed-bucket"))),
        ("WriteObject", plain(&write("compressed", b"data"))),
        ("BidiWriteObject", plain(&bidi_write("compressed", b"data"))),
    ];
    for (method, message) in writes {
        let reply = server.send(call(method, &[], messages(&[message]))).await;
        reply.assert_code(Code::Ok);
    }
    assert!(server.bucket_exists("compressed-bucket").await);
    assert!(server.object("compressed").await.is_some());
    server.state.catalog.close().await.unwrap();
}

/// A message flagged as compressed in a call that names no encoding breaks the gRPC protocol,
/// which asks for INTERNAL. The call writes nothing.
#[tokio::test]
async fn grpc_compressed_flag_without_an_encoding_is_a_protocol_error() {
    let mut server = Server::start().await;
    let calls = [
        ("CreateBucket", gzipped(&create_bucket("flagged-bucket"))),
        ("WriteObject", gzipped(&write("flagged", b"data"))),
        ("BidiWriteObject", gzipped(&bidi_write("flagged", b"data"))),
    ];
    for (method, message) in calls {
        let reply = server.send(call(method, &[], messages(&[message]))).await;
        reply.assert_code(Code::Internal);
        assert_eq!(
            reply.status.message(),
            "protocol error: received message with compressed-flag but no grpc-encoding was specified",
            "{method}"
        );
    }
    assert!(!server.bucket_exists("flagged-bucket").await);
    assert!(server.object("flagged").await.is_none());

    // Unflagged, the same writes are served, so the flag alone refused them.
    let writes = [
        ("CreateBucket", plain(&create_bucket("flagged-bucket"))),
        ("WriteObject", plain(&write("flagged", b"data"))),
        ("BidiWriteObject", plain(&bidi_write("flagged", b"data"))),
    ];
    for (method, message) in writes {
        let reply = server.send(call(method, &[], messages(&[message]))).await;
        reply.assert_code(Code::Ok);
    }
    assert!(server.bucket_exists("flagged-bucket").await);
    assert!(server.object("flagged").await.is_some());
    server.state.catalog.close().await.unwrap();
}

/// Responses are never compressed: a client that accepts gzip gets plain messages and no
/// `grpc-encoding`, which the gRPC compression specification allows. A request that names the
/// identity encoding is served as an uncompressed one.
#[tokio::test]
async fn grpc_responses_stay_uncompressed_whatever_the_client_accepts() {
    let mut server = Server::start().await;
    let reply = server
        .send(call(
            "WriteObject",
            &[("grpc-encoding", "identity")],
            messages(&[plain(&write("plain", b"uncompressed"))]),
        ))
        .await;
    reply.assert_code(Code::Ok);
    let calls = [
        ("ReadObject", plain(&read_object("plain"))),
        ("GetObject", plain(&get_object("plain"))),
    ];
    for (method, request) in calls {
        let reply = server
            .send(call(
                method,
                &[("grpc-accept-encoding", "gzip, deflate, zstd")],
                messages(&[request]),
            ))
            .await;
        reply.assert_code(Code::Ok);
        assert!(!reply.headers.contains_key("grpc-encoding"), "{method}");
        assert!(!reply.messages.is_empty(), "{method}");
    }
    server.state.catalog.close().await.unwrap();
}
