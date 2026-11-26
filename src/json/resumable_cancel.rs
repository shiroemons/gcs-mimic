use super::ApiError;
use crate::bounded_body::read_body;
use crate::cause::BackendFailure;
use crate::storage::{
    Catalog, ResumableError, ResumableOptions, SessionState, failure::object_unavailable,
};
use hyper::{Request, body::Body};
use serde_json::Value;

/// What a DELETE of a session URI did.
pub(crate) enum Cancellation {
    /// The active session is cancelled.
    Cancelled,
    /// The session had already completed and keeps its object. Cloud Storage answered the
    /// DELETE with 200 and the stored object resource, as it answers a status request
    /// (recorded 2026-11-17, case obj-gaps-uploads). The session's options shape that response
    /// as they shape every other response of the completed session.
    Completed {
        resource: Value,
        options: Box<ResumableOptions>,
    },
}

pub(crate) async fn cancel_response<B: Body + Unpin>(
    request: Request<B>,
    bucket: &str,
    id: &str,
    catalog: &Catalog,
) -> Result<Cancellation, ApiError> {
    read_body(request.into_body(), 0)
        .await
        .map_err(|_| ApiError::invalid_parameter("Cancellation requires an empty body"))?;
    match catalog.cancel_resumable_session(id, bucket).await {
        Ok(()) => Ok(Cancellation::Cancelled),
        Err(ResumableError::Gone) => completion(catalog, id, bucket).await,
        Err(error) => Err(error.into()),
    }
}

/// The stored response of a session that refused cancellation as no longer active. Completion
/// is terminal, so a session that reads complete now was complete when it refused.
async fn completion(catalog: &Catalog, id: &str, bucket: &str) -> Result<Cancellation, ApiError> {
    let session = catalog.resumable_session(id, bucket).await?;
    match session.state {
        SessionState::Complete(completion) => completion
            .into_value()
            .map(|resource| Cancellation::Completed {
                resource,
                options: Box::new(session.options),
            })
            .ok_or_else(|| object_unavailable(BackendFailure::MissingCompletion).into()),
        SessionState::Active
        | SessionState::Cancelled
        | SessionState::Aborted
        | SessionState::InvalidContentRange
        | SessionState::Overrun(_) => Err(ResumableError::Gone.into()),
    }
}

#[cfg(test)]
pub(crate) async fn cancel_wire(catalog: &Catalog, id: &str, body: &[u8]) -> u16 {
    use crate::json::JsonErrorResponse;
    use http_body_util::BodyExt;
    use hyper::{body::Incoming, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use std::{convert::Infallible, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let catalog = catalog.clone();
    let server_id = id.to_owned();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut connection = http1::Builder::new().serve_connection(
            TokioIo::new(stream),
            service_fn(move |request: Request<Incoming>| {
                let catalog = catalog.clone();
                let id = server_id.clone();
                async move {
                    let response = match cancel_response(request, "bucket", &id, &catalog).await {
                        Ok(Cancellation::Cancelled) => {
                            ApiError::from(ResumableError::Cancelled).into_response()
                        }
                        Ok(Cancellation::Completed { resource, .. }) => {
                            hyper::Response::new(http_body_util::Full::new(
                                hyper::body::Bytes::from(resource.to_string()),
                            ))
                        }
                        Err(error) => error.into_response(),
                    };
                    Ok::<_, Infallible>(response.map(BodyExt::boxed_unsync))
                }
            }),
        );
        (&mut connection).await.unwrap();
        let mut stream = connection.into_parts().io.into_inner();
        stream.shutdown().await.unwrap();
        // Match the production server's bounded unread-body disposal after early rejection.
        let _ = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::io::copy(&mut stream.take(256 * 1024), &mut tokio::io::sink()),
        )
        .await;
    });
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("DELETE /upload/storage/v1/b/bucket/o?upload_id={id} HTTP/1.1\r\nHost: local\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    task.await.unwrap();
    std::str::from_utf8(&response)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}
