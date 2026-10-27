use crate::grpc::generated::google::rpc::{ErrorInfo, Status as RpcStatus};
use prost::Message;
use tonic::{Code, Status};

pub(super) fn unsupported_for(method: &str, caller: Option<crate::auth::Caller<'_>>) -> Status {
    if matches!(caller, Some(crate::auth::Caller::Anonymous)) {
        return super::auth_error(crate::auth::AuthFailure::InvalidCredentials);
    }
    unsupported(method)
}

pub fn unsupported(method: &str) -> Status {
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
