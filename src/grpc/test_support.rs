//! Fixtures shared by gRPC adapters and authorization tests.

use crate::{
    auth::{Auth, Caller, Registry},
    clock::SystemClock,
    server::State,
    storage::Catalog,
};
use std::sync::{Arc, atomic::AtomicBool};
use tonic::{Code, Request, Status};

pub(crate) fn state(catalog: Catalog, auth: Auth) -> State {
    State {
        ready: AtomicBool::new(true),
        clock: Arc::new(SystemClock),
        catalog,
        api_base_url: "http://localhost:4443".into(),
        default_location: "US".into(),
        auth,
    }
}

pub(super) fn registry(entries: &[(&str, &str, &str)]) -> Registry {
    let identities: Vec<_> = entries
        .iter()
        .map(|(token, project, role)| {
            let mut identity =
                serde_json::json!({"id": format!("{token}@example.invalid"), "tokens": [token]});
            if !role.is_empty() {
                identity["project_roles"] = serde_json::json!({(*project): [role]});
            }
            identity
        })
        .collect();
    Registry::parse(&serde_json::to_vec(&serde_json::json!({"identities": identities})).unwrap())
        .unwrap()
}

pub(super) fn caller<'a>(storage: &'a super::v2::Storage, token: &str) -> Option<Caller<'a>> {
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    storage.authenticate(&request).unwrap()
}

#[track_caller]
pub(super) fn assert_code<T>(result: Result<T, Status>, code: Code) -> Status {
    match result {
        Err(status) => {
            assert_eq!(status.code(), code);
            status
        }
        Ok(_) => panic!("expected gRPC status {code:?}, got success"),
    }
}

pub(super) fn mask(paths: &[&str]) -> Option<prost_types::FieldMask> {
    Some(prost_types::FieldMask {
        paths: paths.iter().map(|path| (*path).into()).collect(),
    })
}

pub(super) async fn rpc<M: prost::Message, R: prost::Message + Default>(
    path: &str,
    token: Option<&str>,
    messages: &[M],
    send: impl AsyncFnOnce(hyper::Request<tonic::body::Body>) -> hyper::Response<tonic::body::Body>,
) -> (Code, Vec<R>) {
    use http_body_util::BodyExt;
    let mut body = Vec::new();
    for message in messages {
        let encoded = message.encode_to_vec();
        body.push(0);
        body.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
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
    let request = request
        .body(tonic::body::Body::new(http_body_util::Full::new(
            bytes::Bytes::from(body),
        )))
        .unwrap();
    let response = send(request).await;
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
        let size =
            usize::try_from(u32::from_be_bytes(remaining[1..5].try_into().unwrap())).unwrap();
        assert!(remaining.len() >= 5 + size);
        messages.push(R::decode(&remaining[5..5 + size]).unwrap());
        remaining = &remaining[5 + size..];
    }
    (code.expect("terminal grpc-status"), messages)
}
