use super::{names, pb::bidi_write_object_request::FirstMessage, status};
use crate::json;
use tonic::{Extensions, Status, metadata::MetadataMap};

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

impl RoutingKey {
    /// The key as clients write it.
    fn as_str(self) -> &'static str {
        match self {
            Self::Bucket => "bucket",
            Self::SourceBucket => "source_bucket",
            Self::Project => "project",
            Self::Appendable => "appendable",
        }
    }
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

/// The project ID of a routed project resource name `projects/<id>`, the form Cloud Storage
/// served on ListBuckets, by ID and by number (recorded, cases grpc-bucket-list and
/// prj-number-inputs). Routing checks only the resource name's form; the handler validates the
/// request's own project.
///
/// A value of any other form is refused in the words of a request's malformed project name:
/// Cloud Storage answered so to `project=buckets%2Fx` beside the parent `buckets/x` (case
/// grpc-bucket-list, step list-malformed-parent). That step cannot tell a refusal of the routed
/// value from one of the parent, so refusing the routed value alone is inferred.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RoutedProject(String);

impl RoutedProject {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for RoutedProject {
    type Err = Status;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        names::project_id(name).map(|id| Self(id.to_owned()))
    }
}

/// A resource the request names, compared with the value routed under the same key.
#[derive(Clone, Copy, Debug)]
pub(super) enum Route<'a> {
    /// A resource name `projects/_/buckets/<id>` or a bare ID resolved from a token; `None`
    /// when the request names no bucket. The metadata must route the same bucket: metadata
    /// that routes none is compared as routing the empty name (recorded 2026-11-18 on GetObject
    /// routed by a project alone, case grpc-probe-routing).
    Bucket(Option<&'a str>),
    /// The destination bucket of a rewrite, compared like [`Route::Bucket`] but answered in
    /// the rewrite's own words (recorded 2026-11-19, case grpc-probe-inferred-routing).
    DestinationBucket(Option<&'a str>),
    /// Compared with the routed source bucket; metadata that routes none is compared as
    /// routing the empty name (recorded 2026-11-19, case grpc-probe-inferred-routing, step
    /// rewrite-routing-bucket-only).
    SourceBucket(Option<&'a str>),
    /// A project resource name `projects/<id>`, whose ID is compared with the routed project's
    /// (recorded 2026-11-19 on ListBuckets, case grpc-probe-inferred-routing). A malformed name,
    /// proto3's empty string included, is compared as written, and its handler refuses it.
    ///
    /// Unlike a bucket, a project is compared only when the metadata routes one. Metadata that
    /// routes no bucket compares as routing the empty bucket name (recorded on GetObject routed
    /// by a project alone, case grpc-probe-routing), but metadata that routes no project was not
    /// recorded on ListBuckets or CreateBucket; such a call keeps being served, which is a local
    /// choice.
    Project(&'a str),
    /// Compared with the routed flag.
    Appendable(bool),
}

/// When [`admit`] compares the routing metadata with the request.
///
/// Every call reads every pair under a key some RPC routes (`bucket`, `source_bucket`,
/// `project`, `appendable`): its value must be of its key's form and the key may be sent only
/// once, whichever RPC the call is; the call then compares only the keys it routes. A pair
/// under any other key is skipped unread. Cloud Storage refused a duplicated `bucket` on
/// GetObject, a malformed `bucket` beside the right `project` on ListBuckets (which routes no
/// bucket) and a `bucket` pair without a value, and served an unknown key, a `routing_token`
/// no redirect issued, a malformed unknown pair, a trailing `&` and a percent-encoded key
/// (recorded 2026-11-18 and 2026-11-19, cases grpc-probe-routing and
/// grpc-probe-inferred-routing). A malformed `source_bucket` or `appendable`, and a duplicated
/// key other than `bucket`, were not recorded; they follow `bucket` by inference. A malformed
/// `project` is refused as [`RoutedProject`] states.
#[derive(Clone, Copy, Debug)]
pub(super) enum Routes<'a> {
    /// The metadata is required, and each listed field must name the same resource. Cloud
    /// Storage was recorded refusing every RPC asked sent without it (2026-11-18 and
    /// 2026-11-19, cases grpc-probe-routing and grpc-probe-inferred-routing). Metadata with no
    /// non-empty value counts as none (step get-routing-empty-value).
    Request(&'a [Route<'a>]),
    /// The metadata is required, but the handler learns its resources later (a stream's first
    /// message, a rewrite token): the handler compares them with the [`Admitted`] it reads
    /// through [`Admitted::of`].
    Deferred,
    /// QueryWriteStatus and CancelResumableWrite: the call is admitted with or without the
    /// metadata ([`Admitted::Unrouted`]), and the handler compares it with the bucket its upload
    /// ID names ([`Admitted::compare_upload`]). Cloud Storage answered a malformed ID without
    /// the metadata for the ID (case grpc-probe-resumable-sessions, steps cancel-malformed,
    /// status-malformed and cancel-empty), and a readable ID without the metadata, and under
    /// another routed bucket, for the metadata (case grpc-probe-inferred-routing, steps
    /// `*-without-routing` and `*-routed-other-bucket`).
    Upload,
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

/// Cloud Storage's answer to a routed source bucket other than the rewrite's (recorded
/// 2026-11-19, case grpc-probe-inferred-routing).
fn other_source_bucket(requested: &str, routed: &str) -> Status {
    status::invalid_with_reason(
        format!(
            "The x-goog-request-params must specify the same source and destination buckets as in the request. Request source bucket was '{requested}' but x-goog-request-params specified '{routed}'"
        ),
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_SOURCE_BUCKET",
    )
}

/// Cloud Storage's answer to a routed bucket other than the rewrite's destination (recorded
/// 2026-11-19, case grpc-probe-inferred-routing).
fn other_destination_bucket(requested: &str, routed: &str) -> Status {
    status::invalid_with_reason(
        format!(
            "The x-goog-request-params must specify the same source and destination buckets as the resource in the request. Request destination bucket was '{requested}', but x-goog-request-params specified '{routed}'"
        ),
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_DEST_BUCKET",
    )
}

/// Cloud Storage's answer to a routed project other than the request's (recorded 2026-11-19 on
/// ListBuckets, case grpc-probe-inferred-routing); both are project IDs, without `projects/`.
fn other_project(requested: &str, routed: &str) -> Status {
    status::invalid_with_reason(
        format!(
            "The x-goog-request-params must specify the same project as the resource in the request. Request project was '{requested}', but x-goog-request-params specified '{routed}'"
        ),
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_PROJECT",
    )
}

/// Cloud Storage's answer to a routed bucket that is no resource name, or a `bucket` pair with
/// no value (recorded 2026-11-18 on GetObject and GetBucket routed by a bare bucket ID, and
/// 2026-11-19 on GetObject routed by `bucket` alone and on ListBuckets, cases grpc-probe-routing
/// and grpc-probe-inferred-routing). The message quotes the whole `x-goog-request-params` value.
fn invalid_bucket_format(header: &str) -> Status {
    status::invalid_with_reason(
        format!(
            "Invalid bucket name found in x-goog-request-params metadata ({header}). Must be of the form `projects/_/buckets/{{bucket_id}}`"
        ),
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT",
    )
}

/// Cloud Storage's answer to a key routed more than once, even with the same value (recorded
/// 2026-11-19 on GetObject's `bucket`, case grpc-probe-inferred-routing).
fn duplicate_entry(key: RoutingKey) -> Status {
    status::invalid_with_reason(
        format!(
            "The x-goog-request-params metadata has duplicate entries for the key '{}'.",
            key.as_str()
        ),
        "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_DUPLICATE_ENTRY",
    )
}

/// The local answer to a routed flag other than `true` and `false`, worded like the recorded
/// bucket form refusal; no recording routed one.
fn invalid_appendable(value: &str) -> Status {
    Status::invalid_argument(format!(
        "Invalid appendable value found in x-goog-request-params metadata (appendable={value}). Must be true or false"
    ))
}

/// The local answer to a routed flag other than the request's; no recording routed one.
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

/// The bucket ID a request names, compared with the routed bucket ID: a resource name's ID, a
/// bare ID as a rewrite token resolves it, or the empty name when the request names none. A
/// malformed resource name is compared as written, and its handler refuses it.
fn request_bucket(name: Option<&str>) -> &str {
    name.map_or("", |name| names::bucket_name(name).unwrap_or(name))
}

/// The routed bucket ID, or the empty name when the metadata routes none.
fn routed_bucket(routed: Option<&RoutedBucket>) -> &str {
    routed.map_or("", RoutedBucket::as_str)
}

/// The project ID a request's project resource name names, compared with the routed project
/// ID. A malformed name is compared as written, and its handler refuses it.
fn request_project(name: &str) -> &str {
    names::project_id(name).unwrap_or(name)
}

/// The key of a routing pair, when it decodes to one some RPC routes. A key is percent-decoded
/// before it is recognized, so `%62ucket` is `bucket`.
fn routing_key(key: &[u8]) -> Option<RoutingKey> {
    let key = std::str::from_utf8(key).ok()?;
    json::decode(key, json::Decoding::Form).ok()?.parse().ok()
}

/// The percent-decoded value of a routing pair under a routed key.
fn routed_value(value: &[u8]) -> Result<String, Status> {
    let value = std::str::from_utf8(value).map_err(|_| {
        Status::invalid_argument(
            "Invalid routing metadata; use bucket=projects%2F_%2Fbuckets%2Fexample-bucket",
        )
    })?;
    Ok(json::decode(value, json::Decoding::Form)?)
}

/// Stores `value` under `key` in `slot`, refusing a key that is already set.
fn fill<T>(slot: &mut Option<T>, key: RoutingKey, value: T) -> Result<(), Status> {
    if slot.replace(value).is_some() {
        return Err(duplicate_entry(key));
    }
    Ok(())
}

/// The values of the routed keys of a call that carried the metadata, each parsed into its
/// key's type; a key the metadata leaves out is `None`. A key is routed at most once, in one
/// `x-goog-request-params` value or across several.
#[derive(Clone, Debug, Default)]
pub(super) struct Routing {
    bucket: Option<RoutedBucket>,
    source_bucket: Option<RoutedBucket>,
    project: Option<RoutedProject>,
    appendable: Option<bool>,
}

impl Routing {
    /// Reads every `x-goog-request-params` value; `None` when the request carries no non-empty
    /// one. An empty value routes nothing, so it is the same as no metadata (recorded
    /// 2026-11-19, case grpc-probe-inferred-routing, step get-routing-empty-value).
    ///
    /// A pair under a routed key is parsed whether or not the call routes that key (see
    /// [`Routes`]); a pair without `=` routes an empty value.
    fn parse(metadata: &MetadataMap) -> Result<Option<Self>, Status> {
        // `x-goog-user-project` names the Requester Pays billing project. A gcs-mimic bucket
        // cannot enable Requester Pays, and Cloud Storage served the JSON parameter on a bucket
        // and a project without it as if it were absent (cases bkt-edges-hmac-keys and
        // bkt-edges-notification-ids), so that metadata is not read either.
        let mut values = metadata
            .get_all(ROUTING_METADATA)
            .iter()
            .filter(|value| !value.is_empty())
            .peekable();
        if values.peek().is_none() {
            return Ok(None);
        }
        let mut routing = Self::default();
        for value in values {
            let header = String::from_utf8_lossy(value.as_bytes());
            for pair in value.as_bytes().split(|&byte| byte == b'&') {
                let mut parts = pair.splitn(2, |&byte| byte == b'=');
                let key = parts.next().unwrap_or_default();
                let Some(key) = routing_key(key) else {
                    continue;
                };
                let value = routed_value(parts.next().unwrap_or_default())?;
                routing.push(key, &value, &header)?;
            }
        }
        Ok(Some(routing))
    }

    /// Adds `value`, read from the `x-goog-request-params` value `header`, under `key`,
    /// parsed into the key's type.
    fn push(&mut self, key: RoutingKey, value: &str, header: &str) -> Result<(), Status> {
        match key {
            RoutingKey::Bucket => {
                let bucket = value.parse().map_err(|_| invalid_bucket_format(header))?;
                fill(&mut self.bucket, key, bucket)
            }
            // Refused with the wording of a request's malformed bucket name; a malformed routed
            // source bucket was not recorded.
            RoutingKey::SourceBucket => fill(&mut self.source_bucket, key, value.parse()?),
            RoutingKey::Project => fill(&mut self.project, key, value.parse()?),
            RoutingKey::Appendable => {
                let flag = value.parse().map_err(|_| invalid_appendable(value))?;
                fill(&mut self.appendable, key, flag)
            }
        }
    }

    /// Compares each route with the value routed under its key, in the order of `routes`, and
    /// answers the first that disagrees with the request. A key the call does not list is not
    /// compared: Cloud Storage answered a routed project on GetObject for the bucket the
    /// metadata left out (recorded 2026-11-18, case grpc-probe-routing).
    fn compare(&self, routes: &[Route<'_>]) -> Result<(), Status> {
        routes
            .iter()
            .try_for_each(|&route| self.compare_route(route))
    }

    fn compare_route(&self, route: Route<'_>) -> Result<(), Status> {
        match route {
            Route::Bucket(requested) => {
                let requested = request_bucket(requested);
                let routed = routed_bucket(self.bucket.as_ref());
                agree(routed, requested, || other_bucket(requested, routed))
            }
            Route::DestinationBucket(requested) => {
                let requested = request_bucket(requested);
                let routed = routed_bucket(self.bucket.as_ref());
                agree(routed, requested, || {
                    other_destination_bucket(requested, routed)
                })
            }
            Route::SourceBucket(requested) => {
                let requested = request_bucket(requested);
                let routed = routed_bucket(self.source_bucket.as_ref());
                agree(routed, requested, || other_source_bucket(requested, routed))
            }
            Route::Project(requested) => match &self.project {
                Some(routed) => {
                    let (requested, routed) = (request_project(requested), routed.as_str());
                    agree(routed, requested, || other_project(requested, routed))
                }
                // Not compared when not routed; see `Route::Project`.
                None => Ok(()),
            },
            Route::Appendable(requested) => match self.appendable {
                Some(routed) => agree(&routed, &requested, conflict),
                None => Ok(()),
            },
        }
    }
}

/// What [`admit`] let a call through with. The dispatch stores it in the request's extensions
/// for every call, and a handler that compares resources it learns later reads it back through
/// [`Admitted::of`].
#[derive(Clone, Debug)]
pub(super) enum Admitted {
    /// The call carried a non-empty `x-goog-request-params` value.
    Routed(Routing),
    /// An upload ID call without the metadata. Only [`Routes::Upload`] admits one; its handler
    /// refuses it once the upload ID names a bucket ([`Admitted::compare_upload`]).
    Unrouted,
    /// A test called the handler directly, without the dispatch, so nothing was admitted and
    /// nothing is compared. Only test builds have it, which reach it through
    /// `crate::grpc::v2::test_support::undispatched`.
    #[cfg(test)]
    Undispatched,
}

impl Admitted {
    /// The admission the dispatch stored in a request's `extensions`. Every dispatched call
    /// carries one, so a handler reached without it is the emulator's fault, answered INTERNAL
    /// rather than compared as nothing.
    pub(super) fn of(extensions: &Extensions) -> Result<&Self, Status> {
        extensions.get().ok_or_else(|| {
            Status::internal("A Storage v2 call reached its handler without routing admission")
        })
    }

    /// Compares each route with the value routed under its key, in the order of `routes` (see
    /// [`Routing::compare`]). A call admitted without the metadata, which only an upload ID
    /// call can be, is refused as a call without it.
    pub(super) fn compare(&self, routes: &[Route<'_>]) -> Result<(), Status> {
        match self {
            Self::Routed(routing) => routing.compare(routes),
            Self::Unrouted => Err(missing_metadata()),
            #[cfg(test)]
            Self::Undispatched => Ok(()),
        }
    }

    /// Compares the routing of a call that names its session by upload ID with `bucket`, the
    /// bucket the ID names: the metadata must have been sent and must route that bucket.
    /// Recorded for QueryWriteStatus and CancelResumableWrite with a readable ID (case
    /// grpc-probe-inferred-routing, steps `*-without-routing` and `*-routed-other-bucket`). Not
    /// recorded for a resumed WriteObject or BidiWriteObject; inferred from those two RPCs. The
    /// Go SDK routes the session's bucket when it resumes, so a client that resumes as it does
    /// passes.
    ///
    /// The handler calls this only after it has authorized the caller for `bucket`, so a caller
    /// without access to the bucket is refused the same whatever it routed, and no routing
    /// answer precedes its refusal. That order is a local choice: no recorded call sent missing
    /// or rejected credentials.
    pub(super) fn compare_upload(&self, bucket: &str) -> Result<(), Status> {
        self.compare(&[Route::Bucket(Some(bucket))])
    }
}

/// The check every RPC passes before its handler runs: the metadata must be sent, unless the
/// call's routes are `Upload`, and must agree with the request. The refusal of a call without
/// the metadata was recorded on most RPCs, and follows them on the rest.
///
/// The metadata is read only here. A `Deferred` or `Upload` call's handler compares the
/// returned admission once it learns its resources; the dispatch hands it over in the
/// request's extensions.
pub(super) fn admit(metadata: &MetadataMap, routes: Routes<'_>) -> Result<Admitted, Status> {
    match (routes, Routing::parse(metadata)?) {
        (Routes::Upload, None) => Ok(Admitted::Unrouted),
        (Routes::Request(_) | Routes::Deferred, None) => Err(missing_metadata()),
        (Routes::Request(compared), Some(routing)) => {
            routing.compare(compared)?;
            Ok(Admitted::Routed(routing))
        }
        (Routes::Deferred | Routes::Upload, Some(routing)) => Ok(Admitted::Routed(routing)),
    }
}

pub(super) fn iam_bucket(resource: &str) -> &str {
    resource
        .split_once("/managedFolders/")
        .or_else(|| resource.split_once("/objects/"))
        .map_or(resource, |(bucket, _)| bucket)
}

/// The bucket resource a write stream's first message names in a spec, or `None` when it names
/// none. A resumed write's upload ID names its bucket as a bare ID, which the handler compares
/// once it has authorized the caller for it ([`Admitted::compare_upload`]).
pub(super) fn write_bucket(first: Option<&FirstMessage>) -> Option<&str> {
    match first? {
        FirstMessage::WriteObjectSpec(spec) => spec.resource.as_ref().map(|r| r.bucket.as_str()),
        FirstMessage::AppendObjectSpec(spec) => Some(spec.bucket.as_str()),
        FirstMessage::UploadId(_) => None,
    }
}

/// Compares the admission of a write stream with its first message: the bucket a spec names and
/// the appendable flag. A resumed write compares only the flag here, since a resumable session
/// is never appendable; its bucket is compared after authorization ([`write_bucket`]).
pub(super) fn write(admitted: &Admitted, first: Option<&FirstMessage>) -> Result<(), Status> {
    let appendable = match first {
        Some(FirstMessage::WriteObjectSpec(spec)) => spec.appendable == Some(true),
        Some(FirstMessage::AppendObjectSpec(_)) => true,
        Some(FirstMessage::UploadId(_)) => return admitted.compare(&[Route::Appendable(false)]),
        None => false,
    };
    admitted.compare(&[
        Route::Bucket(write_bucket(first)),
        Route::Appendable(appendable),
    ])
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

    /// The admission of a write stream routed by `params`.
    fn write_stream(params: &str) -> Admitted {
        admit(&routed(params), Routes::Deferred).unwrap()
    }

    /// A resumed write's first message compares only the appendable flag, since a resumable
    /// session is never appendable. Its bucket is the one its upload ID names, which the handler
    /// compares with `compare_upload` once it has authorized the caller, as QueryWriteStatus and
    /// CancelResumableWrite do (recorded, case grpc-probe-inferred-routing). Not recorded for a
    /// resumed write; inferred from those two RPCs.
    #[test]
    fn routing_compares_only_the_appendable_flag_of_a_resumed_write() {
        let resumed = |bucket: &str| FirstMessage::UploadId(format!("{bucket}/{}", "a".repeat(64)));
        let routing = write_stream("bucket=projects%2F_%2Fbuckets%2Ftest-bucket&appendable=false");
        write(&routing, Some(&resumed("test-bucket"))).unwrap();
        write(&routing, Some(&resumed("other-bucket"))).unwrap();
        // The bucket the upload ID names is compared after authorization.
        routing.compare_upload("test-bucket").unwrap();
        let error = routing.compare_upload("other-bucket").unwrap_err();
        assert_eq!(
            error.message(),
            other_bucket_message("other-bucket", "test-bucket")
        );
        assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET");
        let unrouted = write_stream("appendable=false");
        write(&unrouted, Some(&resumed("test-bucket"))).unwrap();
        assert_eq!(
            unrouted
                .compare_upload("test-bucket")
                .unwrap_err()
                .message(),
            other_bucket_message("test-bucket", "")
        );
        // A resumable session is never appendable.
        let appendable = write_stream("appendable=true");
        assert_eq!(
            write(&appendable, Some(&resumed("test-bucket")))
                .unwrap_err()
                .message(),
            CONFLICT
        );
    }

    /// A deferred call's handler compares the routing `admit` read, without reading the
    /// metadata again.
    #[test]
    fn deferred_routing_is_compared_with_what_admit_read() {
        let routing = admit(
            &routed("bucket=projects%2F_%2Fbuckets%2Fb&unknown=1"),
            Routes::Deferred,
        )
        .unwrap();
        routing
            .compare(&[Route::Bucket(Some("projects/_/buckets/b"))])
            .unwrap();
        assert_eq!(
            routing
                .compare(&[Route::Bucket(Some("projects/_/buckets/other"))])
                .unwrap_err()
                .message(),
            other_bucket_message("other", "b")
        );
        // A rewrite token may resolve a bare source bucket ID.
        let rewrite = admit(
            &routed("source_bucket=projects%2F_%2Fbuckets%2Fa&bucket=projects%2F_%2Fbuckets%2Fb"),
            Routes::Deferred,
        )
        .unwrap();
        rewrite
            .compare(&[
                Route::SourceBucket(Some("a")),
                Route::Bucket(Some("projects/_/buckets/b")),
            ])
            .unwrap();
        // Recorded 2026-11-19 (case grpc-probe-inferred-routing): the source is compared before
        // the destination, each answered in its own words.
        let error = rewrite
            .compare(&[
                Route::SourceBucket(Some("b")),
                Route::DestinationBucket(Some("b")),
            ])
            .unwrap_err();
        assert_eq!(
            error.message(),
            "The x-goog-request-params must specify the same source and destination buckets as in the request. Request source bucket was 'b' but x-goog-request-params specified 'a'"
        );
        assert_eq!(
            reason(&error),
            "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_SOURCE_BUCKET"
        );
        let error = rewrite
            .compare(&[
                Route::SourceBucket(Some("a")),
                Route::DestinationBucket(Some("c")),
            ])
            .unwrap_err();
        assert_eq!(
            error.message(),
            "The x-goog-request-params must specify the same source and destination buckets as the resource in the request. Request destination bucket was 'c', but x-goog-request-params specified 'b'"
        );
        assert_eq!(
            reason(&error),
            "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_DEST_BUCKET"
        );
        // Metadata that routes only one side compares the other as the empty name.
        let routes = [
            Route::SourceBucket(Some("a")),
            Route::DestinationBucket(Some("b")),
        ];
        let bucket_only = admit(
            &routed("bucket=projects%2F_%2Fbuckets%2Fb"),
            Routes::Deferred,
        )
        .unwrap();
        assert!(
            bucket_only
                .compare(&routes)
                .unwrap_err()
                .message()
                .ends_with("Request source bucket was 'a' but x-goog-request-params specified ''")
        );
        let source_only = admit(
            &routed("source_bucket=projects%2F_%2Fbuckets%2Fa"),
            Routes::Deferred,
        )
        .unwrap();
        assert!(
            source_only
                .compare(&routes)
                .unwrap_err()
                .message()
                .ends_with(
                    "Request destination bucket was 'b', but x-goog-request-params specified ''"
                )
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
            Routes::Deferred,
            Routes::Deferred,
            Routes::Deferred,
        ] {
            let error = admit(&none, routes).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument);
            assert_eq!(
                error.message(),
                "An x-goog-request-params request metadata property must be provided for this request."
            );
            assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS");
        }
        // Only an upload ID call is admitted without the metadata, and whatever it is compared
        // with, it is refused as a call without it.
        let unrouted = admit(&none, Routes::Upload).unwrap();
        assert!(matches!(unrouted, Admitted::Unrouted));
        for routes in [
            [Route::Bucket(Some("projects/_/buckets/b"))],
            [Route::Appendable(false)],
        ] {
            let error = unrouted.compare(&routes).unwrap_err();
            assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS");
        }
        // A handler reached without the dispatch's admission answers INTERNAL instead of
        // comparing nothing.
        let error = Admitted::of(&Extensions::new()).unwrap_err();
        assert_eq!(error.code(), Code::Internal);
        let mut extensions = Extensions::new();
        extensions.insert(unrouted);
        assert!(matches!(
            Admitted::of(&extensions).unwrap(),
            Admitted::Unrouted
        ));
    }

    /// Recorded 2026-11-18 and 2026-11-19 (cases grpc-probe-resumable-sessions and
    /// grpc-probe-inferred-routing): an upload ID RPC reaches its handler without the metadata,
    /// which answers a malformed ID first; for a readable ID the metadata is then required and
    /// its bucket must be the one the ID names.
    #[test]
    fn upload_routing_is_compared_once_the_upload_id_is_read() {
        let none = admit(&MetadataMap::new(), Routes::Upload).unwrap();
        let error = none.compare_upload("b").unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(
            error.message(),
            "An x-goog-request-params request metadata property must be provided for this request."
        );
        assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS");
        // An empty value is no metadata.
        let empty = admit(&routed(""), Routes::Upload).unwrap();
        assert_eq!(
            empty.compare_upload("b").unwrap_err().message(),
            error.message()
        );
        let same = admit(&routed("bucket=projects%2F_%2Fbuckets%2Fb"), Routes::Upload).unwrap();
        same.compare_upload("b").unwrap();
        let error = same.compare_upload("other").unwrap_err();
        assert_eq!(error.message(), other_bucket_message("other", "b"));
        assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET");
        // Metadata that routes no bucket is compared as routing the empty name.
        let project = admit(&routed("project=projects%2Fp"), Routes::Upload).unwrap();
        assert_eq!(
            project.compare_upload("b").unwrap_err().message(),
            other_bucket_message("b", "")
        );
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
        let appendable = write_stream("bucket=projects%2F_%2Fbuckets%2Fb&appendable=true");
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
        let bucket = write_stream("bucket=projects%2F_%2Fbuckets%2Fb");
        assert_eq!(
            write(&bucket, None).unwrap_err().message(),
            other_bucket_message("", "b")
        );
        // A resumed write's first message compares no bucket; the handler answers an upload ID
        // it cannot read before it compares one.
        let unreadable = FirstMessage::UploadId("other/not-hex".into());
        write(&bucket, Some(&unreadable)).unwrap();

        // Recorded 2026-11-19 on ListBuckets (case grpc-probe-inferred-routing).
        let error = admit(
            &routed("project=projects%2Fother"),
            Routes::Request(&[Route::Project("projects/p")]),
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "The x-goog-request-params must specify the same project as the resource in the request. Request project was 'p', but x-goog-request-params specified 'other'"
        );
        assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_PROJECT");
    }

    /// A routed project is a resource name `projects/<id>`, compared by its ID with the ID the
    /// request names. The recorded form and mismatch are cited at `RoutedProject` and
    /// `Route::Project`; the rest is inferred from them.
    #[test]
    fn routed_projects_are_resource_names_compared_by_id() {
        fn list_buckets(params: &str, parent: &str) -> Result<Admitted, Status> {
            admit(&routed(params), Routes::Request(&[Route::Project(parent)]))
        }
        list_buckets("project=projects%2Fp", "projects/p").unwrap();
        // A malformed request project is compared as written and left to its handler, so a
        // routed name with the same ID never contradicts it.
        list_buckets("project=projects%2Fp", "p").unwrap();
        assert_eq!(
            list_buckets("project=projects%2Fp", "q")
                .unwrap_err()
                .message(),
            "The x-goog-request-params must specify the same project as the resource in the request. Request project was 'q', but x-goog-request-params specified 'p'"
        );
        // A routed project of another form is refused in the words of a malformed parent, on any
        // RPC (recorded beside the same malformed parent, case grpc-bucket-list).
        for (value, name) in [
            ("buckets%2Fx", "buckets/x"),
            ("p", "p"),
            ("projects%2Fp%2Fx", "projects/p/x"),
        ] {
            let params = format!("bucket=projects%2F_%2Fbuckets%2Fb&project={value}");
            for error in [
                list_buckets(&params, "projects/p").unwrap_err(),
                admit(&routed(&params), GET_OBJECT).unwrap_err(),
            ] {
                assert_eq!(error.code(), Code::InvalidArgument, "{value}");
                assert_eq!(
                    error.message(),
                    format!(
                        "Invalid project name '{name}'. Project names must take the form 'projects/{{project_number}}'."
                    )
                );
                assert!(error.details().is_empty(), "{value}");
            }
        }
        // Metadata that routes no project is not compared (not recorded; see `Route::Project`).
        list_buckets("bucket=projects%2F_%2Fbuckets%2Fb", "projects/p").unwrap();
    }

    /// Recorded 2026-11-18 and 2026-11-19 (cases grpc-probe-routing and
    /// grpc-probe-inferred-routing): an unknown key, a `routing_token`, a malformed unknown pair,
    /// a trailing `&` and a percent-encoded key beside the request's bucket are served, and a
    /// key the RPC does not route is not compared.
    #[test]
    fn routing_ignores_unknown_keys_and_compares_only_the_routed_keys() {
        let routes = [Route::Bucket(Some("b"))];
        for params in [
            "bucket=projects%2F_%2Fbuckets%2Fb&routing_token=token",
            "bucket=projects%2F_%2Fbuckets%2Fb&gcsmimic_unknown=1",
            "direct_connectivity_diagnostic=1&bucket=projects%2F_%2Fbuckets%2Fb",
            "bucket=projects%2F_%2Fbuckets%2Fb&project=projects%2Fp&appendable=true",
        ] {
            for admitted in [Routes::Request(&routes), Routes::Deferred] {
                admit(&routed(params), admitted).unwrap();
            }
        }
        admit(
            &routed("bucket=projects%2F_%2Fbuckets%2Fb&unknown=1"),
            Routes::Request(&[Route::Project("projects/p")]),
        )
        .unwrap();
    }

    /// A pair under a key no RPC routes is never interpreted, so however malformed it is it
    /// cannot fail the call. Recorded for `unknown=%GG`, a trailing `&` and a percent-encoded
    /// key (case grpc-probe-inferred-routing); the rest are inferred from them.
    #[test]
    fn routing_skips_malformed_pairs_under_unknown_keys() {
        for params in [
            b"bucket=projects%2F_%2Fbuckets%2Fb&unknown=%GG".as_slice(),
            b"bucket=projects%2F_%2Fbuckets%2Fb&unknown",
            b"bucket=projects%2F_%2Fbuckets%2Fb&",
            b"&bucket=projects%2F_%2Fbuckets%2Fb",
            b"bucket=projects%2F_%2Fbuckets%2Fb&&=",
            b"%GG=1&bucket=projects%2F_%2Fbuckets%2Fb",
            b"bucket=projects%2F_%2Fbuckets%2Fb&unknown=\xe9",
        ] {
            admit(&routed_by(&[params]), GET_OBJECT)
                .unwrap_or_else(|error| panic!("{}: {error}", params.escape_ascii()));
        }
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
        // An empty value routes nothing, so it is no metadata (recorded 2026-11-19).
        for routes in [Routes::Request(&[Route::Bucket(None)]), Routes::Deferred] {
            let error = admit(&routed(""), routes).unwrap_err();
            assert_eq!(reason(&error), "GRPC_INVALID_X_GOOG_REQUEST_PARAMS");
        }
    }

    /// Every call validates a routed `bucket`, even on an RPC that routes none (recorded
    /// 2026-11-19 on ListBuckets beside the right project, and on GetObject for a pair without a
    /// value, case grpc-probe-inferred-routing). The message quotes the whole metadata value.
    /// A malformed source bucket or flag is inferred from it.
    #[test]
    fn routing_validates_every_routed_key_whichever_rpc_is_called() {
        let list_buckets = Routes::Request(&[Route::Project("projects/p")]);
        for (params, routes) in [
            ("bucket=bad", GET_OBJECT),
            ("bucket=bad", Routes::Deferred),
            ("bucket", GET_OBJECT),
            ("%62ucket", GET_OBJECT),
            ("project=projects%2Fp&bucket=bad", list_buckets),
            ("project=projects%2Fp&bucket=bad", Routes::Upload),
        ] {
            let error = admit(&routed(params), routes).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument, "{params}");
            assert_eq!(
                error.message(),
                format!(
                    "Invalid bucket name found in x-goog-request-params metadata ({params}). Must be of the form `projects/_/buckets/{{bucket_id}}`"
                )
            );
            assert_eq!(
                reason(&error),
                "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT"
            );
        }
        // A source bucket is worded like a request's malformed bucket name.
        let error = admit(
            &routed("source_bucket=bad&bucket=projects%2F_%2Fbuckets%2Fb"),
            GET_OBJECT,
        )
        .unwrap_err();
        assert!(
            error.message().starts_with("Invalid bucket name 'bad'."),
            "{error}"
        );
        assert_eq!(reason(&error), "GRPC_INVALID_BUCKET");
        // A flag is spelled `true` or `false`.
        for flag in ["maybe", "TRUE"] {
            let error = admit(
                &routed(&format!(
                    "bucket=projects%2F_%2Fbuckets%2Fb&appendable={flag}"
                )),
                GET_OBJECT,
            )
            .unwrap_err();
            assert_eq!(
                error.message(),
                format!(
                    "Invalid appendable value found in x-goog-request-params metadata (appendable={flag}). Must be true or false"
                )
            );
        }
        for params in [b"bucket=%GG".as_slice(), b"project=projects%2Fp&bucket=%GG"] {
            let error = admit(&routed_by(&[params]), GET_OBJECT).unwrap_err();
            assert_eq!(error.message(), "Malformed URI encoding");
        }
        let error = admit(&routed_by(&[b"bucket=\xe9"]), GET_OBJECT).unwrap_err();
        assert_eq!(
            error.message(),
            "Invalid routing metadata; use bucket=projects%2F_%2Fbuckets%2Fexample-bucket"
        );
    }

    /// A key sent more than once is refused before any comparison, even with the same value and
    /// across several metadata values (recorded 2026-11-19 on GetObject's `bucket`, case
    /// grpc-probe-inferred-routing); other keys follow `bucket` by inference.
    #[test]
    fn routing_refuses_a_duplicated_key() {
        for metadata in [
            routed("bucket=projects%2F_%2Fbuckets%2Fb&bucket=projects%2F_%2Fbuckets%2Fb"),
            routed("bucket=projects%2F_%2Fbuckets%2Fb&bucket=projects%2F_%2Fbuckets%2Fother"),
            routed("bucket=projects%2F_%2Fbuckets%2Fother&bucket=projects%2F_%2Fbuckets%2Fb"),
            routed("bucket=projects%2F_%2Fbuckets%2Fb&%62ucket=projects%2F_%2Fbuckets%2Fb"),
            routed_by(&[
                b"bucket=projects%2F_%2Fbuckets%2Fb",
                b"bucket=projects%2F_%2Fbuckets%2Fb",
            ]),
        ] {
            let error = admit(&metadata, GET_OBJECT).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument);
            assert_eq!(
                error.message(),
                "The x-goog-request-params metadata has duplicate entries for the key 'bucket'."
            );
            assert_eq!(
                reason(&error),
                "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_DUPLICATE_ENTRY"
            );
        }
        let error = admit(
            &routed("project=projects%2Fp&project=projects%2Fp"),
            Routes::Request(&[Route::Project("projects/p")]),
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "The x-goog-request-params metadata has duplicate entries for the key 'project'."
        );
    }
}
