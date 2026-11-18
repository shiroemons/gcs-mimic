use super::JsonError;
use crate::bounded_body::read_body;
use crate::storage::{
    Catalog, ResumableError, ResumableOptions, SessionState,
    failure::{BackendFailure, object_unavailable},
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
) -> Result<Cancellation, JsonError> {
    read_body(request.into_body(), 0)
        .await
        .map_err(|_| JsonError::invalid_parameter("Cancellation requires an empty body"))?;
    match catalog.cancel_resumable_session(id, bucket).await {
        Ok(()) => Ok(Cancellation::Cancelled),
        Err(ResumableError::Gone) => completion(catalog, id, bucket).await,
        Err(error) => Err(error.into()),
    }
}

/// The stored response of a session that refused cancellation as no longer active. Completion
/// is terminal, so a session that reads complete now was complete when it refused.
async fn completion(catalog: &Catalog, id: &str, bucket: &str) -> Result<Cancellation, JsonError> {
    let session = catalog.resumable_session(id, bucket).await?;
    match session.state {
        SessionState::Complete => session
            .completion
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
