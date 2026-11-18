//! Storage v2 resumable upload IDs.

use crate::grpc::v2::status::internal_error;
use crate::storage::SessionId;
use tonic::Status;

/// A Storage v2 `upload_id`: `<bucket>/<session>`, where the session names a catalog resumable
/// session. Naming the bucket scopes the session to it: the same session under another bucket
/// finds nothing.
///
/// Only the session's shape is checked here (64 hexadecimal digits of either case). The catalog
/// parses it into a [`SessionId`] after the RPC's permission check, so an ID that names no
/// session, upper-case digits included, is denied to a caller without access and otherwise
/// answered like an ID the service cannot read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::grpc::v2) struct UploadId {
    bucket: String,
    session: String,
}

/// An upload ID StartResumableWrite cannot have issued, with the two answers Cloud Storage gave
/// such IDs (recorded 2026-11-18).
#[derive(Debug, PartialEq, Eq)]
pub(in crate::grpc::v2) enum UnknownUploadId {
    /// The text is no upload ID at all: Cloud Storage answered INVALID_ARGUMENT to
    /// `not-an-upload-id` and to an empty ID. Here that is an ID without a bucket part.
    Malformed(String),
    /// The ID cannot be read: Cloud Storage failed with INTERNAL on an issued ID with a
    /// character appended and on an unissued ID under a bucket's resource name. Here that is a
    /// bucket part followed by anything but a session.
    Unreadable,
}

impl std::fmt::Display for UnknownUploadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(id) => write!(f, "Invalid upload ID '{id}'."),
            Self::Unreadable => f.write_str("unreadable upload ID"),
        }
    }
}

impl std::error::Error for UnknownUploadId {}

impl From<UnknownUploadId> for Status {
    fn from(error: UnknownUploadId) -> Self {
        match error {
            UnknownUploadId::Malformed(_) => Status::invalid_argument(error.to_string()),
            UnknownUploadId::Unreadable => internal_error(),
        }
    }
}

/// What a bucket's resource name starts with; an upload ID a client made up may start with one.
const BUCKET_RESOURCE_PREFIX: &str = "projects/_/buckets/";

impl std::str::FromStr for UploadId {
    type Err = UnknownUploadId;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        let Some((bucket, session)) = id.split_once('/').filter(|(bucket, _)| !bucket.is_empty())
        else {
            return Err(UnknownUploadId::Malformed(id.to_owned()));
        };
        if session.len() != 64 || !session.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(UnknownUploadId::Unreadable);
        }
        Ok(Self {
            bucket: bucket.to_owned(),
            session: session.to_owned(),
        })
    }
}

impl std::fmt::Display for UploadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.bucket, self.session)
    }
}

impl UploadId {
    /// The ID StartResumableWrite issues for a new session (lowercase hexadecimal, as stored).
    pub(in crate::grpc::v2) fn issued(bucket: &str, session: SessionId) -> Self {
        Self {
            bucket: bucket.to_owned(),
            session: session.to_string(),
        }
    }

    pub(in crate::grpc::v2) fn bucket(&self) -> &str {
        &self.bucket
    }

    /// The session part as the client sent it; the catalog's `&str` entry points parse it.
    pub(in crate::grpc::v2) fn session(&self) -> &str {
        &self.session
    }

    /// The bucket part routing compares, taken before the ID is validated: routing runs ahead
    /// of the handler, which answers an ID it cannot use. An ID that starts with a bucket's
    /// resource name routes by that name, as Cloud Storage's routing template does; no issued
    /// ID starts that way, and the handler then answers it as unreadable.
    pub(in crate::grpc::v2) fn routing_bucket(id: &str) -> Option<&str> {
        if let Some(rest) = id.strip_prefix(BUCKET_RESOURCE_PREFIX) {
            let name = rest.split_once('/').map_or(rest, |(name, _)| name);
            return id.get(..BUCKET_RESOURCE_PREFIX.len() + name.len());
        }
        id.split_once('/').map(|(bucket, _)| bucket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_ids_round_trip_and_accept_only_the_issued_shape() {
        let session = "0123456789abcdef".repeat(4);
        let id = format!("bucket/{session}");
        let parsed = id.parse::<UploadId>().unwrap();
        assert_eq!(parsed.bucket(), "bucket");
        assert_eq!(parsed.session(), session);
        assert_eq!(parsed.to_string(), id);
        // Upper-case digits keep their shape; the catalog decides whether they name a session.
        let upper = format!("bucket/{}", session.to_ascii_uppercase());
        assert_eq!(upper.parse::<UploadId>().unwrap().to_string(), upper);
        // Recorded 2026-11-18: text that is no upload ID is INVALID_ARGUMENT and repeats it.
        for rejected in [
            String::new(),
            "b".into(),
            "not-an-upload-id".into(),
            format!("/{session}"),
        ] {
            let error = rejected.parse::<UploadId>().unwrap_err();
            assert_eq!(error, UnknownUploadId::Malformed(rejected.clone()));
            let status = Status::from(error);
            assert_eq!(status.code(), tonic::Code::InvalidArgument, "{rejected}");
            assert_eq!(
                status.message(),
                format!("Invalid upload ID '{rejected}'."),
                "{rejected}"
            );
        }
        // Recorded 2026-11-18: an ID the service cannot read is INTERNAL, as an issued ID with
        // a character appended and an unissued ID under a bucket's resource name were.
        for rejected in [
            "b/".into(),
            "b/not-hex".into(),
            format!("b/{}", &session[1..]),
            format!("b/{session}0"),
            format!("b/{}g", &session[1..]),
            // The bucket part comes first; the session part may hold another slash.
            format!("b/{}/x", &session[2..]),
            "projects/_/buckets/b/unknown-upload-id".into(),
        ] {
            let error = rejected.parse::<UploadId>().unwrap_err();
            assert_eq!(error, UnknownUploadId::Unreadable, "{rejected}");
            let status = Status::from(error);
            assert_eq!(status.code(), tonic::Code::Internal, "{rejected}");
            assert_eq!(
                status.message(),
                "We encountered an internal error. Please try again.",
                "{rejected}"
            );
        }
    }

    #[test]
    fn issued_upload_ids_spell_the_stored_session() {
        let session = "AB".repeat(32).parse::<SessionId>();
        // The catalog stores lowercase digits and takes no other case.
        assert!(session.is_err());
        let session = "ab".repeat(32).parse::<SessionId>().unwrap();
        assert_eq!(
            UploadId::issued("bucket", session).to_string(),
            format!("bucket/{}", "ab".repeat(32))
        );
    }

    #[test]
    fn routing_takes_the_bucket_before_validation() {
        assert_eq!(UploadId::routing_bucket("b/not-hex"), Some("b"));
        assert_eq!(UploadId::routing_bucket("/x"), Some(""));
        assert_eq!(UploadId::routing_bucket("b"), None);
        assert_eq!(UploadId::routing_bucket(""), None);
        // An ID under a bucket's resource name routes by that name.
        assert_eq!(
            UploadId::routing_bucket("projects/_/buckets/b/unknown-upload-id"),
            Some("projects/_/buckets/b")
        );
        assert_eq!(
            UploadId::routing_bucket("projects/_/buckets/b"),
            Some("projects/_/buckets/b")
        );
    }
}
