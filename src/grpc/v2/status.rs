use crate::grpc::generated::google::rpc::{
    BadRequest, ErrorInfo, Status as RpcStatus, bad_request,
};
use prost::Message;
use tonic::{Code, Status};

/// The gRPC message of a conditional read that matched `if_*_not_match` (recorded on `GetObject`).
const NOT_MODIFIED: &str =
    "The resource you requested has not been modified according to the condition you specified.";

/// The message of the JSON object error for a failed generation or metageneration condition.
const JSON_PRECONDITION_FAILED: &str = "Precondition failed";

pub(crate) fn from_json(mut error: crate::json::JsonError) -> Status {
    match (error.status.as_u16(), error.reason) {
        // The JSON object error words a failed precondition shorter; gRPC carries the recorded
        // text of bucket RPCs. Other 412s (public access prevention, a bucket that is not empty)
        // keep their own message: they are not failed preconditions.
        (412, "conditionNotMet") if error.message == JSON_PRECONDITION_FAILED => {
            error.message = crate::json::PRECONDITION_NOT_HELD.into();
        }
        (304, "notModified") => error.message = NOT_MODIFIED.into(),
        _ => {}
    }
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

/// Object-level permissions are decided only after the catalog selects a generation and its
/// ACL, past the handlers' bucket-stage anonymous mapping; apply JSON's rule to the whole RPC.
pub(crate) fn anonymous(status: Status, caller: Option<crate::auth::Caller<'_>>) -> Status {
    if matches!(caller, Some(crate::auth::Caller::Anonymous))
        && status.code() == Code::PermissionDenied
    {
        from_json(crate::auth::AuthFailure::LoginRequired.into())
    } else {
        status
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
    with_detail(
        Code::Unimplemented,
        message,
        "type.googleapis.com/google.rpc.ErrorInfo",
        info.encode_to_vec(),
    )
}

/// The domain Cloud Storage puts in the `ErrorInfo` of a rejected request. It names the service,
/// not the endpoint the client dialed, so an emulator on another host keeps the same value.
const ERROR_DOMAIN: &str = "storage.googleapis.com";

/// An `INVALID_ARGUMENT` status that carries one `google.rpc.ErrorInfo`, as Cloud Storage does
/// for malformed resource names.
pub(crate) fn invalid_with_reason(message: String, reason: &str) -> Status {
    let info = ErrorInfo {
        reason: reason.into(),
        domain: ERROR_DOMAIN.into(),
        metadata: Default::default(),
    };
    with_detail(
        Code::InvalidArgument,
        message,
        "type.googleapis.com/google.rpc.ErrorInfo",
        info.encode_to_vec(),
    )
}

/// An `INVALID_ARGUMENT` status that carries a `google.rpc.BadRequest` with one field violation
/// whose description repeats the message, as Cloud Storage does for an empty object name.
pub(crate) fn invalid_field(message: &str) -> Status {
    let violation = BadRequest {
        field_violations: vec![bad_request::FieldViolation {
            description: message.into(),
            ..Default::default()
        }],
    };
    with_detail(
        Code::InvalidArgument,
        message.into(),
        "type.googleapis.com/google.rpc.BadRequest",
        violation.encode_to_vec(),
    )
}

fn with_detail(code: Code, message: String, type_url: &str, value: Vec<u8>) -> Status {
    let details = RpcStatus {
        code: code as i32,
        message: message.clone(),
        details: vec![prost_types::Any {
            type_url: type_url.into(),
            value,
        }],
    };
    Status::with_details(code, message, details.encode_to_vec().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::StatusCode;

    fn rpc_status(status: &Status) -> RpcStatus {
        RpcStatus::decode(status.details()).unwrap()
    }

    #[test]
    fn unsupported_errors_keep_their_details() {
        let status = unsupported("example");
        assert_eq!(status.code(), Code::Unimplemented);
        assert_eq!(
            status.message(),
            "gcs-mimic does not support gRPC example; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
        );
        let details = rpc_status(&status);
        assert_eq!(details.code, Code::Unimplemented as i32);
        assert_eq!(details.message, status.message());
        assert_eq!(details.details.len(), 1);
        let detail = &details.details[0];
        assert_eq!(detail.type_url, "type.googleapis.com/google.rpc.ErrorInfo");
        let info = ErrorInfo::decode(detail.value.as_slice()).unwrap();
        assert_eq!(info.reason, "badRequest");
        assert_eq!(info.domain, "global");
        assert_eq!(info.metadata["service"], "gcs-mimic");
        assert_eq!(
            info.metadata["remedy"],
            "Use the JSON API; set STORAGE_EMULATOR_HOST to api_base_url from the server ready event"
        );
    }

    #[test]
    fn precondition_failures_use_the_grpc_wording() {
        let failed = from_json(crate::json::JsonError::new(
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
            "Precondition failed",
        ));
        assert_eq!(failed.code(), Code::FailedPrecondition);
        assert_eq!(failed.message(), crate::json::PRECONDITION_NOT_HELD);
        let unmodified = from_json(crate::json::JsonError::new(
            StatusCode::NOT_MODIFIED,
            "notModified",
            "Not modified",
        ));
        assert_eq!(unmodified.message(), NOT_MODIFIED);
        // A 412 that is not a failed condition keeps its own message.
        let pap = from_json(crate::bucket::CatalogError::PublicAccessPrevention.into());
        assert_eq!(pap.code(), Code::FailedPrecondition);
        assert_eq!(pap.message(), "Public access prevention is enforced");
        let not_empty = from_json(crate::json::JsonError::new(
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
            "bucket must be empty prior to deletion",
        ));
        assert_eq!(
            not_empty.message(),
            "bucket must be empty prior to deletion"
        );
    }

    #[test]
    fn resource_name_errors_carry_their_details() {
        let bucket = crate::grpc::v2::names::bucket_name("x").unwrap_err();
        assert_eq!(bucket.code(), Code::InvalidArgument);
        assert!(bucket.message().starts_with("Invalid bucket name 'x'."));
        let detail = &rpc_status(&bucket).details[0];
        let info = ErrorInfo::decode(detail.value.as_slice()).unwrap();
        assert_eq!(info.reason, "GRPC_INVALID_BUCKET");
        assert_eq!(info.domain, ERROR_DOMAIN);

        let object = crate::grpc::v2::names::object_name("projects/_/buckets/b", "").unwrap_err();
        assert_eq!(object.message(), "No object name");
        let detail = &rpc_status(&object).details[0];
        let violations = BadRequest::decode(detail.value.as_slice()).unwrap();
        assert_eq!(violations.field_violations[0].description, "No object name");
    }
}
