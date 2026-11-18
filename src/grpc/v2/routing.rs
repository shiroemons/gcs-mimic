use super::{names, pb, status};
use crate::json;
use tonic::{Status, metadata::MetadataMap};

/// A routing field Storage v2 compares with the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoutingField {
    Bucket,
    SourceBucket,
    Project,
    Appendable,
}

/// A routing key no Storage v2 RPC compares with its request.
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

/// A resource the request names, compared with the same routing field.
#[derive(Clone, Copy, Debug)]
pub(super) enum Route<'a> {
    /// A resource name `projects/_/buckets/<id>` or a bare ID resolved from a token; `None`
    /// when the request names no bucket. The metadata must route the same bucket: metadata
    /// that routes none is compared as routing the empty name.
    Bucket(Option<&'a str>),
    /// Compared with the routed source bucket when the metadata routes one.
    SourceBucket(Option<&'a str>),
    /// Compared as text when routed; proto3's empty string is a value here.
    Project(&'a str),
    /// Routed as `true` or `false`.
    Appendable(bool),
}

/// The routing fields a call compares and whether [`admit`] needs the metadata at all.
#[derive(Clone, Copy, Debug)]
pub(super) enum Routes<'a> {
    /// The metadata is required, and each listed field must name the same resource.
    Request(&'a [Route<'a>]),
    /// The call is admitted with or without the metadata, and nothing is compared. Cloud
    /// Storage answered QueryWriteStatus and CancelResumableWrite sent without it for their
    /// malformed upload ID (recorded 2026-11-18, case grpc-probe-resumable-sessions, steps
    /// cancel-malformed, status-malformed and cancel-empty), and sent with a routed bucket equal
    /// to the one an upload ID starts with for that ID too (steps cancel-unknown-under-bucket and
    /// status-unknown-under-bucket): the answer came from the ID, not from a comparison. A local
    /// upload ID `<bucket>/<session>` does not match the proto's routing template
    /// `{bucket=projects/*/buckets/*}/**`, so a client routes no bucket from it, and the
    /// handlers authorize with the bucket the parsed ID names.
    Optional,
    /// The metadata is required, but the handler learns its resources later (a stream's first
    /// message, a rewrite token): only the metadata's form is checked, and the handler calls
    /// [`check`] again with `Request`.
    Deferred,
}

const CONFLICT: &str = "Routing metadata conflicts with the request; use the same resource in x-goog-request-params and the request";

/// Cloud Storage's answer to a routed bucket other than the request's (recorded 2026-11-18,
/// case grpc-probe-routing); both names are bare bucket IDs.
fn other_bucket(requested: &str, routed: &str) -> Status {
    status::invalid_with_reason(
        format!(
            "The x-goog-request-params must specify the same bucket as the resource in the request. Request bucket was '{requested}' but x-goog-request-params specified '{routed}'"
        ),
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET",
    )
}

/// The bucket ID of a resource name, of a bare ID, or of no bucket at all.
fn bucket_id(name: Option<&str>) -> &str {
    name.map_or("", |name| names::bucket_name(name).unwrap_or(name))
}

fn same_bucket(requested: Option<&str>, routed: Option<&str>) -> Result<(), Status> {
    let (requested, routed) = (bucket_id(requested), bucket_id(routed));
    if requested == routed {
        Ok(())
    } else {
        Err(other_bucket(requested, routed))
    }
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

    /// Compares the routed value, `None` when the metadata does not route this field.
    fn compare(self, routed: Option<&str>) -> Result<(), Status> {
        let Some(routed) = routed else {
            return match self {
                Self::Bucket(requested) => same_bucket(requested, None),
                Self::SourceBucket(_) | Self::Project(_) | Self::Appendable(_) => Ok(()),
            };
        };
        match self {
            Self::Bucket(requested) => same_bucket(requested, Some(routed)),
            Self::SourceBucket(requested) => {
                let routed = names::bucket_name(routed)?;
                requested
                    .is_some_and(|requested| bucket_id(Some(requested)) == routed)
                    .then_some(())
                    .ok_or_else(conflict)
            }
            Self::Project(requested) => (routed == requested).then_some(()).ok_or_else(conflict),
            Self::Appendable(requested) => routed
                .parse::<bool>()
                .is_ok_and(|routed| routed == requested)
                .then_some(())
                .ok_or_else(conflict),
        }
    }
}

/// The local answer to a routed value other than the request's, for the fields whose Cloud
/// Storage wording is not recorded.
fn conflict() -> Status {
    Status::invalid_argument(CONFLICT)
}

/// The compared fields of every `x-goog-request-params` value, in the order sent; `None` when
/// the request carries no such metadata.
fn routed(metadata: &MetadataMap) -> Result<Option<Vec<(RoutingField, String)>>, Status> {
    let mut values = metadata.get_all("x-goog-request-params").iter().peekable();
    if values.peek().is_none() {
        return Ok(None);
    }
    let mut fields = Vec::new();
    for metadata in values {
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
            // Cloud Storage served a key no RPC routes, and a `routing_token` no redirect
            // issued, as if it were absent (recorded 2026-11-18, case grpc-probe-routing).
            let Ok(field) = key.parse::<RoutingField>() else {
                continue;
            };
            if field == RoutingField::Bucket && names::bucket_name(&value).is_err() {
                return Err(status::invalid_with_reason(
                    format!(
                        "Invalid bucket name found in x-goog-request-params metadata (bucket={value}). Must be of the form `projects/_/buckets/{{bucket_id}}`"
                    ),
                    "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT",
                ));
            }
            fields.push((field, value));
        }
    }
    Ok(Some(fields))
}

/// The check every RPC passes before its handler runs: the metadata must be sent, unless the
/// call's routes are `Optional`, and must agree with the request.
pub(super) fn admit(metadata: &MetadataMap, routes: Routes<'_>) -> Result<(), Status> {
    let required = match routes {
        Routes::Request(_) | Routes::Deferred => true,
        Routes::Optional => false,
    };
    if required && metadata.get("x-goog-request-params").is_none() {
        // Recorded 2026-11-18 on GetObject, GetBucket, ListBuckets and WriteObject (case
        // grpc-probe-routing).
        return Err(status::invalid_with_reason(
            "An x-goog-request-params request metadata property must be provided for this request.",
            "GRPC_INVALID_X_GOOG_REQUEST_PARAMS",
        ));
    }
    check(metadata, routes)
}

/// Compares the metadata a request carries with its resources. A handler that learns its
/// resources after `admit` calls this again; a request without the metadata passes here.
pub(super) fn check(metadata: &MetadataMap, routes: Routes<'_>) -> Result<(), Status> {
    // `x-goog-user-project` names the Requester Pays billing project. A gcs-mimic bucket cannot
    // enable Requester Pays, and Cloud Storage served the JSON parameter on a bucket and a
    // project without it as if it were absent (cases bkt-edges-hmac-keys and
    // bkt-edges-notification-ids), so the metadata is ignored here too.
    let Some(routed) = routed(metadata)? else {
        return Ok(());
    };
    let routes = match routes {
        Routes::Request(routes) => routes,
        // Stream resources arrive in their first protobuf message.
        Routes::Optional | Routes::Deferred => return Ok(()),
    };
    // A routed field the call does not list is not compared: Cloud Storage answered a routed
    // project on GetObject for the bucket the metadata left out (recorded 2026-11-18).
    for route in routes {
        let mut values = routed
            .iter()
            .filter(|(field, _)| *field == route.field())
            .map(|(_, value)| value.as_str())
            .peekable();
        if values.peek().is_none() {
            route.compare(None)?;
        }
        for value in values {
            route.compare(Some(value))?;
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
            Route::Bucket(spec.resource.as_ref().map(|r| r.bucket.as_str())),
            spec.appendable == Some(true),
        ),
        Some(FirstMessage::AppendObjectSpec(spec)) => {
            (Route::Bucket(Some(spec.bucket.as_str())), true)
        }
        // A resumed write's upload ID names its bucket in a form no client routes (see
        // `Routes::Optional`), and the handler authorizes with that bucket, so only the flag is
        // compared.
        Some(FirstMessage::UploadId(_)) => {
            return check(metadata, Routes::Request(&[Route::Appendable(false)]));
        }
        None => (Route::Bucket(None), false),
    };
    check(
        metadata,
        Routes::Request(&[bucket, Route::Appendable(appendable)]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    fn routed(value: &'static str) -> MetadataMap {
        let mut metadata = MetadataMap::new();
        metadata.insert("x-goog-request-params", value.parse().unwrap());
        metadata
    }

    /// The ErrorInfo reason of a refused request.
    fn reason(error: &Status) -> String {
        use crate::grpc::generated::google::rpc::{ErrorInfo, Status as RpcStatus};
        use prost::Message as _;
        let status = RpcStatus::decode(error.details()).unwrap();
        ErrorInfo::decode(status.details[0].value.as_slice())
            .unwrap()
            .reason
    }

    fn other_bucket_message(requested: &str, routed: &str) -> String {
        format!(
            "The x-goog-request-params must specify the same bucket as the resource in the request. Request bucket was '{requested}' but x-goog-request-params specified '{routed}'"
        )
    }

    /// A resumed write's upload ID names its bucket in a form no client routes, so its routed
    /// bucket is not compared with the ID's: the handler authorizes with the ID's bucket. No
    /// resumed write was recorded under another routed bucket; comparing nothing is the local
    /// reading of the recorded upload ID calls (case grpc-probe-resumable-sessions).
    #[test]
    fn routing_compares_only_the_appendable_flag_of_a_resumed_write() {
        let resumed = |bucket: &str| {
            pb::bidi_write_object_request::FirstMessage::UploadId(format!(
                "{bucket}/{}",
                "a".repeat(64)
            ))
        };
        let metadata = routed("bucket=projects%2F_%2Fbuckets%2Ftest-bucket&appendable=false");
        write(&metadata, Some(&resumed("test-bucket"))).unwrap();
        write(&metadata, Some(&resumed("other-bucket"))).unwrap();
        write(&routed("appendable=false"), Some(&resumed("test-bucket"))).unwrap();
        // A resumable session is never appendable.
        assert_eq!(
            write(&routed("appendable=true"), Some(&resumed("test-bucket")))
                .unwrap_err()
                .message(),
            CONFLICT
        );
        let error = check(&routed("bucket=bad"), Routes::Deferred).unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(
            reason(&error),
            "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT"
        );
    }

    /// Recorded 2026-11-18 (case grpc-probe-routing): GetObject, GetBucket, ListBuckets and
    /// WriteObject sent without the metadata answer INVALID_ARGUMENT.
    #[test]
    fn routing_requires_the_metadata() {
        let none = MetadataMap::new();
        for routes in [
            Routes::Request(&[Route::Bucket(Some("projects/_/buckets/b"))]),
            Routes::Request(&[Route::Project("projects/p")]),
            Routes::Deferred,
        ] {
            let error = admit(&none, routes).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument);
            assert_eq!(
                error.message(),
                "An x-goog-request-params request metadata property must be provided for this request."
            );
            assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS");
            // A handler's second look has nothing to compare.
            check(&none, routes).unwrap();
        }
        write(&none, None).unwrap();
        // Recorded 2026-11-18 (case grpc-probe-resumable-sessions): an upload ID RPC reaches its
        // handler, which answers the upload ID, without the metadata and with a routed bucket.
        admit(&none, Routes::Optional).unwrap();
        admit(
            &routed("bucket=projects%2F_%2Fbuckets%2Fa"),
            Routes::Optional,
        )
        .unwrap();
    }

    /// Recorded 2026-11-18 (case grpc-probe-routing): a routed bucket other than the request's,
    /// and metadata that routes no bucket at all, answer INVALID_ARGUMENT naming both bare IDs.
    #[test]
    fn routing_requires_the_request_bucket() {
        let routes = Routes::Request(&[Route::Bucket(Some("projects/_/buckets/b"))]);
        check(&routed("bucket=projects%2F_%2Fbuckets%2Fb"), routes).unwrap();
        for (params, specified) in [
            ("bucket=projects%2F_%2Fbuckets%2Fother", "other"),
            ("project=projects%2Fp", ""),
            ("unknown=1", ""),
        ] {
            let error = check(&routed(params), routes).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument, "{params}");
            assert_eq!(error.message(), other_bucket_message("b", specified));
            assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET");
        }
        // A bucket the request does not name matches only metadata that routes none; the
        // handler then answers the missing field.
        let unnamed = Routes::Request(&[Route::Bucket(None)]);
        check(&routed("appendable=false"), unnamed).unwrap();
        assert_eq!(
            check(&routed("bucket=projects%2F_%2Fbuckets%2Fb"), unnamed)
                .unwrap_err()
                .message(),
            other_bucket_message("", "b")
        );
    }

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
        assert_eq!(
            write(&bucket, None).unwrap_err().message(),
            other_bucket_message("", "b")
        );
        // A resumed write compares no bucket; the handler answers an upload ID it cannot read.
        let unreadable =
            pb::bidi_write_object_request::FirstMessage::UploadId("other/not-hex".into());
        write(&bucket, Some(&unreadable)).unwrap();

        assert_eq!(
            check(
                &routed("project=projects%2Fother"),
                Routes::Request(&[Route::Project("projects/p")])
            )
            .unwrap_err()
            .message(),
            CONFLICT
        );
    }

    /// Recorded 2026-11-18 (case grpc-probe-routing): an unknown key and a `routing_token`
    /// beside the request's bucket are served, and a field the RPC does not route is not
    /// compared.
    #[test]
    fn routing_ignores_keys_the_rpc_does_not_route() {
        let routes = [Route::Bucket(Some("b"))];
        for params in [
            "bucket=projects%2F_%2Fbuckets%2Fb&routing_token=token",
            "bucket=projects%2F_%2Fbuckets%2Fb&gcsmimic_unknown=1",
            "direct_connectivity_diagnostic=1&bucket=projects%2F_%2Fbuckets%2Fb",
            "bucket=projects%2F_%2Fbuckets%2Fb&project=projects%2Fp&appendable=maybe",
        ] {
            for checked in [Routes::Request(&routes), Routes::Deferred] {
                check(&routed(params), checked).unwrap();
            }
        }
        check(
            &routed("bucket=projects%2F_%2Fbuckets%2Fb&unknown=1"),
            Routes::Request(&[Route::Project("projects/p")]),
        )
        .unwrap();
    }

    #[test]
    fn deferred_routing_checks_only_presence_and_form() {
        // Without the request's resources, no field but a routed bucket's form is checked.
        check(
            &routed("project=projects%2Fp&source_bucket=bad&appendable=maybe"),
            Routes::Deferred,
        )
        .unwrap();
        let error = check(
            &routed("source_bucket=bad&bucket=projects%2F_%2Fbuckets%2Fb"),
            Routes::Request(&[Route::SourceBucket(Some("b")), Route::Bucket(Some("b"))]),
        )
        .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert!(
            error.message().starts_with("Invalid bucket name 'bad'."),
            "{error}"
        );
        assert!(!error.details().is_empty());
        assert_eq!(
            check(
                &routed(
                    "source_bucket=projects%2F_%2Fbuckets%2Fa&bucket=projects%2F_%2Fbuckets%2Fb"
                ),
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
