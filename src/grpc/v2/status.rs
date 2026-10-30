use crate::grpc::generated::google::rpc::{ErrorInfo, Status as RpcStatus};
use prost::Message;
use tonic::{Code, Status};

pub(crate) fn from_json(error: crate::json::JsonError) -> Status {
    if error.message.starts_with("gcs-mimic does not support ") {
        return unsupported(&error.message);
    }
    match error.status.as_u16() {
        400 => Status::invalid_argument(error.message),
        401 => Status::unauthenticated(error.message),
        403 => Status::permission_denied(error.message),
        404 => Status::not_found(error.message),
        409 => Status::already_exists(error.message),
        304 | 410 | 412 => Status::failed_precondition(error.message),
        413 | 429 => Status::resource_exhausted(error.message),
        416 => Status::out_of_range(error.message),
        499 => Status::cancelled(error.message),
        501 => unsupported(&error.message),
        503 => Status::unavailable(error.message),
        504 => Status::deadline_exceeded(error.message),
        _ => Status::internal(error.message),
    }
}

pub(super) fn unsupported_for(method: &str, caller: Option<crate::auth::Caller<'_>>) -> Status {
    if matches!(caller, Some(crate::auth::Caller::Anonymous)) {
        return super::auth_error(crate::auth::AuthFailure::InvalidCredentials);
    }
    unsupported(method)
}

pub(crate) fn unsupported(method: &str) -> Status {
    let message = format!(
        "gcs-mimic does not support gRPC {method}; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
    );
    let info = ErrorInfo {
        // Preserve the existing unsupported JSON contract's reason and domain.
        reason: "badRequest".into(),
        domain: "global".into(),
        metadata: [
            ("service".into(), "gcs-mimic".into()),
            (
                "remedy".into(),
                "Use the JSON API; set STORAGE_EMULATOR_HOST to api_base_url from the server ready event".into(),
            ),
        ]
        .into(),
    };
    let details = RpcStatus {
        code: Code::Unimplemented as i32,
        message: message.clone(),
        details: vec![prost_types::Any {
            type_url: "type.googleapis.com/google.rpc.ErrorInfo".into(),
            value: info.encode_to_vec(),
        }],
    };
    Status::with_details(Code::Unimplemented, message, details.encode_to_vec().into())
}
