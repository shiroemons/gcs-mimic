use super::{names, object_io::UploadId, pb, status};
use crate::json;
use tonic::{Status, metadata::MetadataMap};

/// A routing field Storage v2 accepts in `x-goog-request-params`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoutingField {
    Bucket,
    SourceBucket,
    Project,
    Appendable,
}

/// A routing key Storage v2 does not accept.
#[derive(Debug)]
struct UnknownRoutingField;

impl std::fmt::Display for UnknownRoutingField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unknown routing metadata field")
    }
}

impl std::error::Error for UnknownRoutingField {}

impl std::str::FromStr for RoutingField {
    type Err = UnknownRoutingField;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        match key {
            "bucket" => Ok(Self::Bucket),
            "source_bucket" => Ok(Self::SourceBucket),
            "project" => Ok(Self::Project),
            "appendable" => Ok(Self::Appendable),
            _ => Err(UnknownRoutingField),
        }
    }
}

/// A key the Go SDK may send that routes nothing.
const DIAGNOSTIC_KEY: &str = "direct_connectivity_diagnostic";

/// A resource the request names, compared with the same routing field.
#[derive(Clone, Copy, Debug)]
pub(super) enum Route<'a> {
    /// A resource name `projects/_/buckets/<id>` or a bare ID resolved from a token or an
    /// upload ID; `None` when the request names no bucket, which no routed bucket matches.
    Bucket(Option<&'a str>),
    /// Compared like [`Route::Bucket`].
    SourceBucket(Option<&'a str>),
    /// Compared as text; proto3's empty string is a value here.
    Project(&'a str),
    /// Routed as `true` or `false`.
    Appendable(bool),
}

/// The routing fields a call accepts and what it compares them with.
#[derive(Clone, Copy, Debug)]
pub(super) enum Routes<'a> {
    /// Each routed field must be listed and name the same resource.
    Request(&'a [Route<'a>]),
    /// The handler learns its resources later (a stream's first message, a rewrite token):
    /// only the field is checked here, and the handler checks again with `Request`.
    Deferred,
}

impl Route<'_> {
    fn field(self) -> RoutingField {
        match self {
            Self::Bucket(_) => RoutingField::Bucket,
            Self::SourceBucket(_) => RoutingField::SourceBucket,
            Self::Project(_) => RoutingField::Project,
            Self::Appendable(_) => RoutingField::Appendable,
        }
    }

    fn matches(self, routed: &str) -> Result<bool, Status> {
        match self {
            Self::Bucket(expected) | Self::SourceBucket(expected) => {
                let routed = names::bucket_name(routed)?;
                Ok(expected.is_some_and(|expected| {
                    names::bucket_name(expected).unwrap_or(expected) == routed
                }))
            }
            Self::Project(expected) => Ok(routed == expected),
            Self::Appendable(expected) => Ok(routed
                .parse::<bool>()
                .is_ok_and(|routed| routed == expected)),
        }
    }
}

pub(super) fn check(metadata: &MetadataMap, routes: Routes<'_>) -> Result<(), Status> {
    // `x-goog-user-project` names the Requester Pays billing project. A gcs-mimic bucket cannot
    // enable Requester Pays, and Cloud Storage served the JSON parameter on a bucket and a
    // project without it as if it were absent (cases bkt-edges-hmac-keys and
    // bkt-edges-notification-ids), so the metadata is ignored here too.
    for metadata in metadata.get_all("x-goog-request-params").iter() {
        let value = metadata.to_str().map_err(|_| {
            Status::invalid_argument(
                "Invalid routing metadata; use bucket=projects%2F_%2Fbuckets%2Fexample-bucket",
            )
        })?;
        for parameter in value.split('&') {
            let (key, value) = parameter.split_once('=').ok_or_else(|| {
                Status::invalid_argument("Routing metadata must contain key=value pairs")
            })?;
            let key = json::decode(key, json::Decoding::Form)?;
            let value = json::decode(value, json::Decoding::Form)?;
            let field = key.parse::<RoutingField>();
            if matches!(field, Ok(RoutingField::Bucket)) && names::bucket_name(&value).is_err() {
                return Err(status::invalid_with_reason(
                    format!(
                        "Invalid bucket name found in x-goog-request-params metadata (bucket={value}). Must be of the form `projects/_/buckets/{{bucket_id}}`"
                    ),
                    "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT",
                ));
            }
            if key == DIAGNOSTIC_KEY {
                continue;
            }
            let unsupported = || status::unsupported("routing metadata field");
            let field = field.map_err(|UnknownRoutingField| unsupported())?;
            let route = match routes {
                // Stream resources arrive in their first protobuf message.
                Routes::Deferred => match field {
                    RoutingField::Bucket
                    | RoutingField::SourceBucket
                    | RoutingField::Appendable => {
                        continue;
                    }
                    RoutingField::Project => return Err(unsupported()),
                },
                Routes::Request(routes) => *routes
                    .iter()
                    .find(|route| route.field() == field)
                    .ok_or_else(unsupported)?,
            };
            if !route.matches(&value)? {
                return Err(Status::invalid_argument(
                    "Routing metadata conflicts with the request; use the same resource in x-goog-request-params and the request",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn iam_bucket(resource: &str) -> &str {
    resource
        .split_once("/managedFolders/")
        .or_else(|| resource.split_once("/objects/"))
        .map_or(resource, |(bucket, _)| bucket)
}

pub(super) fn write(
    metadata: &MetadataMap,
    first: Option<&pb::bidi_write_object_request::FirstMessage>,
) -> Result<(), Status> {
    use pb::bidi_write_object_request::FirstMessage;
    let (bucket, appendable) = match first {
        Some(FirstMessage::WriteObjectSpec(spec)) => (
            spec.resource.as_ref().map(|r| r.bucket.as_str()),
            spec.appendable == Some(true),
        ),
        Some(FirstMessage::AppendObjectSpec(spec)) => (Some(spec.bucket.as_str()), true),
        Some(FirstMessage::UploadId(id)) => (UploadId::routing_bucket(id), false),
        None => (None, false),
    };
    check(
        metadata,
        Routes::Request(&[Route::Bucket(bucket), Route::Appendable(appendable)]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_validates_all_values_and_resumed_upload_buckets() {
        let mut metadata = MetadataMap::new();
        metadata.insert(
            "x-goog-request-params",
            "bucket=projects%2F_%2Fbuckets%2Ftest-bucket&appendable=false"
                .parse()
                .unwrap(),
        );
        let first = pb::bidi_write_object_request::FirstMessage::UploadId(format!(
            "test-bucket/{}",
            "a".repeat(64)
        ));
        write(&metadata, Some(&first)).unwrap();
        let wrong = pb::bidi_write_object_request::FirstMessage::UploadId(format!(
            "other-bucket/{}",
            "a".repeat(64)
        ));
        assert_eq!(
            write(&metadata, Some(&wrong)).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            check(&metadata, Routes::Request(&[Route::Project("projects/p")]))
                .unwrap_err()
                .code(),
            tonic::Code::Unimplemented
        );
        metadata.insert("x-goog-request-params", "bucket=bad".parse().unwrap());
        let error = check(&metadata, Routes::Deferred).unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(!error.details().is_empty());
    }

    fn routed(value: &'static str) -> MetadataMap {
        let mut metadata = MetadataMap::new();
        metadata.insert("x-goog-request-params", value.parse().unwrap());
        metadata
    }

    const CONFLICT: &str = "Routing metadata conflicts with the request; use the same resource in x-goog-request-params and the request";

    #[test]
    fn routing_compares_each_field_with_its_own_rule() {
        let spec = |appendable| {
            pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(pb::WriteObjectSpec {
                resource: Some(pb::Object {
                    bucket: "projects/_/buckets/b".into(),
                    ..Default::default()
                }),
                appendable,
                ..Default::default()
            })
        };
        let appendable = routed("bucket=projects%2F_%2Fbuckets%2Fb&appendable=true");
        write(&appendable, Some(&spec(Some(true)))).unwrap();
        for other in [None, Some(false)] {
            assert_eq!(
                write(&appendable, Some(&spec(other)))
                    .unwrap_err()
                    .message(),
                CONFLICT
            );
        }
        // Only `true` and `false` spell a routed flag.
        let spelled = routed("bucket=projects%2F_%2Fbuckets%2Fb&appendable=TRUE");
        assert_eq!(
            write(&spelled, Some(&spec(Some(true))))
                .unwrap_err()
                .message(),
            CONFLICT
        );

        // A bucket the request does not name matches no routed bucket.
        let bucket = routed("bucket=projects%2F_%2Fbuckets%2Fb");
        assert_eq!(write(&bucket, None).unwrap_err().message(), CONFLICT);
        // A malformed upload ID still routes by the text before its first slash; the handler
        // answers it with NOT_FOUND.
        let malformed = pb::bidi_write_object_request::FirstMessage::UploadId("b/not-hex".into());
        write(&bucket, Some(&malformed)).unwrap();

        // The Go SDK's diagnostic key routes nothing.
        let diagnostic =
            routed("direct_connectivity_diagnostic=1&bucket=projects%2F_%2Fbuckets%2Fb");
        check(&diagnostic, Routes::Request(&[Route::Bucket(Some("b"))])).unwrap();
        check(&diagnostic, Routes::Deferred).unwrap();
    }

    /// The emulator never redirects, so it never issues a routing token a client could repeat:
    /// that key, like any other key no Storage v2 RPC routes, is refused whether the request
    /// names its resources or they arrive later.
    #[test]
    fn routing_refuses_keys_no_rpc_routes() {
        let routes = [Route::Bucket(Some("b"))];
        for params in [
            "routing_token=token",
            "bucket=projects%2F_%2Fbuckets%2Fb&routing_token=token",
            "unknown=1",
        ] {
            for checked in [Routes::Request(&routes), Routes::Deferred] {
                crate::grpc::test_support::assert_unsupported(
                    check(&routed(params), checked),
                    "routing metadata field",
                );
            }
        }
    }

    #[test]
    fn deferred_routing_checks_only_the_field() {
        assert_eq!(
            check(&routed("project=projects%2Fp"), Routes::Deferred)
                .unwrap_err()
                .code(),
            tonic::Code::Unimplemented
        );
        // Without the request's resources, a source bucket is neither parsed nor compared.
        check(
            &routed("source_bucket=bad&appendable=maybe"),
            Routes::Deferred,
        )
        .unwrap();
        let error = check(
            &routed("source_bucket=bad"),
            Routes::Request(&[Route::SourceBucket(Some("b")), Route::Bucket(Some("b"))]),
        )
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(
            error.message().starts_with("Invalid bucket name 'bad'."),
            "{error}"
        );
        assert!(!error.details().is_empty());
        assert_eq!(
            check(
                &routed("source_bucket=projects%2F_%2Fbuckets%2Fa"),
                Routes::Request(&[Route::SourceBucket(Some("b")), Route::Bucket(Some("b"))]),
            )
            .unwrap_err()
            .message(),
            CONFLICT
        );
        check(
            &routed("source_bucket=projects%2F_%2Fbuckets%2Fa&bucket=projects%2F_%2Fbuckets%2Fb"),
            Routes::Request(&[
                Route::SourceBucket(Some("a")),
                Route::Bucket(Some("projects/_/buckets/b")),
            ]),
        )
        .unwrap();
    }
}
