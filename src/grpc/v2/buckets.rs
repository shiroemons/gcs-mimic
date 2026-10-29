use super::status::from_json as json_error;
use super::{Storage, pb, status::unsupported_for as unsupported};
use super::{convert, names};
use crate::auth::Caller;
use crate::{bucket::CatalogError, json};
use serde_json::json;
use tonic::{Request, Response, Status};

fn catalog_error(error: CatalogError, caller: Option<Caller<'_>>) -> Status {
    if matches!(error, CatalogError::NotEmpty) {
        return Status::failed_precondition(
            "Bucket is not empty; delete its objects and versions before deleting the bucket",
        );
    }
    json_error(crate::server::anonymous_error(error.into(), caller))
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
            Status::invalid_argument("Invalid project; use projects/local-project or projects/_")
        })?;
    json::validate_project(project).map_err(json_error)?;
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
            let key = json::decode(key, true).map_err(json_error)?;
            let value = json::decode(value, true).map_err(json_error)?;
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
            retention_policy,
            soft_delete_policy,
            iam_config
        );
    }
    Ok(selected)
}

fn create_metadata(
    bucket: &pb::Bucket,
    name: &str,
    project: &str,
    state: &crate::server::State,
) -> Result<crate::bucket::BucketRecord, Status> {
    let mut remaining = bucket.clone();
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
    body["cors"] = json!(bucket.cors.iter().map(|cors| json!({"origin": cors.origin, "method": cors.method, "responseHeader": cors.response_header, "maxAgeSeconds": cors.max_age_seconds})).collect::<Vec<_>>());
    if let Some(versioning) = &bucket.versioning {
        body["versioning"] = json!({"enabled": versioning.enabled});
    }
    for (field, policy) in [
        (
            "retentionPolicy",
            bucket.retention_policy.as_ref().map(|p| {
                (
                    p.retention_duration.as_ref(),
                    p.effective_time.is_some() || p.is_locked,
                )
            }),
        ),
        (
            "softDeletePolicy",
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
                let key = if field == "retentionPolicy" {
                    "retentionPeriod"
                } else {
                    "retentionDurationSeconds"
                };
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
    if let Some(autoclass) = remaining.autoclass.take() {
        if autoclass.toggle_time.is_some() || autoclass.terminal_storage_class_update_time.is_some()
        {
            return Err(super::status::unsupported(
                "create bucket autoclass output fields",
            ));
        }
        body["autoclass"] = json!({"enabled": autoclass.enabled});
        if let Some(class) = autoclass.terminal_storage_class {
            body["autoclass"]["terminalStorageClass"] = class.into();
        }
    }
    if remaining
        .billing
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.billing = None;
    }
    if remaining
        .hierarchical_namespace
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.hierarchical_namespace = None;
    }
    if remaining
        .custom_placement_config
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.custom_placement_config = None;
    }
    if remaining
        .object_retention
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.object_retention = None;
    }
    if remaining
        .lifecycle
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.lifecycle = None;
    }
    if remaining
        .encryption
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.encryption = None;
    }
    if remaining
        .logging
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.logging = None;
    }
    if remaining
        .ip_filter
        .as_ref()
        .is_some_and(|v| v == &Default::default())
    {
        remaining.ip_filter = None;
    }
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
    .map_err(json_error)
}

impl Storage {
    pub(super) async fn delete_bucket(
        &self,
        request: Request<pb::DeleteBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        routing(&request, "bucket", &request.get_ref().name)?;
        let request = request.into_inner();
        let name = names::bucket_name(&request.name)?;
        json::validate_bucket_name(name).map_err(json_error)?;
        let access =
            crate::server::authorize_bucket(&self.state, caller, "storage.buckets.delete", name)
                .await
                .map_err(|error| json_error(crate::server::anonymous_error(error, caller)))?;
        self.state
            .catalog
            .delete_bucket_authorized(
                name,
                request.if_metageneration_match,
                request.if_metageneration_not_match,
                &access,
            )
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
        json::validate_bucket_name(name).map_err(json_error)?;
        bucket_mask(pb::Bucket::default(), request.read_mask.as_ref())?;
        let access =
            crate::server::authorize_bucket(&self.state, caller, "storage.buckets.get", name)
                .await
                .map_err(|error| json_error(crate::server::anonymous_error(error, caller)))?;
        let record = self
            .state
            .catalog
            .get_bucket_authorized(name, &access)
            .await
            .map_err(|error| catalog_error(error, caller))?
            .ok_or_else(|| catalog_error(CatalogError::NotFound, caller))?;
        if request
            .if_metageneration_match
            .is_some_and(|v| v != record.metageneration)
            || request.if_metageneration_not_match == Some(record.metageneration)
        {
            return Err(catalog_error(CatalogError::PreconditionFailed, caller));
        }
        Ok(Response::new(bucket_mask(
            convert::bucket(&record),
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
        if !request.predefined_acl.is_empty()
            || !request.predefined_default_object_acl.is_empty()
            || request.enable_object_retention
        {
            return Err(unsupported("CreateBucket ACL or object retention", caller));
        }
        let bucket = request.bucket.unwrap_or_default();
        if !bucket.name.is_empty() {
            return Err(Status::invalid_argument(
                "CreateBucket.bucket.name must be empty; use bucket_id=example-bucket",
            ));
        }
        let project = project_name(&bucket.project)?;
        if let Some(caller) = caller {
            caller
                .authorize("storage.buckets.create", Some(project))
                .map_err(|e| json_error(e.into()))?;
        }
        let record = create_metadata(&bucket, &request.bucket_id, project, &self.state)?;
        let record = self
            .state
            .catalog
            .insert_bucket(record)
            .await
            .map_err(|error| catalog_error(error, caller))?;
        Ok(Response::new(convert::bucket(&record)))
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
            caller
                .authorize("storage.buckets.list", Some(project))
                .map_err(|e| json_error(e.into()))?;
        }
        if request.return_partial_success {
            return Err(unsupported("ListBuckets return_partial_success", caller));
        }
        if request.page_size < 0 || request.prefix.len() > 222 {
            return Err(Status::invalid_argument(
                "Use a nonnegative page_size and a prefix of at most 222 bytes, for example page_size=100",
            ));
        }
        bucket_mask(pb::Bucket::default(), request.read_mask.as_ref())?;
        let start = if request.page_token.is_empty() {
            None
        } else {
            Some(
                json::decode_page_token(&request.page_token, project, &request.prefix)
                    .map_err(json_error)?,
            )
        };
        let limit = if request.page_size == 0 {
            1000
        } else {
            request.page_size.min(1000) as usize
        };
        let page = self
            .state
            .catalog
            .list_buckets_page(project, Some(&request.prefix), start.as_deref(), limit)
            .await
            .map_err(|error| catalog_error(error, caller))?;
        let next_page_token = if page.has_more {
            json::encode_page_token(
                project,
                &request.prefix,
                &page.items.last().expect("nonempty continuation page").name,
            )
            .map_err(json_error)?
        } else {
            String::new()
        };
        let buckets = page
            .items
            .iter()
            .map(|record| bucket_mask(convert::bucket(record), request.read_mask.as_ref()))
            .collect::<Result<_, _>>()?;
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
    use crate::{
        auth::{Auth, Registry},
        clock::SystemClock,
        server::State,
        storage::Catalog,
        test_support::TestDir,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    use tonic::Code;

    async fn storage(strict: bool) -> (TestDir, Storage) {
        let dir = TestDir::new();
        let auth = if strict {
            Auth::strict(Registry::parse(br#"{"identities":[{"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"p":["roles/storage.admin"]}},{"id":"reader@example.invalid","tokens":["reader"]}]}"#).unwrap())
        } else {
            Auth::permissive()
        };
        let catalog = Catalog::open(&dir.0).await.unwrap();
        (
            dir,
            Storage {
                max_object_bytes: 1024 * 1024 * 1024,
                state: Arc::new(State {
                    ready: AtomicBool::new(true),
                    clock: Arc::new(SystemClock),
                    catalog,
                    api_base_url: "http://localhost:4443".into(),
                    default_location: "US".into(),
                    auth,
                }),
            },
        )
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
        assert_eq!(
            storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap_err()
                .code(),
            Code::AlreadyExists
        );
        assert_eq!(
            storage
                .get_bucket(Request::new(get("missing-bucket")), None)
                .await
                .unwrap_err()
                .code(),
            Code::NotFound
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
                assert_eq!(result.unwrap_err().code(), Code::FailedPrecondition);
            }
        }
        let mut request = get("test-bucket");
        request.read_mask = Some(prost_types::FieldMask {
            paths: vec!["name".into(), "location".into()],
        });
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
            assert_eq!(
                storage
                    .delete_bucket(Request::new(request), None)
                    .await
                    .unwrap_err()
                    .code(),
                Code::FailedPrecondition
            );
            assert!(
                storage
                    .state
                    .catalog
                    .get_bucket("test-bucket")
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
        assert_eq!(
            storage
                .get_bucket(Request::new(get("test-bucket")), None)
                .await
                .unwrap_err()
                .code(),
            Code::NotFound
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
        assert_eq!(
            storage
                .list_buckets(Request::new(query.clone()), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        query.page_token = "bad-token".into();
        assert_eq!(
            storage
                .list_buckets(Request::new(query.clone()), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        query.page_token.clear();
        query.page_size = -1;
        assert_eq!(
            storage
                .list_buckets(Request::new(query), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        for name in [
            "",
            "test-bucket",
            "projects/p/buckets/test-bucket",
            "projects/_/buckets/a/b",
            "projects/_/buckets/UPPER",
        ] {
            assert_eq!(
                storage
                    .get_bucket(
                        Request::new(pb::GetBucketRequest {
                            name: name.into(),
                            ..Default::default()
                        }),
                        None
                    )
                    .await
                    .unwrap_err()
                    .code(),
                Code::InvalidArgument
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
                    read_mask: Some(prost_types::FieldMask {
                        paths: vec!["autoclass".into()],
                    }),
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
        assert_eq!(
            storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap_err()
                .code(),
            Code::Unimplemented
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
        assert_eq!(
            storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap_err()
                .code(),
            Code::Unimplemented
        );
        assert!(
            storage
                .state
                .catalog
                .get_bucket("unsupported-bucket")
                .await
                .unwrap()
                .is_none()
        );
        let mut request = create("bad-parent");
        request.parent = "projects/p".into();
        assert_eq!(
            storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
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
                &crate::bucket::BucketAccess::Permissive,
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
        assert_eq!(
            storage
                .get_bucket(Request::new(get("public-bucket")), caller)
                .await
                .unwrap_err()
                .code(),
            Code::Unauthenticated
        );
        assert_eq!(
            json_error(json::JsonError::new(
                hyper::StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "catalog unavailable"
            ))
            .code(),
            Code::Unavailable
        );
        assert_eq!(
            json_error(json::JsonError::unsupported("userProject")).code(),
            Code::Unimplemented
        );
        storage.state.catalog.close().await.unwrap();
    }
}
