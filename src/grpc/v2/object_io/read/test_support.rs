//! Helpers shared by the GetObject, ReadObject and ListObjects tests.

use super::*;
use crate::grpc::test_support::assert_code;

/// The messages of a ReadObject without a caller, as on a permissive server.
pub(super) async fn read(
    storage: &Storage,
    request: pb::ReadObjectRequest,
) -> Result<Vec<pb::ReadObjectResponse>, Status> {
    read_as(storage, request, None).await
}

/// The messages of a ReadObject by `caller`, whether it fails when opened or while streaming.
pub(super) async fn read_as(
    storage: &Storage,
    request: pb::ReadObjectRequest,
    caller: Option<Caller<'_>>,
) -> Result<Vec<pb::ReadObjectResponse>, Status> {
    let mut stream = storage
        .read_object(Request::new(request), caller)
        .await?
        .into_inner();
    let mut messages = Vec::new();
    while let Some(message) = stream.next().await {
        messages.push(message?);
    }
    Ok(messages)
}

/// The object bytes the messages of a ReadObject carry.
pub(super) fn body(messages: &[pb::ReadObjectResponse]) -> Vec<u8> {
    messages
        .iter()
        .filter_map(|message| message.checksummed_data.as_ref())
        .flat_map(|data| data.content.iter().copied())
        .collect()
}

/// A failure with exactly this code and message.
#[track_caller]
pub(super) fn assert_status<T>(result: Result<T, Status>, code: tonic::Code, message: &str) {
    let status = assert_code(result, code);
    assert_eq!(status.message(), message);
}
