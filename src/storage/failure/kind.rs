use std::{error::Error, fmt};

/// Fixed storage failures whose identity must survive contextual error messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendFailure {
    CatalogClosed,
    OperationLimit,
    UploadWaitTimeout,
    RetainedSessionLimit,
    OutboxLimit,
    FinalizationTimeout,
    CopyDeadline,
    SessionBusy,
    MissingCompletion,
}

impl BackendFailure {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::CatalogClosed => "catalog is closed",
            Self::OperationLimit => "catalog operation limit reached",
            Self::UploadWaitTimeout => "upload catalog wait timed out",
            Self::RetainedSessionLimit => "retained upload session limit reached",
            Self::OutboxLimit => "notification outbox limit reached",
            Self::FinalizationTimeout => "upload finalization timed out",
            Self::CopyDeadline => "copy request deadline exceeded",
            Self::SessionBusy => "upload session is busy",
            Self::MissingCompletion => "Completed upload session has no stored response",
        }
    }

    pub(crate) fn guidance(self) -> Option<&'static str> {
        Some(match self {
            Self::CatalogClosed => {
                "Object storage is unavailable: catalog is closed; wait for the server to restart before retrying"
            }
            Self::OperationLimit => {
                "Object storage is unavailable: catalog operation limit reached; retry with fewer concurrent requests (for example, 16)"
            }
            Self::UploadWaitTimeout => {
                "Object storage is unavailable: upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 300000; 0 disables it); a completed upload may still commit"
            }
            Self::RetainedSessionLimit => {
                "Object storage is unavailable: retained upload session limit reached; wait for expired sessions to be reclaimed or use single-request media uploads"
            }
            Self::OutboxLimit => {
                "Object storage is unavailable: notification outbox limit reached; start or repair the configured local Pub/Sub emulator, or set pubsub_emulator_host to resume delivery, then retry with generation preconditions"
            }
            Self::FinalizationTimeout => {
                "Object storage is unavailable: upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)"
            }
            Self::CopyDeadline => {
                "Object storage is unavailable: copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)"
            }
            Self::SessionBusy => {
                "Object storage is unavailable: upload session is busy; wait for the current request to finish before sending another request for this session"
            }
            Self::MissingCompletion => return None,
        })
    }
}

impl fmt::Display for BackendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl Error for BackendFailure {}
