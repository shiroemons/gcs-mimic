use crate::csek::CsekError;
use hyper::StatusCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum XmlCode {
    InvalidArgument(StatusCode),
    InvalidAuthentication,
    AuthenticationRequired,
    InvalidAccessKeyId,
    SignatureDoesNotMatch,
    ExpiredToken,
    RequestTimeTooSkewed,
    ServiceUnavailable,
    InvalidRange,
    NoSuchBucket,
    NoSuchKey,
    NoSuchUpload,
    InvalidPart,
    InvalidPartOrder,
    MalformedXml,
    InvalidDigest,
    BadDigest,
    ExcessHeaderValues,
    UnexpectedContent,
    MissingContentLength,
    MetadataTooLarge,
    CrcMismatch,
    EntityTooLarge,
    IncompleteBody,
    InvalidPolicyDocument,
    EntityTooSmall,
    BucketNotEmpty,
    BucketNameUnavailable,
    RetentionPolicyNotMet,
    ObjectUnderActiveHold,
    AccessDenied,
    PreconditionFailed,
    UploadBrokenConnection,
    Csek(CsekError),
}

impl XmlCode {
    pub(super) fn status(self) -> StatusCode {
        match self {
            // The JSON adapter also forwards otherwise unmapped statuses under this code.
            Self::InvalidArgument(status) => status,
            Self::AuthenticationRequired => StatusCode::UNAUTHORIZED,
            Self::InvalidAccessKeyId
            | Self::SignatureDoesNotMatch
            | Self::RequestTimeTooSkewed
            | Self::RetentionPolicyNotMet
            | Self::ObjectUnderActiveHold
            | Self::AccessDenied => StatusCode::FORBIDDEN,
            Self::NoSuchBucket | Self::NoSuchKey | Self::NoSuchUpload => StatusCode::NOT_FOUND,
            Self::BucketNotEmpty | Self::BucketNameUnavailable => StatusCode::CONFLICT,
            Self::UploadBrokenConnection => StatusCode::REQUEST_TIMEOUT,
            Self::MissingContentLength => StatusCode::LENGTH_REQUIRED,
            Self::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            Self::InvalidRange => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument(_) => "InvalidArgument",
            Self::InvalidAuthentication => "InvalidAuthentication",
            Self::AuthenticationRequired => "AuthenticationRequired",
            Self::InvalidAccessKeyId => "InvalidAccessKeyId",
            Self::SignatureDoesNotMatch => "SignatureDoesNotMatch",
            Self::ExpiredToken => "ExpiredToken",
            Self::RequestTimeTooSkewed => "RequestTimeTooSkewed",
            Self::ServiceUnavailable => "ServiceUnavailable",
            Self::InvalidRange => "InvalidRange",
            Self::NoSuchBucket => "NoSuchBucket",
            Self::NoSuchKey => "NoSuchKey",
            Self::NoSuchUpload => "NoSuchUpload",
            Self::InvalidPart => "InvalidPart",
            Self::InvalidPartOrder => "InvalidPartOrder",
            Self::MalformedXml => "MalformedXML",
            Self::InvalidDigest => "InvalidDigest",
            Self::BadDigest => "BadDigest",
            Self::ExcessHeaderValues => "ExcessHeaderValues",
            Self::UnexpectedContent => "UnexpectedContent",
            Self::MissingContentLength => "MissingContentLength",
            Self::MetadataTooLarge => "MetadataTooLarge",
            Self::CrcMismatch => "CrcMismatch",
            Self::EntityTooLarge => "EntityTooLarge",
            Self::IncompleteBody => "IncompleteBody",
            Self::InvalidPolicyDocument => "InvalidPolicyDocument",
            Self::EntityTooSmall => "EntityTooSmall",
            Self::BucketNotEmpty => "BucketNotEmpty",
            Self::BucketNameUnavailable => "BucketNameUnavailable",
            Self::RetentionPolicyNotMet => "RetentionPolicyNotMet",
            Self::ObjectUnderActiveHold => "ObjectUnderActiveHold",
            Self::AccessDenied => "AccessDenied",
            Self::PreconditionFailed => "PreconditionFailed",
            Self::UploadBrokenConnection => "uploadBrokenConnection",
            Self::Csek(error) => error.xml_code(),
        }
    }
}
