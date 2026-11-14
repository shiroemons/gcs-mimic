//! Storage v2 resumable upload IDs.

use crate::storage::SessionId;
use tonic::Status;

/// A Storage v2 `upload_id`: `<bucket>/<session>`, where the session names a catalog resumable
/// session. Naming the bucket scopes the session to it: the same session under another bucket
/// finds nothing.
///
/// Only the session's shape is checked here (64 hexadecimal digits of either case). The catalog
/// parses it into a [`SessionId`] after the RPC's permission check, so an ID that names no
/// session, upper-case digits included, is denied to a caller without access and otherwise
/// reported with the catalog's NOT_FOUND wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::grpc::v2) struct UploadId {
    bucket: String,
    session: String,
}

/// An upload ID StartResumableWrite cannot have issued.
#[derive(Debug)]
pub(in crate::grpc::v2) struct UnknownUploadId;

impl std::fmt::Display for UnknownUploadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unknown upload_id; use the value returned by StartResumableWrite")
    }
}

impl std::error::Error for UnknownUploadId {}

impl From<UnknownUploadId> for Status {
    fn from(error: UnknownUploadId) -> Self {
        Status::not_found(error.to_string())
    }
}

impl std::str::FromStr for UploadId {
    type Err = UnknownUploadId;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        let (bucket, session) = id.split_once('/').ok_or(UnknownUploadId)?;
        if bucket.is_empty()
            || session.len() != 64
            || !session.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(UnknownUploadId);
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
    /// of the handler, which answers a malformed ID with NOT_FOUND.
    pub(in crate::grpc::v2) fn routing_bucket(id: &str) -> Option<&str> {
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
        for rejected in [
            String::new(),
            "b".into(),
            "b/".into(),
            "b/not-hex".into(),
            format!("/{session}"),
            format!("b/{}", &session[1..]),
            format!("b/{session}0"),
            format!("b/{}g", &session[1..]),
        ] {
            let error = rejected.parse::<UploadId>().unwrap_err();
            assert_eq!(
                Status::from(error).message(),
                "Unknown upload_id; use the value returned by StartResumableWrite",
                "{rejected}"
            );
        }
        // The bucket part comes first; the session part may hold another slash, which fails.
        assert!(
            format!("b/{}/x", &session[2..])
                .parse::<UploadId>()
                .is_err()
        );
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
    }
}
