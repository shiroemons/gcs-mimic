use super::{Storage, pb, status::unsupported_for};
use super::{convert, names};
use crate::auth::Caller;
use crate::grpc::validate::bucket_conditions;
use crate::{bucket::CatalogError, json};
use serde_json::json;
use tonic::{Request, Response, Status};

/// `?` would bypass the v2 NotEmpty wording and anonymous authorization mapping.
fn catalog_error(error: CatalogError, caller: Option<Caller<'_>>) -> Status {
    if matches!(error, CatalogError::NotEmpty) {
        return Status::failed_precondition(
            "Bucket is not empty; delete its objects and versions before deleting the bucket",
        );
    }
    super::access_status(error, caller)
}

fn require_caller(caller: Option<Caller<'_>>) -> Result<(), Status> {
    if matches!(caller, Some(Caller::Anonymous)) {
        return Err(super::auth_error(crate::auth::AuthFailure::LoginRequired));
    }
    Ok(())
}

fn project_name(resource: &str) -> Result<&str, Status> {
    let project = resource
        .strip_prefix("projects/")
        .filter(|id| !id.contains('/'))
        .ok_or_else(|| {
            Status::invalid_argument(format!(
                "Invalid project name '{resource}'. Project names must take the form 'projects/{{project_number}}'."
            ))
        })?;
    json::validate_project(project)?;
    // `Bucket.project` renders a project with no number as `projects/0` (see
    // `acl::project_number`). That value names no single project, so taking it back as an ID
    // would silently address a different project called "0".
    if project.bytes().all(|byte| byte.is_ascii_digit())
        && crate::acl::project_number(project) == "0"
    {
        return Err(Status::invalid_argument(
            "Invalid project; projects/0 names no project, use projects/local-project or projects/_",
        ));
    }
    Ok(project)
}

fn routing<T>(request: &Request<T>, field: &str, resource: &str) -> Result<(), Status> {
    if request.metadata().contains_key("x-goog-user-project") {
        return Err(super::status::unsupported(
            "x-goog-user-project billing metadata; omit UserProject on bucket calls",
        ));
    }
    for metadata in request.metadata().get_all("x-goog-request-params").iter() {
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
            if key == "bucket" && names::bucket_name(&value).is_err() {
                return Err(super::status::invalid_with_reason(
                    format!(
                        "Invalid bucket name found in x-goog-request-params metadata (bucket={value}). Must be of the form `projects/_/buckets/{{bucket_id}}`"
                    ),
                    "GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT",
                ));
            }
            if key == field && value != resource {
                return Err(Status::invalid_argument(
                    "Routing metadata conflicts with the request; use the same resource in x-goog-request-params and the request",
                ));
            }
            // The ordinary Go client appends this diagnostic even on emulator endpoints.
            if key != field && key != "direct_connectivity_diagnostic" {
                return Err(super::status::unsupported("bucket routing metadata field"));
            }
        }
    }
    Ok(())
}

fn bucket_mask(
    bucket: pb::Bucket,
    mask: Option<&prost_types::FieldMask>,
) -> Result<pb::Bucket, Status> {
    let Some(mask) = mask.filter(|mask| !mask.paths.is_empty()) else {
        return Ok(bucket);
    };
    if mask.paths.iter().any(|path| path == "*") {
        if mask.paths.len() == 1 {
            return Ok(bucket);
        }
        return Err(super::status::unsupported(
            "mixed wildcard bucket read_mask",
        ));
    }
    let mut selected = pb::Bucket::default();
    for path in &mask.paths {
        macro_rules! select { ($($field:ident),* $(,)?) => { match path.as_str() {
            $(stringify!($field) => selected.$field = bucket.$field.to_owned(),)*
            _ => return Err(super::status::unsupported(&format!("bucket read_mask {path}"))),
        } }; }
        select!(
            name,
            bucket_id,
            etag,
            project,
            metageneration,
            location,
            location_type,
            storage_class,
            autoclass,
            labels,
            create_time,
            update_time,
            default_event_based_hold,
            versioning,
            cors,
            lifecycle,
            retention_policy,
            soft_delete_policy,
            object_retention,
            iam_config,
            acl,
            default_object_acl,
            owner
        );
    }
    Ok(selected)
}

/// Whether a read without a mask includes the ACL fields: GetBucket defaults to all fields,
/// ListBuckets to all fields except `acl`, `default_object_acl` and `owner`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AclDefault {
    Include,
    Exclude,
}

pub(super) fn wants_acl(mask: Option<&prost_types::FieldMask>, default: AclDefault) -> bool {
    match mask.filter(|mask| !mask.paths.is_empty()) {
        None => default == AclDefault::Include,
        Some(mask) => mask
            .paths
            .iter()
            .any(|path| matches!(path.as_str(), "*" | "acl" | "default_object_acl" | "owner")),
    }
}

/// Clears a message field that a client sent empty, which carries no supported setting.
fn clear_default<T: Default + PartialEq>(field: &mut Option<T>) {
    if field.as_ref().is_some_and(|v| v == &T::default()) {
        *field = None;
    }
}

/// Adds the retention, soft-delete and IAM settings of a CreateBucket request to `body`.
fn create_policies(bucket: &pb::Bucket, body: &mut serde_json::Value) -> Result<(), Status> {
    for (field, key, policy) in [
        (
            "retentionPolicy",
            "retentionPeriod",
            bucket.retention_policy.as_ref().map(|p| {
                (
                    p.retention_duration.as_ref(),
                    p.effective_time.is_some() || p.is_locked,
                )
            }),
        ),
        (
            "softDeletePolicy",
            "retentionDurationSeconds",
            bucket
                .soft_delete_policy
                .as_ref()
                .map(|p| (p.retention_duration.as_ref(), p.effective_time.is_some())),
        ),
    ] {
        if let Some((duration, output_fields)) = policy {
            if output_fields {
                return Err(super::status::unsupported(
                    "create bucket policy output fields",
                ));
            }
            if let Some(duration) = duration {
                if duration.nanos != 0 {
                    return Err(super::status::unsupported(
                        "fractional bucket retention duration",
                    ));
                }
                body[field] = json!({key: duration.seconds.to_string()});
            }
        }
    }
    if let Some(iam) = &bucket.iam_config {
        if !iam.public_access_prevention.is_empty() {
            body["iamConfiguration"]["publicAccessPrevention"] =
                json!(iam.public_access_prevention);
        }
        if let Some(uniform) = &iam.uniform_bucket_level_access {
            if uniform.lock_time.is_some() {
                return Err(super::status::unsupported("create bucket IAM lock_time"));
            }
            body["iamConfiguration"]["uniformBucketLevelAccess"] =
                json!({"enabled": uniform.enabled});
        }
    }
    Ok(())
}

pub(super) fn cors_json(cors: &[pb::bucket::Cors]) -> serde_json::Value {
    cors.iter()
        .map(|cors| {
            json!({
                "origin": cors.origin,
                "method": cors.method,
                "responseHeader": cors.response_header,
                "maxAgeSeconds": cors.max_age_seconds,
            })
        })
        .collect()
}

/// Builds create-only JSON metadata without introducing update-mask reset defaults.
pub(super) fn create_fields(bucket: &pb::Bucket, name: &str) -> Result<serde_json::Value, Status> {
    let mut body = json!({"name": name, "defaultEventBasedHold": bucket.default_event_based_hold});
    for (field, value) in [
        ("location", &bucket.location),
        ("storageClass", &bucket.storage_class),
    ] {
        if !value.is_empty() {
            body[field] = json!(value);
        }
    }
    body["labels"] = json!(bucket.labels);
    body["cors"] = cors_json(&bucket.cors);
    if let Some(versioning) = &bucket.versioning {
        body["versioning"] = json!({"enabled": versioning.enabled});
    }
    create_policies(bucket, &mut body)?;
    if let Some(autoclass) = bucket.autoclass.as_ref() {
        if autoclass.toggle_time.is_some() || autoclass.terminal_storage_class_update_time.is_some()
        {
            return Err(super::status::unsupported(
                "create bucket autoclass output fields",
            ));
        }
        body["autoclass"] = json!({"enabled": autoclass.enabled});
        if let Some(class) = &autoclass.terminal_storage_class {
            body["autoclass"]["terminalStorageClass"] = class.clone().into();
        }
    }
    Ok(body)
}

fn create_metadata(
    bucket: &pb::Bucket,
    name: &str,
    project: &str,
    state: &crate::server::State,
) -> Result<crate::bucket::BucketRecord, Status> {
    let mut remaining = bucket.clone();
    let body = create_fields(bucket, name)?;
    remaining.project.clear();
    remaining.location.clear();
    remaining.storage_class.clear();
    remaining.labels.clear();
    remaining.cors.clear();
    remaining.versioning = None;
    remaining.default_event_based_hold = false;
    remaining.retention_policy = None;
    remaining.soft_delete_policy = None;
    remaining.iam_config = None;
    if remaining.rpo == "DEFAULT" {
        remaining.rpo.clear();
    }
    remaining.autoclass = None;
    clear_default(&mut remaining.billing);
    clear_default(&mut remaining.hierarchical_namespace);
    clear_default(&mut remaining.custom_placement_config);
    clear_default(&mut remaining.object_retention);
    clear_default(&mut remaining.lifecycle);
    clear_default(&mut remaining.encryption);
    clear_default(&mut remaining.logging);
    clear_default(&mut remaining.ip_filter);
    if remaining != pb::Bucket::default() {
        return Err(super::status::unsupported(
            "CreateBucket fields outside the supported metadata subset",
        ));
    }
    json::parse_insert_with_profile(
        &serde_json::to_vec(&body)
            .map_err(|_| Status::internal("Cannot encode bucket metadata"))?,
        project,
        state.clock.now(),
        crate::config::CompatProfile::Native,
        &state.default_location,
    )
    .map_err(Status::from)
}

impl Storage {
    /// Adds `owner`, `acl` and `default_object_acl` as JSON `projection=full` does. The ACL
    /// is omitted under uniform bucket-level access and for callers that may not read it;
    /// the read itself still succeeds.
    pub(super) async fn add_bucket_acl(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &mut pb::Bucket,
    ) -> Result<(), Status> {
        use crate::{acl::AclKind, json::acl::AclOwner};
        let name = bucket.bucket_id.clone();
        let readable =
            match crate::server::may_read_acl(&self.state, caller, AclKind::Bucket, &name).await {
                Ok(readable) => readable,
                Err(error) if error.status == hyper::StatusCode::UNAUTHORIZED => false,
                Err(error) => return Err(Status::from(error)),
            };
        if !readable {
            return Ok(());
        }
        let Some(acl) = self.state.catalog.acl_projection(&name, Vec::new()).await? else {
            return Ok(());
        };
        bucket.owner = Some(pb::Owner {
            entity: format!(
                "project-owners-{}",
                crate::acl::project_number(&acl.project)
            ),
            entity_id: String::new(),
        });
        if acl.uniform_access {
            return Ok(());
        }
        let base = &self.state.api_base_url;
        let owner = |kind| AclOwner {
            kind,
            bucket: &name,
            object: None,
            metageneration: bucket.metageneration,
        };
        let rendered = |kind, entry| json::acl::acl_entry_resource(base, owner(kind), entry);
        bucket.acl = acl
            .bucket
            .iter()
            .map(|entry| convert::bucket_access_control(&rendered(AclKind::Bucket, entry)))
            .collect();
        bucket.default_object_acl = acl
            .default_object
            .iter()
            .map(|entry| convert::object_access_control(&rendered(AclKind::DefaultObject, entry)))
            .collect();
        Ok(())
    }

    pub(super) async fn delete_bucket(
        &self,
        request: Request<pb::DeleteBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        routing(&request, "bucket", &request.get_ref().name)?;
        let request = request.into_inner();
        let name = names::bucket_name(&request.name)?;
        json::validate_bucket_name(name)?;
        let access = self
            .access(caller, name, crate::auth::BUCKETS_DELETE)
            .await?;
        self.state
            .catalog
            .delete_bucket(name, bucket_conditions!(request), &access)
            .await
            .map_err(|error| catalog_error(error, caller))?;
        Ok(Response::new(()))
    }
    pub(super) async fn get_bucket(
        &self,
        request: Request<pb::GetBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        routing(&request, "bucket", &request.get_ref().name)?;
        let request = request.into_inner();
        let name = names::bucket_name(&request.name)?;
        json::validate_bucket_name(name)?;
        bucket_mask(pb::Bucket::default(), request.read_mask.as_ref())?;
        let access = self.access(caller, name, crate::auth::BUCKETS_GET).await?;
        let record = self
            .state
            .catalog
            .get_bucket(name, &access)
            .await
            .map_err(|error| catalog_error(error, caller))?
            .ok_or_else(|| catalog_error(CatalogError::NotFound, caller))?;
        if !bucket_conditions!(request).hold(record.metageneration) {
            return Err(catalog_error(CatalogError::PreconditionFailed, caller));
        }
        let mut bucket = convert::bucket(&record);
        if wants_acl(request.read_mask.as_ref(), AclDefault::Include) {
            self.add_bucket_acl(caller, &mut bucket).await?;
        }
        Ok(Response::new(bucket_mask(
            bucket,
            request.read_mask.as_ref(),
        )?))
    }
    pub(super) async fn create_bucket(
        &self,
        request: Request<pb::CreateBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        require_caller(caller)?;
        routing(
            &request,
            "project",
            request
                .get_ref()
                .bucket
                .as_ref()
                .map(|bucket| bucket.project.as_str())
                .unwrap_or(&request.get_ref().parent),
        )?;
        let request = request.into_inner();
        if !request.parent.is_empty() && request.parent != "projects/_" {
            return Err(Status::invalid_argument(
                "CreateBucket.parent must be empty or projects/_; put projects/local-project in bucket.project",
            ));
        }
        if !request.predefined_acl.is_empty() || !request.predefined_default_object_acl.is_empty() {
            return Err(unsupported_for("CreateBucket predefined ACLs", caller));
        }
        let mut bucket = request.bucket.unwrap_or_default();
        // The Java SDK repeats the new resource name here, and Cloud Storage accepts it. A
        // different name would leave two candidates for the bucket's name, so it is rejected.
        if !bucket.name.is_empty() {
            if names::bucket_name(&bucket.name).ok() != Some(request.bucket_id.as_str()) {
                return Err(Status::invalid_argument(
                    "CreateBucket.bucket.name must be empty or equal projects/_/buckets/<bucket_id>; use bucket_id=example-bucket",
                ));
            }
            bucket.name.clear();
        }
        let project = project_name(&bucket.project)?;
        if let Some(caller) = caller {
            caller.authorize(crate::auth::BUCKETS_CREATE, Some(project))?;
            if request.enable_object_retention {
                caller.authorize(crate::auth::BUCKETS_ENABLE_OBJECT_RETENTION, Some(project))?;
            }
        }
        // As in JSON, only the request flag enables object retention; the bucket field is
        // output-only and may only repeat the flag.
        if bucket
            .object_retention
            .take()
            .is_some_and(|retention| retention.enabled)
            && !request.enable_object_retention
        {
            return Err(Status::invalid_argument(
                "bucket.object_retention is output-only; set enable_object_retention=true to enable object retention",
            ));
        }
        let mut record = create_metadata(&bucket, &request.bucket_id, project, &self.state)?;
        record.object_retention_enabled = request.enable_object_retention;
        let record = self
            .state
            .catalog
            .insert_bucket(record)
            .await
            .map_err(|error| catalog_error(error, caller))?;
        let mut bucket = convert::bucket(&record);
        self.add_bucket_acl(caller, &mut bucket).await?;
        Ok(Response::new(bucket))
    }
    pub(super) async fn list_buckets(
        &self,
        request: Request<pb::ListBucketsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListBucketsResponse>, Status> {
        require_caller(caller)?;
        routing(&request, "project", &request.get_ref().parent)?;
        let request = request.into_inner();
        let project = project_name(&request.parent)?;
        if let Some(caller) = caller {
            caller.authorize(crate::auth::BUCKETS_LIST, Some(project))?;
        }
        if request.return_partial_success {
            return Err(unsupported_for(
                "ListBuckets return_partial_success",
                caller,
            ));
        }
        let invalid_list = || {
            Status::invalid_argument(
                "Use a nonnegative page_size and a prefix of at most 222 bytes, for example page_size=100",
            )
        };
        let page_size = usize::try_from(request.page_size).map_err(|_| invalid_list())?;
        if request.prefix.len() > 222 {
            return Err(invalid_list());
        }
        bucket_mask(pb::Bucket::default(), request.read_mask.as_ref())?;
        let start = if request.page_token.is_empty() {
            None
        } else {
            Some(json::decode_page_token(
                &request.page_token,
                project,
                &request.prefix,
            )?)
        };
        let limit = if page_size == 0 {
            1000
        } else {
            page_size.min(1000)
        };
        let page = self
            .state
            .catalog
            .list_buckets_page(project, Some(&request.prefix), start.as_deref(), limit)
            .await
            .map_err(|error| catalog_error(error, caller))?;
        // A continuation page is never empty, so its last bucket always names the next start.
        let next_page_token = match page.items.last() {
            Some(last) if page.has_more => {
                json::encode_page_token(project, &request.prefix, &last.name)?
            }
            _ => String::new(),
        };
        let acl = wants_acl(request.read_mask.as_ref(), AclDefault::Exclude);
        let mut buckets = Vec::with_capacity(page.items.len());
        for record in &page.items {
            let mut bucket = convert::bucket(record);
            if acl {
                self.add_bucket_acl(caller, &mut bucket).await?;
            }
            buckets.push(bucket_mask(bucket, request.read_mask.as_ref())?);
        }
        Ok(Response::new(pb::ListBucketsResponse {
            buckets,
            next_page_token,
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::test_support::{self as fixtures, assert_code, caller, registry};
    use crate::storage::test_fixtures::PERMISSIVE;
    use crate::{auth::Auth, storage::Catalog, test_support::TestDir};
    use std::sync::Arc;
    use tonic::Code;

    #[test]
    fn bucket_errors_request_credentials_from_anonymous_callers() {
        let anonymous = catalog_error(CatalogError::Forbidden, Some(Caller::Anonymous));
        let generic = Status::from(CatalogError::Forbidden);
        assert_eq!(anonymous.code(), Code::Unauthenticated);
        assert_eq!(generic.code(), Code::PermissionDenied);
    }
    #[test]
    fn project_name_takes_back_rendered_numbers_and_rejects_the_placeholder() {
        // A numeric ID renders as itself, so the client can pass it back unchanged.
        assert_eq!(project_name("projects/123").unwrap(), "123");
        assert_eq!(
            project_name("projects/local-project").unwrap(),
            "local-project"
        );
        // `projects/0` is what a project without a number renders as; it names no project.
        for placeholder in [
            "projects/0",
            "projects/00",
            "projects/000000000000000000000000",
        ] {
            let error = project_name(placeholder).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument, "{placeholder}");
            assert!(error.message().contains("projects/0"), "{placeholder}");
        }
    }

    async fn storage(strict: bool) -> (TestDir, Storage) {
        let dir = TestDir::new();
        let auth = if strict {
            Auth::strict(registry(&[
                ("admin", "p", "roles/storage.admin"),
                ("reader", "", ""),
            ]))
        } else {
            Auth::permissive()
        };
        let catalog = Catalog::open(&dir.0).await.unwrap();
        (
            dir,
            Storage {
                max_object_bytes: 1024 * 1024 * 1024,
                state: Arc::new(crate::server::State::for_test(catalog, auth)),
            },
        )
    }

    #[test]
    fn create_rejects_output_fields_and_fractional_durations_in_order() {
        type InvalidCase = (&'static str, fn(&mut pb::Bucket));
        const FRACTIONAL: prost_types::Duration = prost_types::Duration {
            seconds: 1,
            nanos: 1,
        };
        let cases: [InvalidCase; 7] = [
            ("create bucket autoclass output fields", |b| {
                b.autoclass
                    .get_or_insert_default()
                    .terminal_storage_class_update_time = Some(Default::default())
            }),
            ("create bucket IAM lock_time", |b| {
                b.iam_config
                    .get_or_insert_default()
                    .uniform_bucket_level_access
                    .get_or_insert_default()
                    .lock_time = Some(Default::default())
            }),
            ("fractional bucket retention duration", |b| {
                b.soft_delete_policy
                    .get_or_insert_default()
                    .retention_duration = Some(FRACTIONAL)
            }),
            ("create bucket policy output fields", |b| {
                b.soft_delete_policy.get_or_insert_default().effective_time =
                    Some(Default::default())
            }),
            ("fractional bucket retention duration", |b| {
                b.retention_policy
                    .get_or_insert_default()
                    .retention_duration = Some(FRACTIONAL)
            }),
            ("create bucket policy output fields", |b| {
                b.retention_policy.get_or_insert_default().is_locked = true
            }),
            ("create bucket policy output fields", |b| {
                let policy = b.retention_policy.get_or_insert_default();
                policy.is_locked = false;
                policy.effective_time = Some(Default::default());
            }),
        ];
        let mut bucket = pb::Bucket::default();
        for (reason, invalidate) in cases {
            // Each higher-priority rejection must supersede the invalid fields already set.
            invalidate(&mut bucket);
            let status = create_fields(&bucket, "test-bucket").unwrap_err();
            assert_eq!(status.code(), tonic::Code::Unimplemented, "{reason}");
            assert!(status.message().contains(reason), "{reason}: {status}");
        }
    }

    fn create(name: &str) -> pb::CreateBucketRequest {
        pb::CreateBucketRequest {
            parent: "projects/_".into(),
            bucket_id: name.into(),
            bucket: Some(pb::Bucket {
                project: "projects/p".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn get(name: &str) -> pb::GetBucketRequest {
        pb::GetBucketRequest {
            name: format!("projects/_/buckets/{name}"),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn grpc_buckets_crud_conditions_and_masks() {
        let (_dir, storage) = storage(false).await;
        let mut request = create("test-bucket");
        let bucket = request.bucket.as_mut().unwrap();
        bucket.location = "asia-northeast1".into();
        bucket.labels.insert("team".into(), "dev".into());
        bucket.versioning = Some(pb::bucket::Versioning { enabled: true });
        let created = storage
            .create_bucket(Request::new(request.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(created.location, "ASIA-NORTHEAST1");
        assert_eq!(created.labels["team"], "dev");
        assert!(created.versioning.unwrap().enabled);
        assert_code(
            storage.create_bucket(Request::new(request), None).await,
            Code::AlreadyExists,
        );
        assert_code(
            storage
                .get_bucket(Request::new(get("missing-bucket")), None)
                .await,
            Code::NotFound,
        );
        for (matching, not_matching, success) in [
            (Some(1), None, true),
            (Some(2), None, false),
            (None, Some(1), false),
            (None, Some(2), true),
        ] {
            let mut request = get("test-bucket");
            request.if_metageneration_match = matching;
            request.if_metageneration_not_match = not_matching;
            let result = storage.get_bucket(Request::new(request), None).await;
            if success {
                assert!(result.is_ok());
            } else {
                assert_code(result, Code::FailedPrecondition);
            }
        }
        let mut request = get("test-bucket");
        request.read_mask = fixtures::mask(&["name", "location"]);
        let masked = storage
            .get_bucket(Request::new(request.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(masked.name, "projects/_/buckets/test-bucket");
        assert_eq!(masked.location, "ASIA-NORTHEAST1");
        assert!(masked.project.is_empty() && masked.labels.is_empty());
        request.read_mask.as_mut().unwrap().paths = vec!["encryption".into()];
        let error = storage
            .get_bucket(Request::new(request), None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::Unimplemented);
        assert!(!error.details().is_empty());
        for (matching, not_matching) in [(Some(2), None), (None, Some(1))] {
            let request = pb::DeleteBucketRequest {
                name: get("test-bucket").name,
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
            };
            assert_code(
                storage.delete_bucket(Request::new(request), None).await,
                Code::FailedPrecondition,
            );
            assert!(
                storage
                    .state
                    .catalog
                    .get_bucket("test-bucket", PERMISSIVE)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        storage
            .delete_bucket(
                Request::new(pb::DeleteBucketRequest {
                    name: get("test-bucket").name,
                    if_metageneration_match: Some(1),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap();
        assert_code(
            storage
                .get_bucket(Request::new(get("test-bucket")), None)
                .await,
            Code::NotFound,
        );
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_pagination_and_routing() {
        let (_dir, storage) = storage(false).await;
        for name in ["page-a", "page-b", "other-bucket"] {
            storage
                .create_bucket(Request::new(create(name)), None)
                .await
                .unwrap();
        }
        let mut query = pb::ListBucketsRequest {
            parent: "projects/p".into(),
            prefix: "page-".into(),
            page_size: 1,
            ..Default::default()
        };
        let first = storage
            .list_buckets(Request::new(query.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(first.buckets[0].bucket_id, "page-a");
        assert!(!first.next_page_token.is_empty());
        query.page_token = first.next_page_token;
        let second = storage
            .list_buckets(Request::new(query.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second.buckets[0].bucket_id, "page-b");
        assert!(second.next_page_token.is_empty());
        query.prefix = "other".into();
        assert_code(
            storage
                .list_buckets(Request::new(query.clone()), None)
                .await,
            Code::InvalidArgument,
        );
        query.page_token = "bad-token".into();
        assert_code(
            storage
                .list_buckets(Request::new(query.clone()), None)
                .await,
            Code::InvalidArgument,
        );
        query.page_token.clear();
        query.page_size = -1;
        assert_code(
            storage.list_buckets(Request::new(query), None).await,
            Code::InvalidArgument,
        );
        for name in [
            "",
            "test-bucket",
            "projects/p/buckets/test-bucket",
            "projects/_/buckets/a/b",
            "projects/_/buckets/UPPER",
        ] {
            assert_code(
                storage
                    .get_bucket(
                        Request::new(pb::GetBucketRequest {
                            name: name.into(),
                            ..Default::default()
                        }),
                        None,
                    )
                    .await,
                Code::InvalidArgument,
            );
        }
        let mut request = Request::new(get("page-a"));
        request.metadata_mut().insert(
            "x-goog-request-params",
            "bucket=projects%2F_%2Fbuckets%2Fpage-a".parse().unwrap(),
        );
        storage.get_bucket(request, None).await.unwrap();
        for header in [
            "bucket=projects%2F_%2Fbuckets%2Fpage-b",
            "bucket=%GG",
            "bucket",
            "unknown=value",
        ] {
            let mut request = Request::new(get("page-a"));
            request
                .metadata_mut()
                .insert("x-goog-request-params", header.parse().unwrap());
            let code = storage.get_bucket(request, None).await.unwrap_err().code();
            assert!(matches!(code, Code::InvalidArgument | Code::Unimplemented));
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_create_autoclass_shares_json_rules() {
        let (_dir, storage) = storage(false).await;
        let mut request = create("auto-bucket");
        request.bucket.as_mut().unwrap().autoclass = Some(pb::bucket::Autoclass {
            enabled: true,
            terminal_storage_class: Some("ARCHIVE".into()),
            ..Default::default()
        });
        let created = storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let config = created.autoclass.clone().unwrap();
        assert!(config.enabled);
        assert_eq!(config.terminal_storage_class.as_deref(), Some("ARCHIVE"));
        assert_eq!(config.toggle_time, created.create_time);
        assert_eq!(
            config.terminal_storage_class_update_time,
            created.create_time
        );
        let fetched = storage
            .get_bucket(Request::new(get("auto-bucket")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(fetched.autoclass, created.autoclass);
        let masked = storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    read_mask: fixtures::mask(&["autoclass"]),
                    ..get("auto-bucket")
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(masked.autoclass, created.autoclass);

        for (name, autoclass, class) in [
            (
                "bad-terminal",
                pb::bucket::Autoclass {
                    enabled: true,
                    terminal_storage_class: Some("COLDLINE".into()),
                    ..Default::default()
                },
                "",
            ),
            (
                "bad-class",
                pb::bucket::Autoclass {
                    enabled: true,
                    ..Default::default()
                },
                "NEARLINE",
            ),
        ] {
            let mut request = create(name);
            let bucket = request.bucket.as_mut().unwrap();
            bucket.autoclass = Some(autoclass);
            bucket.storage_class = class.into();
            assert_eq!(
                storage
                    .create_bucket(Request::new(request), None)
                    .await
                    .unwrap_err()
                    .code(),
                Code::InvalidArgument,
                "{name}"
            );
        }
        let mut request = create("output-only");
        request.bucket.as_mut().unwrap().autoclass = Some(pb::bucket::Autoclass {
            enabled: true,
            toggle_time: Some(prost_types::Timestamp::default()),
            ..Default::default()
        });
        assert_code(
            storage.create_bucket(Request::new(request), None).await,
            Code::Unimplemented,
        );
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_create_subset_and_defaults() {
        let (_dir, storage) = storage(false).await;
        let mut request = create("default-bucket");
        let bucket = request.bucket.as_mut().unwrap();
        bucket.autoclass = Some(Default::default());
        bucket.billing = Some(Default::default());
        bucket.custom_placement_config = Some(Default::default());
        bucket.hierarchical_namespace = Some(Default::default());
        bucket.object_retention = Some(Default::default());
        bucket.rpo = "DEFAULT".into();
        bucket.retention_policy = Some(pb::bucket::RetentionPolicy {
            retention_duration: Some(prost_types::Duration {
                seconds: 60,
                nanos: 0,
            }),
            ..Default::default()
        });
        bucket.soft_delete_policy = Some(pb::bucket::SoftDeletePolicy {
            retention_duration: Some(prost_types::Duration {
                seconds: 604800,
                nanos: 0,
            }),
            ..Default::default()
        });
        let result = storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            result
                .retention_policy
                .unwrap()
                .retention_duration
                .unwrap()
                .seconds,
            60
        );
        assert_eq!(
            result
                .soft_delete_policy
                .unwrap()
                .retention_duration
                .unwrap()
                .seconds,
            604800
        );
        let mut request = create("unsupported-bucket");
        request.bucket.as_mut().unwrap().billing = Some(pb::bucket::Billing {
            requester_pays: true,
        });
        assert_code(
            storage.create_bucket(Request::new(request), None).await,
            Code::Unimplemented,
        );
        assert!(
            storage
                .state
                .catalog
                .get_bucket("unsupported-bucket", PERMISSIVE)
                .await
                .unwrap()
                .is_none()
        );
        let mut request = create("bad-parent");
        request.parent = "projects/p".into();
        assert_code(
            storage.create_bucket(Request::new(request), None).await,
            Code::InvalidArgument,
        );
        let mut request = create("wildcard-bucket");
        request.bucket.as_mut().unwrap().project = "projects/_".into();
        storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap();
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_expose_json_lifecycle_etag_and_object_retention() {
        let (_dir, storage) = storage(false).await;
        let mut record = json::parse_insert(
            br#"{"name":"json-bucket","lifecycle":{"rule":[{"action":{"type":"Delete"},"condition":{"age":30,"matchesPrefix":["logs/"]}}]}}"#,
            "p",
            storage.state.clock.now(),
        )
        .unwrap();
        record.object_retention_enabled = true;
        let record = storage.state.catalog.insert_bucket(record).await.unwrap();
        let fetched = storage
            .get_bucket(Request::new(get("json-bucket")), None)
            .await
            .unwrap()
            .into_inner();
        let rules = fetched.lifecycle.clone().unwrap().rule;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action.as_ref().unwrap().r#type, "Delete");
        let condition = rules[0].condition.as_ref().unwrap();
        assert_eq!(condition.age_days, Some(30));
        assert_eq!(condition.matches_prefix, ["logs/"]);
        assert_eq!(fetched.etag, json::bucket_etag(&record));
        assert_eq!(
            fetched.object_retention,
            Some(pb::bucket::ObjectRetention { enabled: true })
        );
        let masked = storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    read_mask: fixtures::mask(&["lifecycle", "etag", "object_retention"]),
                    ..get("json-bucket")
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(masked.lifecycle, fetched.lifecycle);
        assert_eq!(masked.etag, fetched.etag);
        assert_eq!(masked.object_retention, fetched.object_retention);
        assert!(masked.name.is_empty() && masked.versioning.is_none());
        let listed = storage
            .list_buckets(
                Request::new(pb::ListBucketsRequest {
                    parent: "projects/p".into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.buckets[0].lifecycle, fetched.lifecycle);
        storage
            .create_bucket(Request::new(create("plain-bucket")), None)
            .await
            .unwrap();
        let plain = storage
            .get_bucket(Request::new(get("plain-bucket")), None)
            .await
            .unwrap()
            .into_inner();
        assert!(plain.lifecycle.is_none() && plain.object_retention.is_none());
        storage.state.catalog.close().await.unwrap();
    }

    fn update_acl(bucket: pb::Bucket, path: &str) -> pb::UpdateBucketRequest {
        pb::UpdateBucketRequest {
            bucket: Some(bucket),
            update_mask: fixtures::mask(&[path]),
            ..Default::default()
        }
    }

    fn reader(entity: &str) -> (pb::BucketAccessControl, pb::ObjectAccessControl) {
        (
            pb::BucketAccessControl {
                entity: entity.into(),
                role: "READER".into(),
                ..Default::default()
            },
            pb::ObjectAccessControl {
                entity: entity.into(),
                role: "READER".into(),
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn grpc_buckets_acl_read_and_update() {
        let (_dir, storage) = storage(false).await;
        storage
            .create_bucket(Request::new(create("acl-bucket")), None)
            .await
            .unwrap();
        let current = storage
            .get_bucket(Request::new(get("acl-bucket")), None)
            .await
            .unwrap()
            .into_inner();
        // A new bucket reports the projectPrivate defaults, like JSON projection=full.
        assert!(!current.acl.is_empty() && !current.default_object_acl.is_empty());
        assert!(current.acl.iter().all(|entry| !entry.etag.is_empty()));
        assert!(
            current
                .owner
                .as_ref()
                .unwrap()
                .entity
                .starts_with("project-owners-")
        );
        let listed = storage
            .list_buckets(
                Request::new(pb::ListBucketsRequest {
                    parent: "projects/p".into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(listed.buckets[0].acl.is_empty() && listed.buckets[0].owner.is_none());
        let listed = storage
            .list_buckets(
                Request::new(pb::ListBucketsRequest {
                    parent: "projects/p".into(),
                    read_mask: fixtures::mask(&["acl"]),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.buckets[0].acl, current.acl);
        assert!(listed.buckets[0].default_object_acl.is_empty());

        let (bucket_entry, object_entry) = reader("allAuthenticatedUsers");
        let mut acl = current.acl.clone();
        acl.push(bucket_entry);
        let updated = storage
            .update_bucket(
                Request::new(update_acl(
                    pb::Bucket {
                        name: get("acl-bucket").name,
                        acl,
                        ..Default::default()
                    },
                    "acl",
                )),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(
            updated
                .acl
                .iter()
                .any(|e| e.entity == "allAuthenticatedUsers" && e.role == "READER")
        );
        let mut default_object_acl = current.default_object_acl.clone();
        default_object_acl.push(object_entry);
        storage
            .update_bucket(
                Request::new(update_acl(
                    pb::Bucket {
                        name: get("acl-bucket").name,
                        default_object_acl,
                        ..Default::default()
                    },
                    "default_object_acl",
                )),
                None,
            )
            .await
            .unwrap();
        let fetched = storage
            .get_bucket(Request::new(get("acl-bucket")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(fetched.acl.len(), current.acl.len() + 1);
        assert!(
            fetched
                .default_object_acl
                .iter()
                .any(|e| e.entity == "allAuthenticatedUsers" && e.role == "READER")
        );
        // JSON observes the same stored ACL.
        let stored = storage
            .state
            .catalog
            .acl_projection("acl-bucket", Vec::new())
            .await
            .unwrap()
            .unwrap();
        assert!(
            stored
                .bucket
                .iter()
                .any(|e| e.entity == "allAuthenticatedUsers")
        );

        let (bad, _) = reader("not-an-entity");
        let error = storage
            .update_bucket(
                Request::new(update_acl(
                    pb::Bucket {
                        name: get("acl-bucket").name,
                        acl: vec![bad],
                        ..Default::default()
                    },
                    "acl",
                )),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);

        let mut request = create("uniform-bucket");
        request.bucket.as_mut().unwrap().iam_config = Some(pb::bucket::IamConfig {
            uniform_bucket_level_access: Some(pb::bucket::iam_config::UniformBucketLevelAccess {
                enabled: true,
                lock_time: None,
            }),
            ..Default::default()
        });
        storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap();
        let uniform = storage
            .get_bucket(Request::new(get("uniform-bucket")), None)
            .await
            .unwrap()
            .into_inner();
        assert!(uniform.acl.is_empty() && uniform.default_object_acl.is_empty());
        assert!(uniform.owner.is_some());
        let (entry, _) = reader("allUsers");
        let error = storage
            .update_bucket(
                Request::new(update_acl(
                    pb::Bucket {
                        name: get("uniform-bucket").name,
                        acl: vec![entry],
                        ..Default::default()
                    },
                    "acl",
                )),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_acl_strict_authorization() {
        let dir = TestDir::new();
        let auth = Auth::strict(registry(&[
            ("admin", "p", "roles/storage.admin"),
            ("viewer", "p", "roles/storage.bucketViewer"),
        ]));
        let storage = Storage {
            max_object_bytes: 1024,
            state: Arc::new(crate::server::State::for_test(
                Catalog::open(&dir.0).await.unwrap(),
                auth,
            )),
        };
        storage
            .create_bucket(
                Request::new(create("strict-acl")),
                caller(&storage, "admin"),
            )
            .await
            .unwrap();
        let admin = storage
            .get_bucket(Request::new(get("strict-acl")), caller(&storage, "admin"))
            .await
            .unwrap()
            .into_inner();
        assert!(!admin.acl.is_empty());
        // Reading the bucket succeeds without the ACL for a caller that cannot read ACLs.
        let viewer = storage
            .get_bucket(Request::new(get("strict-acl")), caller(&storage, "viewer"))
            .await
            .unwrap()
            .into_inner();
        assert!(viewer.acl.is_empty() && viewer.default_object_acl.is_empty());
        let (entry, _) = reader("allUsers");
        let request = update_acl(
            pb::Bucket {
                name: get("strict-acl").name,
                acl: vec![entry],
                ..Default::default()
            },
            "acl",
        );
        assert_code(
            storage
                .update_bucket(Request::new(request.clone()), caller(&storage, "viewer"))
                .await,
            Code::PermissionDenied,
        );
        assert_code(
            storage
                .update_bucket(Request::new(request.clone()), Some(Caller::Anonymous))
                .await,
            Code::Unauthenticated,
        );
        let unchanged = storage
            .get_bucket(Request::new(get("strict-acl")), caller(&storage, "admin"))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(unchanged.acl, admin.acl);
        storage
            .update_bucket(Request::new(request), caller(&storage, "admin"))
            .await
            .unwrap();
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_create_enables_object_retention() {
        let dir = TestDir::new();
        let auth = Auth::strict(registry(&[
            ("admin", "p", "roles/storage.admin"),
            ("editor", "p", "roles/storage.editor"),
        ]));
        let storage = Storage {
            max_object_bytes: 1024,
            state: Arc::new(crate::server::State::for_test(
                Catalog::open(&dir.0).await.unwrap(),
                auth,
            )),
        };
        let mut request = create("retention-bucket");
        request.enable_object_retention = true;
        // Creating a bucket without enabling object retention needs no extra permission.
        storage
            .create_bucket(
                Request::new(create("plain-bucket")),
                caller(&storage, "editor"),
            )
            .await
            .unwrap();
        assert_code(
            storage
                .create_bucket(Request::new(request.clone()), caller(&storage, "editor"))
                .await,
            Code::PermissionDenied,
        );
        assert!(
            storage
                .state
                .catalog
                .get_bucket("retention-bucket", PERMISSIVE)
                .await
                .unwrap()
                .is_none()
        );
        let created = storage
            .create_bucket(Request::new(request), caller(&storage, "admin"))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            created.object_retention,
            Some(pb::bucket::ObjectRetention { enabled: true })
        );
        let stored = storage
            .state
            .catalog
            .get_bucket("retention-bucket", PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.object_retention_enabled);
        // The bucket field alone cannot enable object retention, as in JSON.
        let mut request = create("field-only");
        request.bucket.as_mut().unwrap().object_retention =
            Some(pb::bucket::ObjectRetention { enabled: true });
        assert_code(
            storage
                .create_bucket(Request::new(request.clone()), caller(&storage, "admin"))
                .await,
            Code::InvalidArgument,
        );
        request.enable_object_retention = true;
        storage
            .create_bucket(Request::new(request), caller(&storage, "admin"))
            .await
            .unwrap();
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_create_accepts_name_matching_bucket_id() {
        let (_dir, storage) = storage(false).await;
        // The Java SDK repeats the full resource name of the new bucket in bucket.name.
        let mut request = create("named-bucket");
        request.bucket.as_mut().unwrap().name = "projects/_/buckets/named-bucket".into();
        let created = storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(created.name, "projects/_/buckets/named-bucket");
        assert_eq!(created.bucket_id, "named-bucket");
        for name in [
            "projects/_/buckets/other-bucket",
            "mismatch-bucket",
            "projects/p/buckets/mismatch-bucket",
            "projects/_/buckets/mismatch-bucket/x",
        ] {
            let mut request = create("mismatch-bucket");
            request.bucket.as_mut().unwrap().name = name.into();
            assert_eq!(
                storage
                    .create_bucket(Request::new(request), None)
                    .await
                    .unwrap_err()
                    .code(),
                Code::InvalidArgument,
                "{name}"
            );
        }
        for bucket in ["mismatch-bucket", "other-bucket"] {
            assert!(
                storage
                    .state
                    .catalog
                    .get_bucket(bucket, PERMISSIVE)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_strict_shared_authorization() {
        let (_dir, storage) = storage(true).await;
        for token in [None, Some("reader"), Some("admin")] {
            let mut metadata = Request::new(());
            if let Some(token) = token {
                metadata
                    .metadata_mut()
                    .insert("authorization", format!("Bearer {token}").parse().unwrap());
            }
            let caller = storage.authenticate(&metadata).unwrap();
            let result = storage
                .create_bucket(Request::new(create("strict-bucket")), caller)
                .await;
            match token {
                None => assert_eq!(result.unwrap_err().code(), Code::Unauthenticated),
                Some("reader") => assert_eq!(result.unwrap_err().code(), Code::PermissionDenied),
                _ => {
                    result.unwrap();
                }
            }
        }
        for token in [None, Some("reader"), Some("admin")] {
            let mut metadata = Request::new(());
            if let Some(token) = token {
                metadata
                    .metadata_mut()
                    .insert("authorization", format!("Bearer {token}").parse().unwrap());
            }
            let caller = storage.authenticate(&metadata).unwrap();
            let read = storage
                .get_bucket(Request::new(get("strict-bucket")), caller)
                .await;
            let list = storage
                .list_buckets(
                    Request::new(pb::ListBucketsRequest {
                        parent: "projects/p".into(),
                        ..Default::default()
                    }),
                    caller,
                )
                .await;
            let delete = storage
                .delete_bucket(
                    Request::new(pb::DeleteBucketRequest {
                        name: get("strict-bucket").name,
                        ..Default::default()
                    }),
                    caller,
                )
                .await;
            if token == Some("admin") {
                assert!(read.is_ok() && list.is_ok() && delete.is_ok());
            } else {
                let expected = if token.is_none() {
                    Code::Unauthenticated
                } else {
                    Code::PermissionDenied
                };
                assert_eq!(read.unwrap_err().code(), expected);
                assert_eq!(list.unwrap_err().code(), expected);
                assert_eq!(delete.unwrap_err().code(), expected);
            }
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_buckets_public_grants_and_billing_metadata() {
        let (_dir, storage) = storage(true).await;
        storage
            .state
            .catalog
            .insert_bucket(
                json::parse_insert(
                    br#"{"name":"public-bucket"}"#,
                    "p",
                    storage.state.clock.now(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        storage
            .state
            .catalog
            .set_policy(
                "public-bucket",
                crate::auth::Policy {
                    bindings: vec![crate::auth::Binding {
                        role: "roles/storage.admin".into(),
                        members: vec!["allUsers".into()],
                    }],
                    etag: None,
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        let caller = Some(Caller::Anonymous);
        storage
            .get_bucket(Request::new(get("public-bucket")), caller)
            .await
            .unwrap();
        for value in ["", "billing-project"] {
            let mut request = Request::new(get("public-bucket"));
            request
                .metadata_mut()
                .insert("x-goog-user-project", value.parse().unwrap());
            let error = storage.get_bucket(request, caller).await.unwrap_err();
            assert_eq!(error.code(), Code::Unimplemented);
            assert!(!error.details().is_empty());
        }
        storage
            .delete_bucket(
                Request::new(pb::DeleteBucketRequest {
                    name: get("public-bucket").name,
                    ..Default::default()
                }),
                caller,
            )
            .await
            .unwrap();
        assert_code(
            storage
                .get_bucket(Request::new(get("public-bucket")), caller)
                .await,
            Code::Unauthenticated,
        );
        assert_eq!(
            Status::from(json::JsonError::new(
                hyper::StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "catalog unavailable"
            ))
            .code(),
            Code::Unavailable
        );
        assert_eq!(
            Status::from(json::JsonError::unsupported("userProject")).code(),
            Code::Unimplemented
        );
        storage.state.catalog.close().await.unwrap();
    }
}
