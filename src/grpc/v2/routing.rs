use super::{names, pb::bidi_write_object_request::FirstMessage, status};
use crate::json;
use tonic::{Status, metadata::MetadataMap};

/// The metadata that routes a Storage v2 call: `key=value` pairs joined by `&`, each key and
/// value percent-encoded like a form field.
const ROUTING_METADATA: &str = "x-goog-request-params";

/// A routing key some Storage v2 RPC compares with its request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RoutingKey {
    Bucket,
    SourceBucket,
    Project,
    Appendable,
}

/// A routing key no Storage v2 RPC compares with its request.
#[derive(Debug)]
pub(super) struct UnknownRoutingKey;

impl std::fmt::Display for UnknownRoutingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unknown routing metadata key")
    }
}

impl std::error::Error for UnknownRoutingKey {}

impl std::str::FromStr for RoutingKey {
    type Err = UnknownRoutingKey;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        match key {
            "bucket" => Ok(Self::Bucket),
            "source_bucket" => Ok(Self::SourceBucket),
            "project" => Ok(Self::Project),
            "appendable" => Ok(Self::Appendable),
            _ => Err(UnknownRoutingKey),
        }
    }
}

/// The bucket ID of a routed bucket resource name `projects/_/buckets/<id>`. Routing checks
/// only the resource name's form; the handler validates the request's own bucket name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RoutedBucket(String);

impl RoutedBucket {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for RoutedBucket {
    type Err = Status;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        names::bucket_name(name).map(|id| Self(id.to_owned()))
    }
}

/// A resource the request names, compared with the values routed under the same key.
#[derive(Clone, Copy, Debug)]
pub(super) enum Route<'a> {
    /// A resource name `projects/_/buckets/<id>` or a bare ID resolved from a token; `None`
    /// when the request names no bucket. The metadata must route the same bucket: metadata
    /// that routes none is compared as routing the empty name (recorded 2026-11-18 on GetObject
    /// routed by a project alone, case grpc-probe-routing).
    Bucket(Option<&'a str>),
    /// Compared with each routed source bucket; the metadata need not route one.
    SourceBucket(Option<&'a str>),
    /// Compared as text with each routed project; proto3's empty string is a value here.
    Project(&'a str),
    /// Compared with each routed flag.
    Appendable(bool),
}

impl Route<'_> {
    fn key(self) -> RoutingKey {
        match self {
            Self::Bucket(_) => RoutingKey::Bucket,
            Self::SourceBucket(_) => RoutingKey::SourceBucket,
            Self::Project(_) => RoutingKey::Project,
            Self::Appendable(_) => RoutingKey::Appendable,
        }
    }
}

/// The routing keys a call routes and when [`admit`] compares them.
///
/// A call reads and validates only the values of the keys it routes: a pair under any other
/// key is skipped unread, whatever it holds. Cloud Storage served a key no RPC routes, a
/// `routing_token` no redirect issued, and a project on GetObject as if they were absent
/// (recorded 2026-11-18, case grpc-probe-routing); no recording sent a malformed value under a
/// key the RPC does not route, so skipping it unread is a local choice.
#[derive(Clone, Copy, Debug)]
pub(super) enum Routes<'a> {
    /// The metadata is required, and each listed field must name the same resource. Of the
    /// RPCs routed this way, Cloud Storage was recorded refusing GetObject, GetBucket and
    /// ListBuckets sent without it (2026-11-18, case grpc-probe-routing, which also recorded
    /// WriteObject); requiring it on the others is a generalization from those four.
    Request(&'a [Route<'a>]),
    /// The call is admitted with or without the metadata, and nothing is read or compared.
    /// Cloud Storage answered QueryWriteStatus and CancelResumableWrite sent without it for
    /// their malformed upload ID (recorded 2026-11-18, case grpc-probe-resumable-sessions, steps
    /// cancel-malformed, status-malformed and cancel-empty), and sent with a routed bucket equal
    /// to the one an upload ID starts with for that ID too (steps cancel-unknown-under-bucket and
    /// status-unknown-under-bucket): the answer came from the ID, not from a comparison. A local
    /// upload ID `<bucket>/<session>` does not match the proto's routing template
    /// `{bucket=projects/*/buckets/*}/**`, so a client routes no bucket from it, and the
    /// handlers authorize with the bucket the parsed ID names.
    Optional,
    /// The metadata is required, but the handler learns its resources later (a stream's first
    /// message, a rewrite token): the values of these keys are read and validated now, and the
    /// handler compares them through [`check`]. Of these RPCs only WriteObject was recorded
    /// without the metadata (case grpc-probe-routing); BidiWriteObject and RewriteObject follow
    /// it by inference, and BidiReadObject, which Cloud Storage refused for the test project,
    /// could not be recorded at all.
    Deferred(&'static [RoutingKey]),
}

impl Routes<'static> {
    /// BidiReadObject: the bucket of the stream's first read spec.
    pub(super) const BIDI_READ_OBJECT: Self = Self::Deferred(&[RoutingKey::Bucket]);
    /// WriteObject and BidiWriteObject: the bucket and appendable flag of the first message,
    /// compared by [`write`].
    pub(super) const WRITE_OBJECT: Self =
        Self::Deferred(&[RoutingKey::Bucket, RoutingKey::Appendable]);
    /// RewriteObject: the source and destination buckets, which a rewrite token may resolve.
    pub(super) const REWRITE_OBJECT: Self =
        Self::Deferred(&[RoutingKey::SourceBucket, RoutingKey::Bucket]);
}

impl Routes<'_> {
    /// Whether the call routes `key`, and so reads and validates its values.
    fn routes(self, key: RoutingKey) -> bool {
        match self {
            Self::Request(routes) => routes.iter().any(|route| route.key() == key),
            Self::Optional => false,
            Self::Deferred(keys) => keys.contains(&key),
        }
    }
}

const CONFLICT: &str = "Routing metadata conflicts with the request; use the same resource in x-goog-request-params and the request";

/// Cloud Storage's answer to a call without the metadata (recorded 2026-11-18 on GetObject,
/// GetBucket, ListBuckets and WriteObject, case grpc-probe-routing).
fn missing_metadata() -> Status {
    status::invalid_with_reason(
        "An x-goog-request-params request metadata property must be provided for this request.",
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS",
    )
}

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

/// Cloud Storage's answer to a routed bucket that is no resource name (recorded 2026-11-18 on
/// GetObject and GetBucket routed by a bare bucket ID, case grpc-probe-routing).
fn invalid_bucket_format(value: &str) -> Status {
    status::invalid_with_reason(
        format!(
            "Invalid bucket name found in x-goog-request-params metadata (bucket={value}). Must be of the form `projects/_/buckets/{{bucket_id}}`"
        ),
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT",
    )
}

/// The local answer to a routed flag other than `true` and `false`, worded like the recorded
/// bucket form refusal; no recording routed one.
fn invalid_appendable(value: &str) -> Status {
    Status::invalid_argument(format!(
        "Invalid appendable value found in x-goog-request-params metadata (appendable={value}). Must be true or false"
    ))
}

/// The local answer to a routed value other than the request's, for the keys whose Cloud
/// Storage wording is not recorded.
fn conflict() -> Status {
    Status::invalid_argument(CONFLICT)
}

/// `Ok` when a routed value equals the request's, otherwise the answer `differs` words.
fn agree<T: PartialEq + ?Sized>(
    routed: &T,
    requested: &T,
    differs: impl FnOnce() -> Status,
) -> Result<(), Status> {
    if routed == requested {
        Ok(())
    } else {
        Err(differs())
    }
}

/// The bucket ID a request names, compared with routed bucket IDs: a resource name's ID, a bare
/// ID as a rewrite token resolves it, or the empty name when the request names none. A
/// malformed resource name is compared as written, and its handler refuses it.
fn request_bucket(name: Option<&str>) -> &str {
    name.map_or("", |name| names::bucket_name(name).unwrap_or(name))
}

/// The key of a routing pair, when it decodes to one some RPC routes. A key is percent-decoded
/// before it is recognized, so `%62ucket` is `bucket`.
fn routing_key(key: &[u8]) -> Option<RoutingKey> {
    let key = std::str::from_utf8(key).ok()?;
    json::decode(key, json::Decoding::Form).ok()?.parse().ok()
}

/// The percent-decoded value of a routing pair whose key the call routes.
fn routed_value(value: &[u8]) -> Result<String, Status> {
    let value = std::str::from_utf8(value).map_err(|_| {
        Status::invalid_argument(
            "Invalid routing metadata; use bucket=projects%2F_%2Fbuckets%2Fexample-bucket",
        )
    })?;
    Ok(json::decode(value, json::Decoding::Form)?)
}

/// The values of the keys a call routes, each parsed into its key's type, in the order sent.
///
/// A key may be routed more than once, in one `x-goog-request-params` value or across several;
/// every value is kept and compared, and the first that disagrees with the request is
/// answered. That is a local choice: no recording routed a key twice.
#[derive(Clone, Debug, Default)]
pub(super) struct Routing {
    buckets: Vec<RoutedBucket>,
    source_buckets: Vec<RoutedBucket>,
    projects: Vec<String>,
    appendable: Vec<bool>,
}

impl Routing {
    /// Reads every `x-goog-request-params` value for the keys `routes` routes; `None` when the
    /// request carries no such metadata.
    ///
    /// A pair under a routed key without `=`, or whose value is not percent-encoded UTF-8 or
    /// not of its key's form, is refused. No recording sent such a pair but a bucket that is no
    /// resource name, so the other refusals are local choices.
    fn parse(metadata: &MetadataMap, routes: Routes<'_>) -> Result<Option<Self>, Status> {
        // `x-goog-user-project` names the Requester Pays billing project. A gcs-mimic bucket
        // cannot enable Requester Pays, and Cloud Storage served the JSON parameter on a bucket
        // and a project without it as if it were absent (cases bkt-edges-hmac-keys and
        // bkt-edges-notification-ids), so that metadata is not read either.
        let mut values = metadata.get_all(ROUTING_METADATA).iter().peekable();
        if values.peek().is_none() {
            return Ok(None);
        }
        let mut routing = Self::default();
        for value in values {
            for pair in value.as_bytes().split(|&byte| byte == b'&') {
                let mut parts = pair.splitn(2, |&byte| byte == b'=');
                let key = parts.next().unwrap_or_default();
                let Some(key) = routing_key(key).filter(|&key| routes.routes(key)) else {
                    continue;
                };
                let value = parts.next().ok_or_else(|| {
                    Status::invalid_argument("Routing metadata must contain key=value pairs")
                })?;
                routing.push(key, routed_value(value)?)?;
            }
        }
        Ok(Some(routing))
    }

    /// Adds `value` under `key`, parsed into the key's type.
    fn push(&mut self, key: RoutingKey, value: String) -> Result<(), Status> {
        match key {
            RoutingKey::Bucket => {
                let bucket = value.parse().map_err(|_| invalid_bucket_format(&value))?;
                self.buckets.push(bucket);
            }
            // Refused with the wording of a request's malformed bucket name; a malformed routed
            // source bucket was not recorded.
            RoutingKey::SourceBucket => self.source_buckets.push(value.parse()?),
            RoutingKey::Project => self.projects.push(value),
            RoutingKey::Appendable => {
                let flag = value.parse().map_err(|_| invalid_appendable(&value))?;
                self.appendable.push(flag);
            }
        }
        Ok(())
    }

    /// Compares each route with every value routed under its key, in the order of `routes`
    /// and then in the order sent, and answers the first that disagrees with the request. A
    /// key the call does not list is not compared: Cloud Storage answered a routed project on
    /// GetObject for the bucket the metadata left out (recorded 2026-11-18, case
    /// grpc-probe-routing).
    fn compare(&self, routes: &[Route<'_>]) -> Result<(), Status> {
        routes
            .iter()
            .try_for_each(|&route| self.compare_route(route))
    }

    fn compare_route(&self, route: Route<'_>) -> Result<(), Status> {
        match route {
            Route::Bucket(requested) => {
                let requested = request_bucket(requested);
                if self.buckets.is_empty() {
                    return agree("", requested, || other_bucket(requested, ""));
                }
                self.buckets.iter().try_for_each(|routed| {
                    agree(routed.as_str(), requested, || {
                        other_bucket(requested, routed.as_str())
                    })
                })
            }
            Route::SourceBucket(requested) => {
                let requested = request_bucket(requested);
                self.source_buckets
                    .iter()
                    .try_for_each(|routed| agree(routed.as_str(), requested, conflict))
            }
            Route::Project(requested) => self
                .projects
                .iter()
                .try_for_each(|routed| agree(routed.as_str(), requested, conflict)),
            Route::Appendable(requested) => self
                .appendable
                .iter()
                .try_for_each(|routed| agree(routed, &requested, conflict)),
        }
    }
}

/// The check every RPC passes before its handler runs: the metadata must be sent, unless the
/// call's routes are `Optional`, and must agree with the request. The refusal of a call
/// without the metadata was recorded only on GetObject, GetBucket, ListBuckets and WriteObject;
/// refusing it on every other RPC but the `Optional` ones is a generalization from those four.
pub(super) fn admit(metadata: &MetadataMap, routes: Routes<'_>) -> Result<(), Status> {
    match (routes, Routing::parse(metadata, routes)?) {
        (Routes::Optional, _) | (Routes::Deferred(_), Some(_)) => Ok(()),
        (Routes::Request(_) | Routes::Deferred(_), None) => Err(missing_metadata()),
        (Routes::Request(compared), Some(routing)) => routing.compare(compared),
    }
}

/// Compares the metadata a request carries with its resources. A handler that learns its
/// resources after `admit` calls this again; a request without the metadata passes here.
pub(super) fn check(metadata: &MetadataMap, routes: Routes<'_>) -> Result<(), Status> {
    let Some(routing) = Routing::parse(metadata, routes)? else {
        return Ok(());
    };
    match routes {
        Routes::Request(compared) => routing.compare(compared),
        Routes::Optional | Routes::Deferred(_) => Ok(()),
    }
}

pub(super) fn iam_bucket(resource: &str) -> &str {
    resource
        .split_once("/managedFolders/")
        .or_else(|| resource.split_once("/objects/"))
        .map_or(resource, |(bucket, _)| bucket)
}

/// The bucket resource a write stream's first message routes: the bucket its spec names, or
/// `None` when it names none. A resumed write routes none: its upload ID names its bucket in a
/// form no client routes (see `Routes::Optional`).
pub(super) fn write_bucket(first: Option<&FirstMessage>) -> Option<&str> {
    match first? {
        FirstMessage::WriteObjectSpec(spec) => spec.resource.as_ref().map(|r| r.bucket.as_str()),
        FirstMessage::AppendObjectSpec(spec) => Some(spec.bucket.as_str()),
        FirstMessage::UploadId(_) => None,
    }
}

/// Compares the routing of a write stream with its first message.
pub(super) fn write(metadata: &MetadataMap, first: Option<&FirstMessage>) -> Result<(), Status> {
    let appendable = match first {
        Some(FirstMessage::WriteObjectSpec(spec)) => spec.appendable == Some(true),
        Some(FirstMessage::AppendObjectSpec(_)) => true,
        // The handler authorizes a resumed write with the bucket its upload ID names, so no
        // bucket is compared.
        Some(FirstMessage::UploadId(_)) => {
            return check(metadata, Routes::Request(&[Route::Appendable(false)]));
        }
        None => false,
    };
    check(
        metadata,
        Routes::Request(&[
            Route::Bucket(write_bucket(first)),
            Route::Appendable(appendable),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::super::pb;
    use super::*;
    use tonic::{Code, metadata::AsciiMetadataValue};

    /// GetObject on the bucket `b`.
    const GET_OBJECT: Routes<'static> =
        Routes::Request(&[Route::Bucket(Some("projects/_/buckets/b"))]);

    /// Metadata that carries each of `values` as one `x-goog-request-params` value.
    fn routed_by(values: &[&[u8]]) -> MetadataMap {
        let mut metadata = MetadataMap::new();
        for value in values {
            metadata.append(
                ROUTING_METADATA,
                AsciiMetadataValue::try_from(*value).unwrap(),
            );
        }
        metadata
    }

    fn routed(value: &str) -> MetadataMap {
        routed_by(&[value.as_bytes()])
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
        let resumed = |bucket: &str| FirstMessage::UploadId(format!("{bucket}/{}", "a".repeat(64)));
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
    }

    /// Recorded 2026-11-18 (case grpc-probe-routing): GetObject, GetBucket, ListBuckets and
    /// WriteObject sent without the metadata answer INVALID_ARGUMENT.
    #[test]
    fn routing_requires_the_metadata() {
        let none = MetadataMap::new();
        for routes in [
            GET_OBJECT,
            Routes::Request(&[Route::Project("projects/p")]),
            Routes::BIDI_READ_OBJECT,
            Routes::WRITE_OBJECT,
            Routes::REWRITE_OBJECT,
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
        admit(&routed("bucket=projects%2F_%2Fbuckets%2Fb"), GET_OBJECT).unwrap();
        for (params, specified) in [
            ("bucket=projects%2F_%2Fbuckets%2Fother", "other"),
            ("project=projects%2Fp", ""),
            ("unknown=1", ""),
        ] {
            let error = admit(&routed(params), GET_OBJECT).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument, "{params}");
            assert_eq!(error.message(), other_bucket_message("b", specified));
            assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET");
        }
        // A bucket the request does not name matches only metadata that routes none; the
        // handler then answers the missing field.
        let unnamed = Routes::Request(&[Route::Bucket(None)]);
        admit(&routed("appendable=false"), unnamed).unwrap();
        assert_eq!(
            admit(&routed("bucket=projects%2F_%2Fbuckets%2Fb"), unnamed)
                .unwrap_err()
                .message(),
            other_bucket_message("", "b")
        );
    }

    #[test]
    fn routing_compares_each_field_with_its_own_rule() {
        let spec = |appendable| {
            FirstMessage::WriteObjectSpec(pb::WriteObjectSpec {
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

        // A bucket the request does not name matches no routed bucket.
        let bucket = routed("bucket=projects%2F_%2Fbuckets%2Fb");
        assert_eq!(
            write(&bucket, None).unwrap_err().message(),
            other_bucket_message("", "b")
        );
        // A resumed write compares no bucket; the handler answers an upload ID it cannot read.
        let unreadable = FirstMessage::UploadId("other/not-hex".into());
        write(&bucket, Some(&unreadable)).unwrap();

        assert_eq!(
            admit(
                &routed("project=projects%2Fother"),
                Routes::Request(&[Route::Project("projects/p")])
            )
            .unwrap_err()
            .message(),
            CONFLICT
        );
        // A rewrite token may resolve a bare source bucket ID.
        let rewrite =
            routed("source_bucket=projects%2F_%2Fbuckets%2Fa&bucket=projects%2F_%2Fbuckets%2Fb");
        admit(
            &rewrite,
            Routes::Request(&[
                Route::SourceBucket(Some("a")),
                Route::Bucket(Some("projects/_/buckets/b")),
            ]),
        )
        .unwrap();
        assert_eq!(
            admit(
                &rewrite,
                Routes::Request(&[Route::SourceBucket(Some("b")), Route::Bucket(Some("b"))]),
            )
            .unwrap_err()
            .message(),
            CONFLICT
        );
    }

    /// Recorded 2026-11-18 (case grpc-probe-routing): an unknown key and a `routing_token`
    /// beside the request's bucket are served, and a key the RPC does not route is not
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
            for admitted in [Routes::Request(&routes), Routes::BIDI_READ_OBJECT] {
                admit(&routed(params), admitted).unwrap();
            }
        }
        admit(
            &routed("bucket=projects%2F_%2Fbuckets%2Fb&unknown=1"),
            Routes::Request(&[Route::Project("projects/p")]),
        )
        .unwrap();
    }

    /// A routed value is validated only when the RPC routes its key. Cloud Storage's refusal of
    /// a bucket that is no resource name was recorded on GetObject and GetBucket (case
    /// grpc-probe-routing); no recording routed a malformed value under a key the RPC does not
    /// route, or a malformed source bucket or flag.
    #[test]
    fn routing_validates_only_the_keys_the_rpc_routes() {
        for routes in [
            GET_OBJECT,
            Routes::BIDI_READ_OBJECT,
            Routes::WRITE_OBJECT,
            Routes::REWRITE_OBJECT,
        ] {
            let error = admit(&routed("bucket=bad"), routes).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument);
            assert_eq!(
                error.message(),
                "Invalid bucket name found in x-goog-request-params metadata (bucket=bad). Must be of the form `projects/_/buckets/{bucket_id}`"
            );
            assert_eq!(
                reason(&error),
                "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT"
            );
        }
        // ListBuckets routes a project, BidiReadObject a bucket, and the upload ID RPCs nothing.
        admit(
            &routed("project=projects%2Fp&bucket=bad&source_bucket=bad&appendable=maybe"),
            Routes::Request(&[Route::Project("projects/p")]),
        )
        .unwrap();
        for routes in [GET_OBJECT, Routes::BIDI_READ_OBJECT] {
            admit(
                &routed("bucket=projects%2F_%2Fbuckets%2Fb&source_bucket=bad&appendable=maybe"),
                routes,
            )
            .unwrap();
        }
        admit(&routed("bucket=bad&appendable=maybe"), Routes::Optional).unwrap();

        // RewriteObject routes a source bucket, worded like a request's bucket name.
        let error = admit(
            &routed("source_bucket=bad&bucket=projects%2F_%2Fbuckets%2Fb"),
            Routes::REWRITE_OBJECT,
        )
        .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert!(
            error.message().starts_with("Invalid bucket name 'bad'."),
            "{error}"
        );
        assert_eq!(reason(&error), "GRPC_INVALID_BUCKET");
        // WriteObject routes a flag, which only `true` and `false` spell.
        for flag in ["maybe", "TRUE"] {
            let error = admit(
                &routed(&format!(
                    "bucket=projects%2F_%2Fbuckets%2Fb&appendable={flag}"
                )),
                Routes::WRITE_OBJECT,
            )
            .unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument);
            assert_eq!(
                error.message(),
                format!(
                    "Invalid appendable value found in x-goog-request-params metadata (appendable={flag}). Must be true or false"
                )
            );
        }
    }

    /// Every value of a key routed more than once is compared, in the order sent, and the first
    /// that names another bucket is answered. A local choice: no recording routed a key twice.
    #[test]
    fn routing_compares_every_value_of_a_duplicated_key() {
        for metadata in [
            routed("bucket=projects%2F_%2Fbuckets%2Fb&bucket=projects%2F_%2Fbuckets%2Fother"),
            routed("bucket=projects%2F_%2Fbuckets%2Fother&bucket=projects%2F_%2Fbuckets%2Fb"),
            routed_by(&[
                b"bucket=projects%2F_%2Fbuckets%2Fb",
                b"bucket=projects%2F_%2Fbuckets%2Fother",
            ]),
        ] {
            let error = admit(&metadata, GET_OBJECT).unwrap_err();
            assert_eq!(error.message(), other_bucket_message("b", "other"));
            assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET");
        }
        assert_eq!(
            admit(
                &routed(
                    "bucket=projects%2F_%2Fbuckets%2Ffirst&bucket=projects%2F_%2Fbuckets%2Fsecond"
                ),
                GET_OBJECT,
            )
            .unwrap_err()
            .message(),
            other_bucket_message("b", "first")
        );
        admit(
            &routed("bucket=projects%2F_%2Fbuckets%2Fb&bucket=projects%2F_%2Fbuckets%2Fb"),
            GET_OBJECT,
        )
        .unwrap();
    }

    /// A pair under a key the RPC does not route is never interpreted, so however malformed it
    /// is it cannot fail the call. A local choice: the recorded unknown keys were well formed
    /// (case grpc-probe-routing).
    #[test]
    fn routing_skips_malformed_pairs_under_keys_the_rpc_does_not_route() {
        for params in [
            b"bucket=projects%2F_%2Fbuckets%2Fb&unknown=%GG".as_slice(),
            b"bucket=projects%2F_%2Fbuckets%2Fb&unknown",
            b"bucket=projects%2F_%2Fbuckets%2Fb&",
            b"&bucket=projects%2F_%2Fbuckets%2Fb",
            b"bucket=projects%2F_%2Fbuckets%2Fb&&=",
            b"%GG=1&bucket=projects%2F_%2Fbuckets%2Fb",
            b"bucket=projects%2F_%2Fbuckets%2Fb&project",
            b"bucket=projects%2F_%2Fbuckets%2Fb&source_bucket=%GG",
            b"bucket=projects%2F_%2Fbuckets%2Fb&unknown=\xe9",
        ] {
            admit(&routed_by(&[params]), GET_OBJECT)
                .unwrap_or_else(|error| panic!("{}: {error}", params.escape_ascii()));
        }
        // An empty value routes nothing.
        admit(&routed(""), Routes::Request(&[Route::Bucket(None)])).unwrap();
    }

    /// A key is percent-decoded before it is recognized, and a malformed pair under a key the
    /// RPC routes is refused. A local choice: no recording sent a malformed pair.
    #[test]
    fn routing_refuses_malformed_pairs_under_keys_the_rpc_routes() {
        admit(&routed("%62ucket=projects%2F_%2Fbuckets%2Fb"), GET_OBJECT).unwrap();
        assert_eq!(
            admit(
                &routed("%62ucket=projects%2F_%2Fbuckets%2Fother"),
                GET_OBJECT
            )
            .unwrap_err()
            .message(),
            other_bucket_message("b", "other")
        );
        for (params, message) in [
            (
                b"bucket".as_slice(),
                "Routing metadata must contain key=value pairs",
            ),
            (b"%62ucket", "Routing metadata must contain key=value pairs"),
            (b"bucket=%GG", "Malformed URI encoding"),
            (
                b"bucket=\xe9",
                "Invalid routing metadata; use bucket=projects%2F_%2Fbuckets%2Fexample-bucket",
            ),
        ] {
            let error = admit(&routed_by(&[params]), GET_OBJECT).unwrap_err();
            assert_eq!(
                (error.code(), error.message()),
                (Code::InvalidArgument, message),
                "{}",
                params.escape_ascii()
            );
        }
    }
}
