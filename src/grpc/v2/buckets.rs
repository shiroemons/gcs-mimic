use super::{Storage, access_status, pb, status::unsupported_for};
use super::{
    convert,
    mask::{AclDefault, BucketMask},
    names,
};
use crate::auth::Caller;
use crate::bucket::{BucketConditions, CatalogError, name::MAX_BUCKET_NAME_BYTES, resource};
use serde_json::{Map, Value, json};
use tonic::{Request, Response, Status};

fn require_caller(caller: Option<Caller<'_>>) -> Result<(), Status> {
    if matches!(caller, Some(Caller::Anonymous)) {
        return Err(super::auth_error(
            crate::auth::AuthFailure::LoginRequired.into(),
        ));
    }
    Ok(())
}

fn project_name(resource: &str) -> Result<&str, Status> {
    let project = names::project_id(resource)?;
    crate::auth::validate_project_id(project)?;
    // Project numbers are positive and a derived one is never zero, so `projects/0` names no
    // project (recorded 2026-11-17). Earlier releases rendered every project ID as `projects/0`;
    // taking that value back would silently address a different project called "0".
    if crate::project::ProjectNumber::literal(project) == Some(crate::project::ProjectNumber::ZERO)
    {
        return Err(super::status::invalid_field(
            "Project id: 0 is invalid or not found",
        ));
    }
    Ok(project)
}

/// A message field a client sent empty carries no setting.
fn is_set<T: Default + PartialEq>(field: Option<&T>) -> bool {
    field.is_some_and(|value| value != &T::default())
}

/// A requested retention policy, or `None` when it sets no period.
fn retention_body(policy: &pb::bucket::RetentionPolicy) -> Result<Option<Value>, Status> {
    if policy.effective_time.is_some() || policy.is_locked {
        return Err(super::status::unsupported(
            "create bucket policy output fields",
        ));
    }
    let Some(duration) = &policy.retention_duration else {
        return Ok(None);
    };
    let seconds = super::bucket_update::retention_seconds(duration);
    crate::bucket::RetentionPeriod::try_from(seconds)?;
    Ok(Some(json!({"retentionPeriod": seconds.to_string()})))
}

/// A requested soft-delete policy, or `None` when it sets no duration.
fn soft_delete_body(policy: &pb::bucket::SoftDeletePolicy) -> Result<Option<Value>, Status> {
    if policy.effective_time.is_some() {
        return Err(super::status::unsupported(
            "create bucket policy output fields",
        ));
    }
    let Some(duration) = &policy.retention_duration else {
        return Ok(None);
    };
    let seconds = super::bucket_update::soft_delete_seconds(duration)?;
    crate::bucket::SoftDeleteRetention::try_from(seconds)?;
    Ok(Some(
        json!({"retentionDurationSeconds": seconds.to_string()}),
    ))
}

/// The requested IAM configuration, or `None` when it sets nothing.
fn iam_body(iam: pb::bucket::IamConfig) -> Result<Option<Value>, Status> {
    let mut config = Map::new();
    if !iam.public_access_prevention.is_empty() {
        iam.public_access_prevention
            .parse::<crate::bucket::PublicAccessPrevention>()?;
        config.insert(
            "publicAccessPrevention".into(),
            Value::from(iam.public_access_prevention),
        );
    }
    if let Some(uniform) = &iam.uniform_bucket_level_access {
        if uniform.lock_time.is_some() {
            return Err(super::status::unsupported("create bucket IAM lock_time"));
        }
        config.insert(
            "uniformBucketLevelAccess".into(),
            json!({"enabled": uniform.enabled}),
        );
    }
    Ok((!config.is_empty()).then_some(Value::Object(config)))
}

fn autoclass_body(autoclass: pb::bucket::Autoclass) -> Result<Value, Status> {
    let pb::bucket::Autoclass {
        enabled,
        terminal_storage_class,
        // Output only, so ignored as input (see `create_body`).
        toggle_time: _,
        terminal_storage_class_update_time: _,
    } = autoclass;
    let mut config = Map::from_iter([("enabled".into(), Value::from(enabled))]);
    if let Some(class) = terminal_storage_class {
        class.parse::<crate::bucket::autoclass::TerminalStorageClass>()?;
        config.insert("terminalStorageClass".into(), Value::from(class));
    }
    Ok(Value::Object(config))
}

/// The CreateBucket settings as the JSON insert object every protocol's bucket creation
/// validates ([`resource::parse_insert`]). Destructuring the whole message makes a new proto
/// field a compile error, so each field is read here, ignored because the proto marks it
/// output only, or required to be empty; a setting is never silently dropped. Only settings the
/// request carries are added, never update-mask reset defaults.
pub(super) fn create_body(bucket: pb::Bucket, name: &str) -> Result<Map<String, Value>, Status> {
    let pb::Bucket {
        // `create_bucket` compared `name` with the bucket ID and cleared it, read `project`,
        // and took `object_retention` before this call.
        name: _,
        project: _,
        object_retention: _,
        // OUTPUT_ONLY: "including the field in a request does nothing (the server *must* ignore
        // it and *must not* throw an error as a result of the field's presence)"
        // (field_behavior.proto). The server sets each of them on the new bucket.
        bucket_id: _,
        metageneration: _,
        location_type: _,
        create_time: _,
        update_time: _,
        owner: _,
        location,
        storage_class,
        labels,
        cors,
        versioning,
        default_event_based_hold,
        retention_policy,
        soft_delete_policy,
        iam_config,
        autoclass,
        // Accepted only when empty.
        etag,
        rpo,
        acl,
        default_object_acl,
        lifecycle,
        website,
        logging,
        encryption,
        billing,
        satisfies_pzs,
        custom_placement_config,
        hierarchical_namespace,
        ip_filter,
    } = bucket;
    let mut body = Map::from_iter([
        ("name".into(), Value::from(name)),
        (
            "defaultEventBasedHold".into(),
            Value::from(default_event_based_hold),
        ),
    ]);
    for (field, value) in [("location", location), ("storageClass", storage_class)] {
        if !value.is_empty() {
            body.insert(field.into(), Value::from(value));
        }
    }
    body.insert(
        "labels".into(),
        Value::Object(
            labels
                .into_iter()
                .map(|(key, value)| (key, Value::from(value)))
                .collect(),
        ),
    );
    body.insert(
        "cors".into(),
        crate::bucket::cors_value(&super::bucket_update::cors_rules(&cors)?),
    );
    if let Some(versioning) = versioning {
        body.insert("versioning".into(), json!({"enabled": versioning.enabled}));
    }
    if let Some(policy) = retention_policy
        && let Some(value) = retention_body(&policy)?
    {
        body.insert("retentionPolicy".into(), value);
    }
    if let Some(policy) = soft_delete_policy
        && let Some(value) = soft_delete_body(&policy)?
    {
        body.insert("softDeletePolicy".into(), value);
    }
    if let Some(iam) = iam_config
        && let Some(value) = iam_body(iam)?
    {
        body.insert("iamConfiguration".into(), value);
    }
    if let Some(autoclass) = autoclass {
        body.insert("autoclass".into(), autoclass_body(autoclass)?);
    }
    if !etag.is_empty()
        || !(rpo.is_empty() || rpo == "DEFAULT")
        || !acl.is_empty()
        || !default_object_acl.is_empty()
        || website.is_some()
        || satisfies_pzs
        || is_set(lifecycle.as_ref())
        || is_set(logging.as_ref())
        || is_set(encryption.as_ref())
        || is_set(billing.as_ref())
        || is_set(custom_placement_config.as_ref())
        || is_set(hierarchical_namespace.as_ref())
        || is_set(ip_filter.as_ref())
    {
        return Err(super::status::unsupported(
            "CreateBucket fields outside the supported metadata subset",
        ));
    }
    Ok(body)
}

impl Storage {
    /// The ID of the project a `projects/{project}` resource names by ID or by number.
    async fn project_id(
        &self,
        resource: &str,
        caller: Option<Caller<'_>>,
    ) -> Result<String, Status> {
        self.state
            .project_id(project_name(resource)?)
            .await
            .map_err(|error| access_status(error, caller))
    }

    /// Adds `owner`, `acl` and `default_object_acl` as JSON `projection=full` does. All three
    /// are omitted under uniform bucket-level access and for callers that may not read the ACL;
    /// the read itself still succeeds.
    pub(super) async fn add_bucket_acl(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &mut pb::Bucket,
    ) -> Result<(), Status> {
        use crate::acl::AclKind;
        let name = bucket.bucket_id.as_str();
        let readable =
            match crate::server::may_read_acl(&self.state, caller, AclKind::Bucket, name).await {
                Ok(readable) => readable,
                Err(error) if error.status == hyper::StatusCode::UNAUTHORIZED => false,
                Err(error) => return Err(Status::from(error)),
            };
        if !readable {
            return Ok(());
        }
        let Some(acl) = self.state.catalog.acl_projection(name, Vec::new()).await? else {
            return Ok(());
        };
        Self::apply_bucket_acl(bucket, &acl);
        Ok(())
    }

    fn apply_bucket_acl(bucket: &mut pb::Bucket, acl: &crate::storage::AclProjection) {
        use crate::acl::AclOwner;
        // Cloud Storage answers a uniform-access bucket without its owner as well as its ACLs
        // (recorded 2026-11-13), as JSON does.
        if acl.uniform_access {
            return;
        }
        let name = &bucket.bucket_id;
        bucket.owner = Some(pb::Owner {
            entity: crate::acl::Entity::project_team(crate::acl::ProjectTeam::Owners, &acl.project)
                .into_string(),
            entity_id: String::new(),
        });
        let bucket_owner = AclOwner::Bucket {
            bucket: name,
            metageneration: bucket.metageneration,
        };
        let default_owner = AclOwner::DefaultObject {
            metageneration: bucket.metageneration,
        };
        bucket.acl = acl
            .bucket
            .iter()
            .map(|entry| convert::bucket_access_control(bucket_owner, &acl.project, entry))
            .collect();
        bucket.default_object_acl = acl
            .default_object
            .iter()
            .map(|entry| convert::object_access_control(default_owner, &acl.project, entry))
            .collect();
    }

    pub(super) async fn delete_bucket(
        &self,
        request: Request<pb::DeleteBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();
        let name = self.bucket_id(&request.name)?;
        let access = self
            .access(caller, name, crate::auth::BUCKETS_DELETE)
            .await?;
        let conditions = BucketConditions::from(&request);
        self.state
            .catalog
            .delete_bucket(name, conditions, &access)
            .await
            .map_err(|error| access_status(error, caller))?;
        Ok(Response::new(()))
    }
    pub(super) async fn get_bucket(
        &self,
        request: Request<pb::GetBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let request = request.into_inner();
        let name = self.bucket_id(&request.name)?;
        let mask = BucketMask::parse(request.read_mask.as_ref())?;
        let access = self.access(caller, name, crate::auth::BUCKETS_GET).await?;
        let record = self
            .state
            .catalog
            .get_bucket(name, &access)
            .await
            .map_err(|error| access_status(error, caller))?
            .ok_or_else(|| access_status(CatalogError::NotFound, caller))?;
        BucketConditions::from(&request)
            .check(record.metageneration)
            .map_err(|error| access_status(error, caller))?;
        let mut bucket = convert::bucket(&record, mask.lifecycle())?;
        if mask.includes_acl(AclDefault::Include) {
            self.add_bucket_acl(caller, &mut bucket).await?;
        }
        Ok(Response::new(mask.apply(bucket)))
    }
    pub(super) async fn create_bucket(
        &self,
        request: Request<pb::CreateBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        require_caller(caller)?;
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
        let project = self
            .project_id(&std::mem::take(&mut bucket.project), caller)
            .await?;
        let project = project.as_str();
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
        let body = create_body(bucket, &request.bucket_id)?;
        let mut record = resource::parse_insert(
            &body,
            project,
            self.state.clock.now(),
            self.compat_profile,
            &self.state.default_location,
        )?;
        record.object_retention_enabled = request.enable_object_retention;
        let record = self
            .state
            .catalog
            .insert_bucket(record)
            .await
            .map_err(|error| access_status(error, caller))?;
        let mut bucket = convert::bucket(&record, convert::Lifecycle::Include)?;
        self.add_bucket_acl(caller, &mut bucket).await?;
        Ok(Response::new(bucket))
    }
    pub(super) async fn list_buckets(
        &self,
        request: Request<pb::ListBucketsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListBucketsResponse>, Status> {
        require_caller(caller)?;
        let request = request.into_inner();
        let project = self.project_id(&request.parent, caller).await?;
        let project = project.as_str();
        if let Some(caller) = caller {
            caller.authorize(crate::auth::BUCKETS_LIST, Some(project))?;
        }
        // `return_partial_success` only changes how unreachable locations are reported. Every
        // local bucket is reachable, so the listing is complete with or without it, as Cloud
        // Storage's was (case grpc-unsupported-options).
        let invalid_list = || {
            Status::invalid_argument(
                "Use a nonnegative page_size and a prefix of at most 222 bytes, for example page_size=100",
            )
        };
        // Cloud Storage's answer to a negative page size (recorded 2026-11-18).
        let limit = crate::grpc::validate::page_size(request.page_size).ok_or_else(|| {
            super::status::invalid_with_reason(
                "The requested page_size must be positive.",
                "GRPC_PAGE_SIZE_MUST_BE_POSITIVE",
            )
        })?;
        if request.prefix.len() > MAX_BUCKET_NAME_BYTES {
            return Err(invalid_list());
        }
        let mask = BucketMask::parse(request.read_mask.as_ref())?;
        let start = if request.page_token.is_empty() {
            None
        } else {
            Some(crate::page_token::decode_page_token(
                &request.page_token,
                project,
                &request.prefix,
            )?)
        };
        let page = self
            .state
            .catalog
            .list_buckets_page(project, Some(&request.prefix), start.as_deref(), limit)
            .await
            .map_err(|error| access_status(error, caller))?;
        // A continuation page is never empty, so its last bucket always names the next start.
        let next_page_token = match page.items.last() {
            Some(last) if page.has_more => {
                crate::page_token::encode_page_token(project, &request.prefix, &last.name)?
            }
            _ => String::new(),
        };
        let acl = mask.includes_acl(AclDefault::Exclude);
        let mut projections = if acl {
            let access = caller.map_or(crate::bucket::BucketAccess::Permissive, |caller| {
                crate::bucket::BucketAccess::Policy {
                    identity: match caller {
                        Caller::Principal(identity) => Some(identity.clone()),
                        Caller::Anonymous => None,
                    },
                    permission: crate::auth::Permission::BucketsGetIam,
                    project: Some(project.to_owned()),
                }
            });
            self.state
                .catalog
                .bucket_acl_projections(
                    page.items
                        .iter()
                        .map(|record| record.name.clone())
                        .collect(),
                    &access,
                )
                .await?
        } else {
            Vec::new()
        }
        .into_iter();
        let mut buckets = Vec::with_capacity(page.items.len());
        for record in &page.items {
            let mut bucket = convert::bucket(record, mask.lifecycle())?;
            if let Some(Some(projection)) = projections.next() {
                Self::apply_bucket_acl(&mut bucket, &projection);
            }
            buckets.push(mask.apply(bucket));
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
    use crate::grpc::test_support::{
        self as fixtures, assert_code, assert_unsupported, caller, registry,
    };
    use crate::storage::test_fixtures::PERMISSIVE;
    use crate::{auth::Auth, json, storage::Catalog, test_support::TestDir};
    use std::sync::Arc;
    use tonic::Code;

    #[test]
    fn bucket_errors_request_credentials_from_anonymous_callers() {
        let anonymous = access_status(CatalogError::Forbidden, Some(Caller::Anonymous));
        let generic = Status::from(CatalogError::Forbidden);
        assert_eq!(anonymous.code(), Code::Unauthenticated);
        assert_eq!(generic.code(), Code::PermissionDenied);
    }
    #[test]
    fn project_name_takes_back_rendered_numbers_and_rejects_zero() {
        // A numeric ID renders as itself, so the client can pass it back unchanged.
        assert_eq!(project_name("projects/123").unwrap(), "123");
        assert_eq!(
            project_name("projects/local-project").unwrap(),
            "local-project"
        );
        // Any spelling of number 0 names no project.
        for zero in [
            "projects/0",
            "projects/00",
            "projects/000000000000000000000000",
        ] {
            let error = project_name(zero).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument, "{zero}");
            assert_eq!(error.message(), "Project id: 0 is invalid or not found");
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
                compat_profile: crate::config::CompatProfile::Native,
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
        // A fractional retention_policy duration is rounded down instead
        // (create_bucket_rounds_retention_nanos_down), so only the soft delete one is refused,
        // and the output-only Autoclass times are ignored (create_bucket_ignores_output_only_fields).
        // The policy times, a lock and a uniform access lock_time are not output only.
        let cases: [InvalidCase; 5] = [
            ("create bucket IAM lock_time", |b| {
                b.iam_config
                    .get_or_insert_default()
                    .uniform_bucket_level_access
                    .get_or_insert_default()
                    .lock_time = Some(Default::default())
            }),
            ("Invalid argument.", |b| {
                b.soft_delete_policy
                    .get_or_insert_default()
                    .retention_duration = Some(FRACTIONAL)
            }),
            ("create bucket policy output fields", |b| {
                b.soft_delete_policy.get_or_insert_default().effective_time =
                    Some(Default::default())
            }),
            ("create bucket policy output fields", |b| {
                let policy = b.retention_policy.get_or_insert_default();
                policy.retention_duration = Some(FRACTIONAL);
                policy.is_locked = true;
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
            let status = create_body(bucket.clone(), "test-bucket").unwrap_err();
            // Only the fractional soft delete duration has Cloud Storage's recorded answer.
            let code = if reason == "Invalid argument." {
                tonic::Code::InvalidArgument
            } else {
                tonic::Code::Unimplemented
            };
            assert_eq!(status.code(), code, "{reason}");
            assert!(status.message().contains(reason), "{reason}: {status}");
        }
        // Creation and update refuse a fractional soft delete duration alike.
        let fractional = pb::Bucket {
            soft_delete_policy: Some(pb::bucket::SoftDeletePolicy {
                retention_duration: Some(FRACTIONAL),
                ..Default::default()
            }),
            ..Default::default()
        };
        let created = create_body(fractional.clone(), "test-bucket").unwrap_err();
        let updated =
            super::super::bucket_update::from_proto(&fractional, &["soft_delete_policy".into()])
                .unwrap_err();
        assert_eq!(created.code(), updated.code());
        assert_eq!(created.message(), updated.message());
    }

    /// Fields the proto marks OUTPUT_ONLY are ignored as input: "the server *must* ignore it and
    /// *must not* throw an error as a result of the field's presence" (field_behavior.proto).
    #[tokio::test]
    async fn create_bucket_ignores_output_only_fields() {
        let (_dir, storage) = storage(false).await;
        let past = prost_types::Timestamp {
            seconds: 1,
            nanos: 0,
        };
        let mut request = create("output-only");
        let bucket = request.bucket.as_mut().unwrap();
        bucket.bucket_id = "other-bucket".into();
        bucket.metageneration = 7;
        bucket.location_type = "dual-region".into();
        bucket.create_time = Some(past);
        bucket.update_time = Some(past);
        bucket.owner = Some(pb::Owner {
            entity: "user-someone@example.com".into(),
            entity_id: "id".into(),
        });
        bucket.autoclass = Some(pb::bucket::Autoclass {
            enabled: true,
            toggle_time: Some(past),
            terminal_storage_class_update_time: Some(past),
            ..Default::default()
        });
        let created = storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let plain = {
            let mut request = create("plain-bucket");
            request.bucket.as_mut().unwrap().autoclass = Some(pb::bucket::Autoclass {
                enabled: true,
                ..Default::default()
            });
            storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap()
                .into_inner()
        };
        assert_eq!(created.name, "projects/_/buckets/output-only");
        assert_eq!(created.bucket_id, "output-only");
        assert_eq!(created.metageneration, 1);
        assert_eq!(created.location_type, plain.location_type);
        assert_eq!(created.owner, plain.owner);
        assert_ne!(created.create_time, Some(past));
        assert_ne!(created.update_time, Some(past));
        let autoclass = created.autoclass.unwrap();
        assert!(autoclass.enabled);
        assert_eq!(autoclass.toggle_time, created.create_time);
        assert_ne!(autoclass.terminal_storage_class_update_time, Some(past));
        assert!(
            storage
                .state
                .catalog
                .get_bucket("other-bucket", PERMISSIVE)
                .await
                .unwrap()
                .is_none()
        );
        storage.state.catalog.close().await.unwrap();
    }

    /// The proto rounds any `nanos` of a retention duration down to the nearest second
    /// (storage.proto lines 2429-2433), on creation as on update, so a period under one second
    /// is no period at all.
    #[tokio::test]
    async fn create_bucket_rounds_retention_nanos_down() {
        let (_dir, storage) = storage(false).await;
        let retention = |name: &str, seconds, nanos| {
            let mut request = create(name);
            request.bucket.as_mut().unwrap().retention_policy = Some(pb::bucket::RetentionPolicy {
                retention_duration: Some(prost_types::Duration { seconds, nanos }),
                ..Default::default()
            });
            request
        };
        let created = storage
            .create_bucket(
                Request::new(retention("fractional-retention", 3600, 999_999_999)),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            created.retention_policy.unwrap().retention_duration,
            Some(prost_types::Duration {
                seconds: 3600,
                nanos: 0
            })
        );
        let stored = storage
            .state
            .catalog
            .get_bucket("fractional-retention", PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.retention_policy.unwrap().retention_period, 3600);
        let status = assert_code(
            storage
                .create_bucket(
                    Request::new(retention("sub-second-retention", 0, 500_000_000)),
                    None,
                )
                .await,
            Code::InvalidArgument,
        );
        assert!(status.message().contains("retention"), "{status}");
        assert_code(
            storage
                .get_bucket(Request::new(get("sub-second-retention")), None)
                .await,
            Code::NotFound,
        );
        // Durations outside the canonical form, whose nanos have the other sign than the seconds
        // or reach a whole second, round down by their value: the stored period, or `None` for
        // INVALID_ARGUMENT.
        for (name, seconds, nanos, period) in [
            ("negative-nanos-hour", 3600, -1, Some(3599)),
            ("negative-nanos-second", 1, -1, None),
            ("excess-nanos", 0, 1_500_000_000, Some(1)),
        ] {
            let result = storage
                .create_bucket(Request::new(retention(name, seconds, nanos)), None)
                .await;
            let stored = storage
                .state
                .catalog
                .get_bucket(name, PERMISSIVE)
                .await
                .unwrap();
            let Some(period) = period else {
                assert_code(result, Code::InvalidArgument);
                assert!(stored.is_none(), "{name}");
                continue;
            };
            assert_eq!(
                result
                    .unwrap()
                    .into_inner()
                    .retention_policy
                    .unwrap()
                    .retention_duration,
                Some(prost_types::Duration {
                    seconds: period,
                    nanos: 0
                }),
                "{name}"
            );
            assert_eq!(
                stored.unwrap().retention_policy.unwrap().retention_period,
                u64::try_from(period).unwrap()
            );
        }
        storage.state.catalog.close().await.unwrap();
    }

    /// Every `Bucket` field CreateBucket does not read must be empty: a setting the request
    /// cannot apply is rejected rather than silently ignored.
    #[tokio::test]
    async fn create_bucket_rejects_each_field_outside_the_supported_subset() {
        let (_dir, storage) = storage(false).await;
        type FieldCase = (&'static str, fn(&mut pb::Bucket));
        // The output-only fields are ignored instead (create_bucket_ignores_output_only_fields).
        // Unlike the other unmodeled settings, even an empty website message is refused.
        let cases: [FieldCase; 14] = [
            ("website (empty)", |b| b.website = Some(Default::default())),
            ("etag", |b| b.etag = "etag".into()),
            ("rpo", |b| b.rpo = "ASYNC_TURBO".into()),
            ("acl", |b| b.acl = vec![Default::default()]),
            ("default_object_acl", |b| {
                b.default_object_acl = vec![Default::default()]
            }),
            ("lifecycle", |b| {
                b.lifecycle = Some(pb::bucket::Lifecycle {
                    rule: vec![Default::default()],
                })
            }),
            ("website", |b| {
                b.website = Some(pb::bucket::Website {
                    main_page_suffix: "index.html".into(),
                    ..Default::default()
                })
            }),
            ("logging", |b| {
                b.logging = Some(pb::bucket::Logging {
                    log_bucket: "logs".into(),
                    ..Default::default()
                })
            }),
            ("encryption", |b| {
                b.encryption = Some(pb::bucket::Encryption {
                    default_kms_key: "key".into(),
                    ..Default::default()
                })
            }),
            ("billing", |b| {
                b.billing = Some(pb::bucket::Billing {
                    requester_pays: true,
                })
            }),
            ("satisfies_pzs", |b| b.satisfies_pzs = true),
            ("custom_placement_config", |b| {
                b.custom_placement_config = Some(pb::bucket::CustomPlacementConfig {
                    data_locations: vec!["us-east1".into()],
                })
            }),
            ("hierarchical_namespace", |b| {
                b.hierarchical_namespace = Some(pb::bucket::HierarchicalNamespace { enabled: true })
            }),
            ("ip_filter", |b| {
                b.ip_filter = Some(pb::bucket::IpFilter {
                    mode: Some("Enabled".into()),
                    ..Default::default()
                })
            }),
        ];
        for (field, set) in cases {
            let mut request = create("unsupported-field");
            set(request.bucket.as_mut().unwrap());
            let status = storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap_err();
            assert_eq!(status.code(), Code::Unimplemented, "{field}");
            assert!(
                status
                    .message()
                    .contains("CreateBucket fields outside the supported metadata subset"),
                "{field}: {status}"
            );
            assert!(!status.details().is_empty(), "{field}");
            assert_code(
                storage
                    .get_bucket(Request::new(get("unsupported-field")), None)
                    .await,
                Code::NotFound,
            );
        }
        // An empty message carries no setting, and `DEFAULT` is the only replication offered.
        let accepted: [fn(&mut pb::Bucket); 8] = [
            |b| b.billing = Some(Default::default()),
            |b| b.hierarchical_namespace = Some(Default::default()),
            |b| b.custom_placement_config = Some(Default::default()),
            |b| b.lifecycle = Some(Default::default()),
            |b| b.encryption = Some(Default::default()),
            |b| b.logging = Some(Default::default()),
            |b| b.ip_filter = Some(Default::default()),
            |b| b.rpo = "DEFAULT".into(),
        ];
        for (index, set) in accepted.into_iter().enumerate() {
            let mut request = create(&format!("accepted-default-{index}"));
            set(request.bucket.as_mut().unwrap());
            storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap();
        }
        storage.state.catalog.close().await.unwrap();
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
        // satisfies_pzs is no stored setting the emulator can answer for: Cloud Storage may
        // report true by location, so a selected value would be a guess.
        request.read_mask.as_mut().unwrap().paths = vec!["satisfies_pzs".into()];
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

    /// Cloud Storage compares whatever bucket metageneration conditions arrive on GetBucket,
    /// UpdateBucket and DeleteBucket (recorded 2026-11-18): a zero or negative match never
    /// holds, a zero or negative not-match always does, and both are evaluated together.
    /// LockBucketRetentionPolicy, whose request has only a required match, refuses 0 and
    /// negative values before the bucket is looked up.
    #[tokio::test]
    async fn bucket_rpcs_keep_their_own_metageneration_condition_rules() {
        const STRICT: &str = "Invalid generation conditions; use one match or not-match per generation, for example if_generation_match=0";
        let (_dir, storage) = storage(false).await;
        for name in ["conditions", "neg-not-match", "zero-not-match", "both-hold"] {
            storage
                .create_bucket(Request::new(create(name)), None)
                .await
                .unwrap();
        }
        let precondition = Status::from(CatalogError::PreconditionFailed);
        let unvalidated = [
            // A negative or zero match names no metageneration, so it never holds.
            (Some(-1), None, false),
            (Some(0), None, false),
            // A negative or zero not-match never names the bucket's, so it always holds.
            (None, Some(-1), true),
            (None, Some(0), true),
            // Both conditions are evaluated together.
            (Some(1), Some(2), true),
            (Some(1), Some(1), false),
        ];
        for (matching, not_matching, holds) in unvalidated {
            let request = pb::GetBucketRequest {
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
                ..get("conditions")
            };
            let result = storage.get_bucket(Request::new(request), None).await;
            if holds {
                assert_eq!(result.unwrap().into_inner().metageneration, 1);
            } else {
                let status = assert_code(result, Code::FailedPrecondition);
                // A not-match that alone fails is worded as a resource that has not been
                // modified (recorded 2026-11-18, case grpc-probe-inferred-bucket-masks).
                let expected = if (matching, not_matching) == (Some(1), Some(1)) {
                    "The resource you requested has not been modified according to the condition you specified."
                } else {
                    precondition.message()
                };
                assert_eq!(status.message(), expected, "{matching:?} {not_matching:?}");
            }
        }
        // A not-match that alone fails, with no match beside it.
        let status = assert_code(
            storage
                .get_bucket(
                    Request::new(pb::GetBucketRequest {
                        if_metageneration_not_match: Some(1),
                        ..get("conditions")
                    }),
                    None,
                )
                .await,
            Code::FailedPrecondition,
        );
        assert_eq!(
            status.message(),
            "The resource you requested has not been modified according to the condition you specified."
        );
        // A missing bucket is reported before any condition is compared.
        let missing = pb::GetBucketRequest {
            if_metageneration_match: Some(-1),
            ..get("missing-bucket")
        };
        assert_code(
            storage.get_bucket(Request::new(missing), None).await,
            Code::NotFound,
        );
        let delete = |name: &str, matching, not_matching| pb::DeleteBucketRequest {
            name: get(name).name,
            if_metageneration_match: matching,
            if_metageneration_not_match: not_matching,
        };
        // A not-match that alone fails is worded as a resource that has not been modified.
        const NOT_MODIFIED: &str = "The resource you requested has not been modified according to the condition you specified.";
        for (matching, not_matching, message) in [
            (Some(-1), None, precondition.message()),
            (Some(0), None, precondition.message()),
            (Some(2), Some(1), precondition.message()),
            (Some(1), Some(1), NOT_MODIFIED),
            (None, Some(1), NOT_MODIFIED),
        ] {
            let status = assert_code(
                storage
                    .delete_bucket(
                        Request::new(delete("conditions", matching, not_matching)),
                        None,
                    )
                    .await,
                Code::FailedPrecondition,
            );
            assert_eq!(status.message(), message);
        }
        for (name, matching, not_matching) in [
            ("neg-not-match", None, Some(-1)),
            ("zero-not-match", None, Some(0)),
            ("both-hold", Some(1), Some(2)),
        ] {
            storage
                .delete_bucket(Request::new(delete(name, matching, not_matching)), None)
                .await
                .unwrap();
            assert_code(
                storage.get_bucket(Request::new(get(name)), None).await,
                Code::NotFound,
            );
        }
        let update = |name: &str, matching, not_matching| pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: get(name).name,
                labels: [("changed".into(), "yes".into())].into(),
                ..Default::default()
            }),
            if_metageneration_match: matching,
            if_metageneration_not_match: not_matching,
            update_mask: fixtures::mask(&["labels"]),
            ..Default::default()
        };
        // UpdateBucket compares the conditions like the reads do (recorded 2026-11-18).
        // A not-match that alone fails is worded as a resource that has not been modified
        // (recorded 2026-11-18, case grpc-probe-inferred-bucket-masks).
        for (matching, not_matching, message) in [
            (Some(0), None, precondition.message()),
            (Some(-1), None, precondition.message()),
            (Some(1), Some(1), NOT_MODIFIED),
            (None, Some(1), NOT_MODIFIED),
        ] {
            let status = assert_code(
                storage
                    .update_bucket(
                        Request::new(update("conditions", matching, not_matching)),
                        None,
                    )
                    .await,
                Code::FailedPrecondition,
            );
            assert_eq!(status.message(), message, "{matching:?} {not_matching:?}");
        }
        let unchanged = storage
            .get_bucket(Request::new(get("conditions")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(unchanged.metageneration, 1);
        assert!(unchanged.labels.is_empty());
        for (metageneration, (matching, not_matching)) in
            (2..).zip([(None, Some(0)), (None, Some(-1)), (Some(3), Some(2))])
        {
            let updated = storage
                .update_bucket(
                    Request::new(update("conditions", matching, not_matching)),
                    None,
                )
                .await
                .unwrap()
                .into_inner();
            assert_eq!(updated.metageneration, metageneration);
        }
        // A missing bucket is reported before any condition is compared.
        assert_code(
            storage
                .update_bucket(Request::new(update("missing-bucket", Some(0), None)), None)
                .await,
            Code::NotFound,
        );
        // LockBucketRetentionPolicy has no recording (the harness never locks a real bucket),
        // so it keeps refusing a match that names no metageneration before the lookup.
        for name in ["conditions", "missing-bucket"] {
            for metageneration in [0, -1] {
                let lock = pb::LockBucketRetentionPolicyRequest {
                    bucket: get(name).name,
                    if_metageneration_match: metageneration,
                };
                let status = assert_code(
                    storage
                        .lock_bucket_retention_policy(Request::new(lock), None)
                        .await,
                    Code::InvalidArgument,
                );
                assert_eq!(status.message(), STRICT, "{name}");
            }
        }
        storage.state.catalog.close().await.unwrap();
    }

    /// Google documents no order for DeleteBucket failures. The adapter authorizes first; the
    /// catalog's deletion transaction then finds the bucket, compares the conditions and checks
    /// that no live or noncurrent object remains, in that order, and a failure deletes nothing.
    #[tokio::test]
    async fn delete_bucket_reports_authorization_existence_conditions_then_contents() {
        let (_dir, storage) = storage(true).await;
        let admin = caller(&storage, "admin").await;
        let reader = caller(&storage, "reader").await;
        let anonymous = Some(Caller::Anonymous);
        storage
            .create_bucket(Request::new(create("held")), admin)
            .await
            .unwrap();
        fixtures::put_keyed(&storage.state.catalog, "held", "object", b"data", None).await;
        let delete = |name: &str, matching| pb::DeleteBucketRequest {
            name: get(name).name,
            if_metageneration_match: matching,
            ..Default::default()
        };
        const DENIED: &str = "reader@example.invalid does not have storage.buckets.delete access to the Google Cloud Storage bucket. Permission 'storage.buckets.delete' denied on resource (or it may not exist).";
        const NOT_EMPTY: &str = "The bucket you tried to delete is not empty.";
        let precondition = Status::from(CatalogError::PreconditionFailed);
        let missing = Status::from(CatalogError::NotFound);
        // A failing condition rides along on every request but the last, so each answer shows
        // the check that comes before it.
        for (name, who, matching, code, message) in [
            // Authorization comes first, so a caller without access cannot learn whether the
            // bucket exists.
            (
                "missing-bucket",
                anonymous,
                Some(2),
                Code::Unauthenticated,
                "Login Required",
            ),
            (
                "missing-bucket",
                reader,
                Some(2),
                Code::PermissionDenied,
                DENIED,
            ),
            (
                "missing-bucket",
                admin,
                Some(2),
                Code::NotFound,
                missing.message(),
            ),
            (
                "held",
                anonymous,
                Some(2),
                Code::Unauthenticated,
                "Login Required",
            ),
            ("held", reader, Some(2), Code::PermissionDenied, DENIED),
            (
                "held",
                admin,
                Some(2),
                Code::FailedPrecondition,
                precondition.message(),
            ),
            ("held", admin, Some(1), Code::FailedPrecondition, NOT_EMPTY),
        ] {
            let status = assert_code(
                storage
                    .delete_bucket(Request::new(delete(name, matching)), who)
                    .await,
                code,
            );
            assert_eq!(status.message(), message, "{name} {matching:?}");
        }
        let bucket = storage
            .state
            .catalog
            .get_bucket("held", PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bucket.metageneration, 1);
        assert!(
            storage
                .state
                .catalog
                .get_object(
                    crate::storage::test_fixtures::live("held", "object"),
                    Default::default(),
                    PERMISSIVE,
                )
                .await
                .is_ok()
        );
        storage.state.catalog.close().await.unwrap();
    }

    /// The proto keeps a bucket deletable while it holds only soft-deleted objects or unfinished
    /// uploads, and refuses one that holds a live or noncurrent object; a bucket with a soft
    /// delete policy is soft-deleted and stays restorable (storage.proto lines 76-95).
    #[tokio::test]
    async fn delete_bucket_follows_the_shared_catalog_for_retained_state() {
        let (_dir, storage) = storage(false).await;
        let catalog = &storage.state.catalog;
        let mut soft = create("soft-deleted-only");
        soft.bucket.as_mut().unwrap().soft_delete_policy = Some(pb::bucket::SoftDeletePolicy {
            retention_duration: Some(prost_types::Duration {
                seconds: 604_800,
                nanos: 0,
            }),
            ..Default::default()
        });
        let mut versioned = create("noncurrent-only");
        versioned.bucket.as_mut().unwrap().versioning =
            Some(pb::bucket::Versioning { enabled: true });
        for request in [
            soft,
            versioned,
            create("multipart-only"),
            create("appendable-only"),
        ] {
            storage
                .create_bucket(Request::new(request), None)
                .await
                .unwrap();
        }
        for bucket in ["soft-deleted-only", "noncurrent-only"] {
            fixtures::put_keyed(catalog, bucket, "object", b"data", None).await;
            storage
                .delete_object(
                    Request::new(pb::DeleteObjectRequest {
                        bucket: get(bucket).name,
                        object: "object".into(),
                        ..Default::default()
                    }),
                    None,
                )
                .await
                .unwrap();
        }
        catalog
            .begin_xml_multipart(
                "multipart-only",
                "parts",
                crate::storage::xml_multipart::Options {
                    content_type: "text/plain".into(),
                    metadata: Default::default(),
                },
                crate::bucket::BucketAccess::Permissive,
            )
            .await
            .unwrap();
        let mut appendable = catalog
            .create_appendable(
                "appendable-only",
                "appending",
                crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
            )
            .await
            .unwrap();
        appendable
            .append(0, bytes::Bytes::from_static(b"unfinalized"), 1024)
            .await
            .unwrap();
        appendable.flush().await.unwrap();
        let delete = |name: &str| pb::DeleteBucketRequest {
            name: get(name).name,
            ..Default::default()
        };
        // A noncurrent version keeps the bucket, as a live object does.
        let status = assert_code(
            storage
                .delete_bucket(Request::new(delete("noncurrent-only")), None)
                .await,
            Code::FailedPrecondition,
        );
        assert_eq!(
            status.message(),
            "The bucket you tried to delete is not empty."
        );
        assert!(
            catalog
                .get_bucket("noncurrent-only", PERMISSIVE)
                .await
                .unwrap()
                .is_some()
        );
        // Soft-deleted objects, an unfinished XML multipart upload and an unfinalized appendable
        // object leave the bucket deletable. The open appendable writer then finds its object
        // gone at its next flush.
        for name in ["soft-deleted-only", "multipart-only", "appendable-only"] {
            storage
                .delete_bucket(Request::new(delete(name)), None)
                .await
                .unwrap();
            assert_code(
                storage.get_bucket(Request::new(get(name)), None).await,
                Code::NotFound,
            );
        }
        appendable
            .append(11, bytes::Bytes::from_static(b"more"), 1024)
            .await
            .unwrap();
        assert!(matches!(
            appendable.flush().await,
            Err(crate::storage::AppendError::Superseded)
        ));
        // Only the bucket with a soft delete policy was kept as a soft-deleted version.
        let retained = catalog
            .list_soft_deleted_buckets("p", "", None, 10)
            .await
            .unwrap()
            .items;
        assert_eq!(
            retained
                .iter()
                .map(|version| version.record.name.as_str())
                .collect::<Vec<_>>(),
            ["soft-deleted-only"]
        );
        storage.state.catalog.close().await.unwrap();
    }

    /// A bucket read mask is refused when it selects satisfies_pzs or a path under a field
    /// (UNIMPLEMENTED naming the path) or names no bucket field (Cloud Storage's
    /// INVALID_ARGUMENT, recorded 2026-11-18). GetBucket validates the mask right after the
    /// bucket name, before authorization and lookup; ListBuckets validates it after the caller,
    /// the project permission, page_size and prefix, and before the page token.
    #[tokio::test]
    async fn bucket_read_masks_refuse_unselectable_paths_in_each_rpcs_order() {
        let (_dir, storage) = storage(true).await;
        let admin = caller(&storage, "admin").await;
        let reader = caller(&storage, "reader").await;
        let anonymous = Some(Caller::Anonymous);
        storage
            .create_bucket(Request::new(create("masked")), admin)
            .await
            .unwrap();
        for path in [
            "satisfies_pzs",
            "no_such_field",
            "Name",
            "versioning.enabled",
        ] {
            let refused = |status: Status| {
                // Field names are case sensitive, so `Name` names nothing either.
                if ["no_such_field", "Name"].contains(&path) {
                    assert_eq!(status.code(), Code::InvalidArgument, "{path}");
                    assert_eq!(
                        status.message(),
                        format!("There is no field for this resource named '{path}'.")
                    );
                } else {
                    assert_eq!(status.code(), Code::Unimplemented, "{path}");
                    assert!(
                        status
                            .message()
                            .contains(&format!("bucket read_mask {path}")),
                        "{status}"
                    );
                }
            };
            let read = |name: &str| pb::GetBucketRequest {
                read_mask: fixtures::mask(&[path]),
                ..get(name)
            };
            for (name, who) in [
                ("masked", admin),
                ("masked", anonymous),
                ("masked", reader),
                ("missing-bucket", reader),
                ("missing-bucket", admin),
            ] {
                refused(
                    storage
                        .get_bucket(Request::new(read(name)), who)
                        .await
                        .unwrap_err(),
                );
            }
            // A malformed bucket name is reported before the mask.
            assert_code(
                storage.get_bucket(Request::new(read("UPPER")), admin).await,
                Code::InvalidArgument,
            );
            let list = |page_size, prefix: &str, page_token: &str| pb::ListBucketsRequest {
                parent: "projects/p".into(),
                page_size,
                prefix: prefix.into(),
                page_token: page_token.into(),
                read_mask: fixtures::mask(&[path]),
                ..Default::default()
            };
            for (who, code) in [
                (anonymous, Code::Unauthenticated),
                (reader, Code::PermissionDenied),
            ] {
                assert_code(
                    storage
                        .list_buckets(Request::new(list(0, "", "")), who)
                        .await,
                    code,
                );
            }
            for invalid in [list(-1, "", ""), list(0, &"p".repeat(223), "")] {
                assert_code(
                    storage.list_buckets(Request::new(invalid), admin).await,
                    Code::InvalidArgument,
                );
            }
            for request in [list(0, "", ""), list(1, "", "not-a-token")] {
                refused(
                    storage
                        .list_buckets(Request::new(request), admin)
                        .await
                        .unwrap_err(),
                );
            }
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_bucket_options_reject_only_what_is_unsupported() {
        let (_dir, storage) = storage(false).await;
        for (acl, default_acl) in [("publicRead", ""), ("", "bucketOwnerRead")] {
            let request = pb::CreateBucketRequest {
                predefined_acl: acl.into(),
                predefined_default_object_acl: default_acl.into(),
                ..create("acl-bucket")
            };
            assert_unsupported(
                storage.create_bucket(Request::new(request), None).await,
                "CreateBucket predefined ACLs",
            );
        }
        assert_code(
            storage
                .get_bucket(Request::new(get("acl-bucket")), None)
                .await,
            Code::NotFound,
        );
        storage
            .create_bucket(Request::new(create("mask-bucket")), None)
            .await
            .unwrap();
        let masked = |paths: &[&str]| pb::GetBucketRequest {
            read_mask: fixtures::mask(paths),
            ..get("mask-bucket")
        };
        // Cloud Storage serves nested paths; gcs-mimic selects whole fields only.
        assert_unsupported(
            storage
                .get_bucket(Request::new(masked(&["versioning.enabled"])), None)
                .await,
            "bucket read_mask versioning.enabled",
        );
        // A wildcard beside a name is malformed, in Cloud Storage's recorded wording.
        let status = assert_code(
            storage
                .get_bucket(Request::new(masked(&["*", "name"])), None)
                .await,
            Code::InvalidArgument,
        );
        assert_eq!(
            status.message(),
            "Field masks may either contain only a single '*' field or specify specific fields, but not both. Got: '*,name'."
        );
        // Every local bucket is reachable, so asking for partial success changes nothing.
        let list = |return_partial_success| pb::ListBucketsRequest {
            parent: "projects/p".into(),
            return_partial_success,
            ..Default::default()
        };
        let complete = storage
            .list_buckets(Request::new(list(false)), None)
            .await
            .unwrap()
            .into_inner();
        let partial = storage
            .list_buckets(Request::new(list(true)), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(partial, complete);
        assert!(partial.unreachable.is_empty());
        assert_eq!(partial.buckets.len(), 1);
        storage.state.catalog.close().await.unwrap();
    }

    /// The read mask google-cloud-storage 2.78.0 for Java sends on every GetBucket and
    /// ListBuckets: the gRPC name of every `Storage.BucketField`, sorted.
    const JAVA_DEFAULT_BUCKET_MASK: &[&str] = &[
        "acl",
        "autoclass",
        "billing",
        "bucket_id",
        "cors",
        "create_time",
        "custom_placement_config",
        "default_event_based_hold",
        "default_object_acl",
        "encryption",
        "etag",
        "hierarchical_namespace",
        "iam_config",
        "ip_filter",
        "labels",
        "lifecycle",
        "location",
        "location_type",
        "logging",
        "metageneration",
        "name",
        "object_retention",
        "owner",
        "project",
        "retention_policy",
        "rpo",
        "soft_delete_policy",
        "storage_class",
        "update_time",
        "versioning",
        "website",
    ];

    #[tokio::test]
    async fn grpc_bucket_reads_accept_the_java_default_mask_and_leave_unstored_settings_unset() {
        let (_dir, storage) = storage(false).await;
        let mut request = create("java-mask");
        let bucket = request.bucket.as_mut().unwrap();
        bucket.labels.insert("team".into(), "dev".into());
        bucket.versioning = Some(pb::bucket::Versioning { enabled: true });
        storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap();
        let unmasked = storage
            .get_bucket(Request::new(get("java-mask")), None)
            .await
            .unwrap()
            .into_inner();
        // Naming every field reads what a request without a mask reads: the settings bucket
        // creation and update refuse are never stored, so they stay unset either way.
        let masked = storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    read_mask: fixtures::mask(JAVA_DEFAULT_BUCKET_MASK),
                    ..get("java-mask")
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(masked, unmasked);
        assert!(masked.billing.is_none() && masked.website.is_none() && masked.rpo.is_empty());
        // `*` alone selects every field too.
        let wildcard = storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    read_mask: fixtures::mask(&["*"]),
                    ..get("java-mask")
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(wildcard, unmasked);
        // Selecting only an unstored setting returns nothing else.
        let billing_only = storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    read_mask: fixtures::mask(&["billing", "ip_filter"]),
                    ..get("java-mask")
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(billing_only, pb::Bucket::default());
        let listed = storage
            .list_buckets(
                Request::new(pb::ListBucketsRequest {
                    parent: "projects/p".into(),
                    read_mask: fixtures::mask(JAVA_DEFAULT_BUCKET_MASK),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.buckets, vec![unmasked]);
        storage.state.catalog.close().await.unwrap();
    }

    /// A project's number names it like its ID: the `projects/N` a bucket renders lists it, and
    /// a page token from one spelling continues under the other.
    #[tokio::test]
    async fn grpc_lists_a_project_by_the_number_its_buckets_render() {
        let (_dir, storage) = storage(false).await;
        let mut rendered = String::new();
        for name in ["page-a", "page-b"] {
            rendered = storage
                .create_bucket(Request::new(create(name)), None)
                .await
                .unwrap()
                .into_inner()
                .project;
        }
        assert_eq!(rendered, "projects/345434927424");
        let mut query = pb::ListBucketsRequest {
            parent: rendered,
            page_size: 1,
            ..Default::default()
        };
        let first = storage
            .list_buckets(Request::new(query.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(first.buckets[0].bucket_id, "page-a");
        query.parent = "projects/p".into();
        query.page_token = first.next_page_token;
        let second = storage
            .list_buckets(Request::new(query), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second.buckets[0].bucket_id, "page-b");
        assert!(second.next_page_token.is_empty());
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
        let routing = fixtures::bucket_routing("page-a");
        let request = fixtures::routed_request(&routing, get("page-a"));
        pb::storage_server::Storage::get_bucket(&storage, request)
            .await
            .unwrap();
        let other_bucket = |routed: &str| {
            format!(
                "The x-goog-request-params must specify the same bucket as the resource in the request. Request bucket was 'page-a' but x-goog-request-params specified '{routed}'"
            )
        };
        for (header, message) in [
            // Recorded 2026-11-18 on GetBucket (case grpc-probe-routing).
            (
                "bucket=projects%2F_%2Fbuckets%2Fpage-b",
                other_bucket("page-b"),
            ),
            // An unknown key is ignored (recorded on GetBucket beside the bucket), so metadata
            // that names only one routes no bucket, answered as an empty routed bucket is
            // (recorded on GetObject routed by a project alone).
            ("unknown=value", other_bucket("")),
            // A bucket pair without a value was recorded on GetObject (case
            // grpc-probe-inferred-routing); a malformed percent escape was not, so that
            // refusal is local.
            ("bucket=%GG", "Malformed URI encoding".to_owned()),
            (
                "bucket",
                "Invalid bucket name found in x-goog-request-params metadata (bucket). Must be of the form `projects/_/buckets/{bucket_id}`".to_owned(),
            ),
        ] {
            let request = fixtures::routed_request(header, get("page-a"));
            let status = assert_code(
                pb::storage_server::Storage::get_bucket(&storage, request).await,
                Code::InvalidArgument,
            );
            assert_eq!(status.message(), message, "{header}");
        }
        // Recorded 2026-11-18 on GetBucket (case grpc-probe-routing).
        let status = assert_code(
            pb::storage_server::Storage::get_bucket(&storage, Request::new(get("page-a"))).await,
            Code::InvalidArgument,
        );
        assert_eq!(
            status.message(),
            "An x-goog-request-params request metadata property must be provided for this request."
        );
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
        // The output-only toggle time is ignored; the server sets it to the creation time.
        let mut request = create("output-only");
        request.bucket.as_mut().unwrap().autoclass = Some(pb::bucket::Autoclass {
            enabled: true,
            toggle_time: Some(prost_types::Timestamp::default()),
            ..Default::default()
        });
        let created = storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(created.autoclass.unwrap().toggle_time, created.create_time);
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
    async fn grpc_lifecycle_sizes_only_block_reads_that_request_lifecycle() {
        let (_dir, storage) = storage(false).await;
        let record = json::parse_insert(
            br#"{"name":"json-size-bucket","lifecycle":{"rule":[{"action":{"type":"Delete"},"condition":{"sizeBelowBytes":100}}]}}"#,
            "p",
            storage.state.clock.now(),
        ).unwrap();
        storage.state.catalog.insert_bucket(record).await.unwrap();
        storage
            .create_bucket(Request::new(create("plain-bucket")), None)
            .await
            .unwrap();
        for read_mask in [
            None,
            fixtures::mask(&[]),
            fixtures::mask(&["*"]),
            fixtures::mask(&["lifecycle"]),
        ] {
            assert_code(
                storage
                    .get_bucket(
                        Request::new(pb::GetBucketRequest {
                            read_mask: read_mask.clone(),
                            ..get("json-size-bucket")
                        }),
                        None,
                    )
                    .await,
                Code::Unimplemented,
            );
            assert_code(
                storage
                    .list_buckets(
                        Request::new(pb::ListBucketsRequest {
                            parent: "projects/p".into(),
                            read_mask,
                            ..Default::default()
                        }),
                        None,
                    )
                    .await,
                Code::Unimplemented,
            );
        }
        let masked = storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    read_mask: fixtures::mask(&["name", "metageneration"]),
                    ..get("json-size-bucket")
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(masked.name, "projects/_/buckets/json-size-bucket");
        assert_eq!(masked.metageneration, 1);
        assert!(masked.lifecycle.is_none());
        let listed = storage
            .list_buckets(
                Request::new(pb::ListBucketsRequest {
                    parent: "projects/p".into(),
                    read_mask: fixtures::mask(&["name"]),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.buckets.len(), 2);
        assert!(
            listed
                .buckets
                .iter()
                .all(|bucket| bucket.lifecycle.is_none())
        );
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
        assert_eq!(fetched.etag, crate::etag::bucket_etag(&record));
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

    /// The catalog's list budget gives a resource above a client's common 4 MiB limit a page of
    /// its own, so the buckets around it stay readable, and paging reaches them all.
    #[tokio::test]
    async fn grpc_bucket_lists_keep_a_large_bucket_on_its_own_page() {
        use prost::Message;
        let dir = TestDir::new();
        let storage = fixtures::storage_with(
            Arc::new(crate::server::State::for_test(
                Catalog::open(&dir.0).await.unwrap(),
                Auth::permissive(),
            )),
            1024,
        );
        let origin = format!("https://{}.invalid", "o".repeat(5 * 1024 * 1024));
        for body in [
            serde_json::json!({"name":"a-small"}),
            serde_json::json!({"name":"m-large","cors":[{"origin":[origin]}]}),
            serde_json::json!({"name":"z-small"}),
        ] {
            let record = crate::json::parse_insert(
                body.to_string().as_bytes(),
                "p",
                std::time::SystemTime::now(),
            )
            .unwrap();
            storage.state.catalog.insert_bucket(record).await.unwrap();
        }
        let (mut pages, mut token) = (Vec::new(), String::new());
        loop {
            let page = storage
                .list_buckets(
                    Request::new(pb::ListBucketsRequest {
                        parent: "projects/p".into(),
                        page_token: std::mem::take(&mut token),
                        ..Default::default()
                    }),
                    None,
                )
                .await
                .unwrap()
                .into_inner();
            pages.push((
                page.buckets
                    .iter()
                    .map(|bucket| bucket.name.clone())
                    .collect::<Vec<_>>(),
                page.encoded_len(),
            ));
            if page.next_page_token.is_empty() {
                break;
            }
            token = page.next_page_token;
        }
        let names: Vec<_> = pages.iter().map(|(names, _)| names.join(",")).collect();
        assert_eq!(
            names,
            [
                "projects/_/buckets/a-small",
                "projects/_/buckets/m-large",
                "projects/_/buckets/z-small"
            ]
        );
        assert!(pages[0].1 < 1024 * 1024 && pages[2].1 < 1024 * 1024);
        assert!(pages[1].1 > 5 * 1024 * 1024);
        // A client that keeps a 4 MiB limit can still read that bucket without its large fields.
        let masked = storage
            .list_buckets(
                Request::new(pb::ListBucketsRequest {
                    parent: "projects/p".into(),
                    prefix: "m-".into(),
                    read_mask: Some(prost_types::FieldMask {
                        paths: vec!["name".into()],
                    }),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(masked.buckets.len(), 1);
        assert!(masked.encoded_len() < 1024, "{}", masked.encoded_len());
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_bucket_list_acl_views_keep_per_bucket_permissions_and_uniform_access() {
        let dir = TestDir::new();
        let storage = fixtures::storage_with(
            Arc::new(crate::server::State::for_test(
                Catalog::open(&dir.0).await.unwrap(),
                Auth::strict(registry(&[
                    ("admin", "p", "roles/storage.admin"),
                    ("reader", "p", "roles/storage.bucketViewer"),
                ])),
            )),
            1024,
        );
        let admin = caller(&storage, "admin").await;
        let viewer = caller(&storage, "reader").await;
        for name in ["batch-readable", "batch-denied", "batch-uniform"] {
            storage
                .create_bucket(Request::new(create(name)), admin)
                .await
                .unwrap();
        }
        // The collection permission does not grant access to every bucket's ACL.
        for name in ["batch-readable", "batch-uniform"] {
            storage
                .state
                .catalog
                .set_policy(
                    name,
                    crate::auth::Policy {
                        bindings: vec![crate::auth::Binding {
                            role: "roles/storage.legacyBucketOwner".parse().unwrap(),
                            members: vec!["allAuthenticatedUsers".parse().unwrap()],
                        }],
                        etag: None,
                    },
                    PERMISSIVE,
                )
                .await
                .unwrap();
        }
        storage
            .state
            .catalog
            .patch_bucket(
                "batch-uniform",
                crate::bucket::BucketMetadataUpdate {
                    uniform_bucket_level_access_enabled: Some(true.into()),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let request = || {
            Request::new(pb::ListBucketsRequest {
                parent: "projects/p".into(),
                read_mask: fixtures::mask(&["*"]),
                ..Default::default()
            })
        };
        let response = storage
            .list_buckets(request(), viewer)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.buckets.len(), 3);
        for bucket in response.buckets {
            let expected = storage
                .get_bucket(Request::new(get(&bucket.bucket_id)), viewer)
                .await
                .unwrap()
                .into_inner();
            assert_eq!(bucket.owner, expected.owner);
            assert_eq!(bucket.acl, expected.acl);
            assert_eq!(bucket.default_object_acl, expected.default_object_acl);
            match bucket.bucket_id.as_str() {
                "batch-readable" => {
                    assert!(bucket.owner.is_some());
                    assert!(
                        bucket
                            .acl
                            .iter()
                            .any(|entry| entry.entity == "allAuthenticatedUsers")
                    );
                }
                "batch-denied" | "batch-uniform" => {
                    assert!(bucket.owner.is_none());
                    assert!(bucket.acl.is_empty() && bucket.default_object_acl.is_empty());
                }
                other => panic!("unexpected bucket: {other}"),
            }
        }
        storage.state.catalog.close().await.unwrap();
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
                .any(|e| e.entity.as_str() == "allAuthenticatedUsers")
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
        let created = storage
            .create_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let uniform = storage
            .get_bucket(Request::new(get("uniform-bucket")), None)
            .await
            .unwrap()
            .into_inner();
        let relabeled = storage
            .update_bucket(
                Request::new(update_acl(
                    pb::Bucket {
                        name: get("uniform-bucket").name,
                        labels: [("k".to_owned(), "v".to_owned())].into(),
                        ..Default::default()
                    },
                    "labels",
                )),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        // Cloud Storage answered UpdateBucket on a uniform-access bucket without owner, acl and
        // default_object_acl, and with all three on a bucket without it (cases
        // grpc-iam-config-partial-update and grpc-empty-iam-update, recorded 2026-11-13).
        for (rpc, bucket) in [
            ("create", &created),
            ("get", &uniform),
            ("update", &relabeled),
        ] {
            assert!(
                bucket.acl.is_empty() && bucket.default_object_acl.is_empty(),
                "{rpc}"
            );
            assert!(bucket.owner.is_none(), "{rpc}");
        }
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
            compat_profile: crate::config::CompatProfile::Native,
            max_object_bytes: 1024,
            state: Arc::new(crate::server::State::for_test(
                Catalog::open(&dir.0).await.unwrap(),
                auth,
            )),
        };
        storage
            .create_bucket(
                Request::new(create("strict-acl")),
                caller(&storage, "admin").await,
            )
            .await
            .unwrap();
        let admin = storage
            .get_bucket(
                Request::new(get("strict-acl")),
                caller(&storage, "admin").await,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(!admin.acl.is_empty());
        // Reading the bucket succeeds without the ACL for a caller that cannot read ACLs.
        let viewer = storage
            .get_bucket(
                Request::new(get("strict-acl")),
                caller(&storage, "viewer").await,
            )
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
                .update_bucket(
                    Request::new(request.clone()),
                    caller(&storage, "viewer").await,
                )
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
            .get_bucket(
                Request::new(get("strict-acl")),
                caller(&storage, "admin").await,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(unchanged.acl, admin.acl);
        storage
            .update_bucket(Request::new(request), caller(&storage, "admin").await)
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
            compat_profile: crate::config::CompatProfile::Native,
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
                caller(&storage, "editor").await,
            )
            .await
            .unwrap();
        assert_code(
            storage
                .create_bucket(
                    Request::new(request.clone()),
                    caller(&storage, "editor").await,
                )
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
            .create_bucket(Request::new(request), caller(&storage, "admin").await)
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
                .create_bucket(
                    Request::new(request.clone()),
                    caller(&storage, "admin").await,
                )
                .await,
            Code::InvalidArgument,
        );
        request.enable_object_retention = true;
        storage
            .create_bucket(Request::new(request), caller(&storage, "admin").await)
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
            let caller = storage.authenticate(&metadata).await.unwrap();
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
            let caller = storage.authenticate(&metadata).await.unwrap();
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
                        role: "roles/storage.admin".parse().unwrap(),
                        members: vec!["allUsers".parse().unwrap()],
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
        // The Requester Pays billing project is ignored, as in the JSON API.
        for value in ["", "billing-project"] {
            let routing = fixtures::bucket_routing("public-bucket");
            let mut request = fixtures::routed_request(&routing, get("public-bucket"));
            request
                .metadata_mut()
                .insert("x-goog-user-project", value.parse().unwrap());
            pb::storage_server::Storage::get_bucket(&storage, request)
                .await
                .unwrap();
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
            Status::from(crate::api_error::ApiError::new(
                hyper::StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "catalog unavailable"
            ))
            .code(),
            Code::Unavailable
        );
        assert_eq!(
            Status::from(crate::api_error::ApiError::unsupported("callback")).code(),
            Code::Unimplemented
        );
        storage.state.catalog.close().await.unwrap();
    }
}
