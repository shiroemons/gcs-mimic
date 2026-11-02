use super::{Storage, access_status, convert, iam, names, pb, status::unsupported};
use crate::object::ObjectTarget;
use crate::{
    auth::{self, Caller},
    bucket::{BucketAccess, BucketMetadataUpdate},
    grpc::validate::{self, request_conditions, strict_conditions},
    json::{self, JsonError},
    object::{ComposeSource, ExpectedHashes, ObjectConditions, ObjectError},
    server::authorize_bucket,
    storage::{
        DerivedDestination, DerivedRequestGuard, MoveAccess, MoveConditions, MoveNames,
        RestoreAccess,
        rewrite::{RewriteBinding, RewriteError, RewriteStart},
    },
};
use serde_json::{Map, Value, json};
use tonic::{Request, Response, Status};

fn error(error: impl Into<JsonError>) -> Status {
    super::status::from_json(error.into())
}

fn mask(mask: Option<&prost_types::FieldMask>) -> Result<&[String], Status> {
    mask.filter(|m| !m.paths.is_empty())
        .map(|m| m.paths.as_slice())
        .ok_or_else(|| {
            Status::invalid_argument(
                "update_mask is required; use paths such as content_type or versioning.enabled",
            )
        })
}

fn common(params: &Option<pb::CommonObjectRequestParams>) -> Result<(), Status> {
    validate::common(
        params.as_ref(),
        "customer-supplied encryption; omit common_object_request_params",
    )
}

/// The bucket of an IAM resource. Managed folders and objects are valid IAM resources
/// without a local policy store, so they are unsupported rather than malformed.
fn iam_bucket(resource: &str) -> Result<&str, Status> {
    if let Some((bucket, child)) = resource
        .strip_prefix("projects/_/buckets/")
        .and_then(|rest| rest.split_once('/'))
        && (child.starts_with("managedFolders/") || child.starts_with("objects/"))
    {
        names::bucket_name(&format!("projects/_/buckets/{bucket}"))?;
        return Err(unsupported(
            "managed folder and object IAM policies; use a bucket resource such as projects/_/buckets/example-bucket",
        ));
    }
    names::bucket_name(resource)
}

fn policy(policy: auth::Policy) -> Result<iam::Policy, Status> {
    Ok(iam::Policy {
        version: 1,
        bindings: policy
            .bindings
            .into_iter()
            .map(|b| iam::Binding {
                role: b.role,
                members: b.members,
                condition: None,
            })
            .collect(),
        etag: policy
            .etag
            .map(|v| {
                json::decode_hash::<8>(&v).map(|v| v.to_vec()).map_err(|_| {
                    Status::internal("Invalid stored policy etag; restore a valid catalog backup")
                })
            })
            .transpose()?
            .unwrap_or_default(),
        ..Default::default()
    })
}

fn derived_fields(o: &pb::Object) -> Result<Value, Status> {
    let supported = pb::Object {
        name: o.name.clone(),
        bucket: o.bucket.clone(),
        content_type: o.content_type.clone(),
        content_encoding: o.content_encoding.clone(),
        content_disposition: o.content_disposition.clone(),
        content_language: o.content_language.clone(),
        cache_control: o.cache_control.clone(),
        metadata: o.metadata.clone(),
        storage_class: o.storage_class.clone(),
        temporary_hold: o.temporary_hold,
        event_based_hold: o.event_based_hold,
        custom_time: o.custom_time,
        ..Default::default()
    };
    if o != &supported {
        return Err(unsupported(
            "derived object fields; supply only editable JSON-supported metadata",
        ));
    }
    let mut value = json!({"metadata":o.metadata});
    for (key, v) in [
        ("contentType", &o.content_type),
        ("contentEncoding", &o.content_encoding),
        ("contentDisposition", &o.content_disposition),
        ("contentLanguage", &o.content_language),
        ("cacheControl", &o.cache_control),
        ("storageClass", &o.storage_class),
    ] {
        if !v.is_empty() {
            value[key] = v.clone().into();
        }
    }
    if o.temporary_hold {
        value["temporaryHold"] = true.into();
    }
    if let Some(v) = o.event_based_hold {
        value["eventBasedHold"] = v.into();
    }
    if let Some(v) = o.custom_time {
        value["customTime"] = v.to_string().into();
    }
    Ok(value)
}

fn lifecycle(value: &pb::bucket::Lifecycle) -> Value {
    let rules = value
        .rule
        .iter()
        .map(|rule| {
            let mut value = json!({});
            if let Some(action) = &rule.action {
                value["action"] = json!({"type": action.r#type});
                if !action.storage_class.is_empty() {
                    value["action"]["storageClass"] = action.storage_class.clone().into();
                }
            }
            if let Some(condition) = &rule.condition {
                let mut conditions = Map::new();
                for (key, number) in [
                    ("age", condition.age_days),
                    ("numNewerVersions", condition.num_newer_versions),
                    ("daysSinceCustomTime", condition.days_since_custom_time),
                    (
                        "daysSinceNoncurrentTime",
                        condition.days_since_noncurrent_time,
                    ),
                ] {
                    if let Some(number) = number {
                        conditions.insert(key.into(), number.into());
                    }
                }
                if let Some(live) = condition.is_live {
                    conditions.insert("isLive".into(), live.into());
                }
                for (key, date) in [
                    ("createdBefore", &condition.created_before),
                    ("customTimeBefore", &condition.custom_time_before),
                    ("noncurrentTimeBefore", &condition.noncurrent_time_before),
                ] {
                    if let Some(date) = date {
                        conditions.insert(
                            key.into(),
                            format!("{:04}-{:02}-{:02}", date.year, date.month, date.day).into(),
                        );
                    }
                }
                for (key, values) in [
                    ("matchesStorageClass", &condition.matches_storage_class),
                    ("matchesPrefix", &condition.matches_prefix),
                    ("matchesSuffix", &condition.matches_suffix),
                ] {
                    if !values.is_empty() {
                        conditions.insert(key.into(), json!(values));
                    }
                }
                value["condition"] = conditions.into();
            }
            value
        })
        .collect::<Vec<_>>();
    json!({"rule": rules})
}

/// The JSON form of a replacement ACL. Only entity and role are writable; the shared JSON
/// validation rejects invalid entities and roles and uniform bucket-level access.
fn acl_entries<'a>(entries: impl Iterator<Item = (&'a String, &'a String)>) -> Value {
    entries
        .map(|(entity, role)| json!({"entity": entity, "role": role}))
        .collect::<Vec<_>>()
        .into()
}

/// The JSON form of an object retention configuration; an absent message removes it.
fn retention(retention: Option<&pb::object::Retention>) -> Result<Value, Status> {
    use pb::object::retention::Mode;
    let Some(retention) = retention else {
        return Ok(Value::Null);
    };
    let mode = match Mode::try_from(retention.mode) {
        Ok(Mode::Unlocked) => "Unlocked",
        Ok(Mode::Locked) => "Locked",
        _ => {
            return Err(Status::invalid_argument(
                "retention.mode must be UNLOCKED or LOCKED",
            ));
        }
    };
    let until = retention.retain_until_time.ok_or_else(|| {
        Status::invalid_argument("retention.retain_until_time is required with retention.mode")
    })?;
    Ok(json!({"mode": mode, "retainUntilTime": until.to_string()}))
}

fn bucket_patch(b: &pb::Bucket, paths: &[String]) -> Result<Value, Status> {
    let mut body = json!({});
    for path in paths {
        match path.as_str() {
            "labels" => {
                body["labels"] = if b.labels.is_empty() {
                    Value::Null
                } else {
                    json!(b.labels)
                };
            }
            p if p.starts_with("labels.") && p.len() > 7 => {
                let key = &p[7..];
                body["labels"][key] = b.labels.get(key).map_or(Value::Null, |v| v.clone().into());
            }
            "storage_class" => body["storageClass"] = b.storage_class.clone().into(),
            "autoclass" | "autoclass.enabled" | "autoclass.terminal_storage_class" => {
                let autoclass = b.autoclass.clone().unwrap_or_default();
                if path != "autoclass.terminal_storage_class" {
                    body["autoclass"]["enabled"] = autoclass.enabled.into();
                }
                if path != "autoclass.enabled"
                    && let Some(class) = autoclass.terminal_storage_class
                {
                    body["autoclass"]["terminalStorageClass"] = class.into();
                }
            }
            "versioning" | "versioning.enabled" => {
                body["versioning"] =
                    json!({"enabled": b.versioning.as_ref().is_some_and(|v| v.enabled)});
            }
            "default_event_based_hold" => {
                body["defaultEventBasedHold"] = b.default_event_based_hold.into();
            }
            "cors" => body["cors"] = super::buckets::cors_json(&b.cors),
            "lifecycle" | "lifecycle.rule" => {
                body["lifecycle"] = b.lifecycle.as_ref().map_or(json!({"rule": []}), lifecycle);
            }
            "retention_policy" | "retention_policy.retention_duration" => {
                body["retentionPolicy"] = b
                    .retention_policy
                    .as_ref()
                    .and_then(|p| p.retention_duration)
                    .map_or(
                        Value::Null,
                        |d| json!({"retentionPeriod": d.seconds.to_string()}),
                    );
                if b.retention_policy.as_ref().is_some_and(|p| p.is_locked) {
                    return Err(unsupported(
                        "retention_policy.is_locked; use LockBucketRetentionPolicy",
                    ));
                }
            }
            "soft_delete_policy" | "soft_delete_policy.retention_duration" => {
                let duration = b
                    .soft_delete_policy
                    .as_ref()
                    .and_then(|p| p.retention_duration)
                    .unwrap_or_default();
                if duration.nanos != 0 {
                    return Err(unsupported(
                        "fractional soft delete duration; use whole seconds",
                    ));
                }
                body["softDeletePolicy"] =
                    json!({"retentionDurationSeconds": duration.seconds.to_string()});
            }
            "iam_config" => {
                // Go uses this parent mask even when only one IAM setting is present.
                if let Some(iam) = &b.iam_config {
                    if let Some(uniform) = &iam.uniform_bucket_level_access {
                        body["iamConfiguration"]["uniformBucketLevelAccess"] =
                            json!({"enabled": uniform.enabled});
                    }
                    if !iam.public_access_prevention.is_empty() {
                        body["iamConfiguration"]["publicAccessPrevention"] =
                            iam.public_access_prevention.clone().into();
                    }
                }
            }
            "iam_config.uniform_bucket_level_access"
            | "iam_config.uniform_bucket_level_access.enabled" => {
                let enabled = b
                    .iam_config
                    .as_ref()
                    .and_then(|iam| iam.uniform_bucket_level_access.as_ref())
                    .is_some_and(|uniform| uniform.enabled);
                body["iamConfiguration"]["uniformBucketLevelAccess"] = json!({"enabled": enabled});
            }
            "acl" => body["acl"] = acl_entries(b.acl.iter().map(|e| (&e.entity, &e.role))),
            "default_object_acl" => {
                body["defaultObjectAcl"] =
                    acl_entries(b.default_object_acl.iter().map(|e| (&e.entity, &e.role)));
            }
            "iam_config.public_access_prevention" => {
                body["iamConfiguration"]["publicAccessPrevention"] = b
                    .iam_config
                    .as_ref()
                    .map(|i| i.public_access_prevention.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("inherited")
                    .into();
            }
            _ => {
                return Err(unsupported(&format!(
                    "UpdateBucket update_mask {path}; use labels, versioning, lifecycle or iam_config"
                )));
            }
        }
    }
    Ok(body)
}

fn object_patch(o: &pb::Object, paths: &[String]) -> Result<Value, Status> {
    let mut body = json!({});
    for path in paths {
        match path.as_str() {
            "metadata" => {
                body["metadata"] = if o.metadata.is_empty() {
                    Value::Null
                } else {
                    json!(o.metadata)
                }
            }
            p if p.starts_with("metadata.") && p.len() > 9 => {
                let key = &p[9..];
                body["metadata"][key] = o
                    .metadata
                    .get(key)
                    .map_or(Value::Null, |v| v.clone().into());
            }
            "temporary_hold" => body["temporaryHold"] = o.temporary_hold.into(),
            "event_based_hold" => {
                body["eventBasedHold"] = o.event_based_hold.unwrap_or(false).into()
            }
            "custom_time" => {
                body["customTime"] = o.custom_time.map_or(Value::Null, |v| v.to_string().into())
            }
            "content_type" => body["contentType"] = o.content_type.clone().into(),
            "content_encoding" => body["contentEncoding"] = o.content_encoding.clone().into(),
            "content_disposition" => {
                body["contentDisposition"] = o.content_disposition.clone().into()
            }
            "content_language" => {
                body["contentLanguage"] = if o.content_language.is_empty() {
                    Value::Null
                } else {
                    o.content_language.clone().into()
                }
            }
            "cache_control" => body["cacheControl"] = o.cache_control.clone().into(),
            // Go sends this output-only path for an ordinary empty Update.
            "create_time" => {}
            "acl" => body["acl"] = acl_entries(o.acl.iter().map(|e| (&e.entity, &e.role))),
            "retention" => body["retention"] = retention(o.retention.as_ref())?,
            _ => {
                return Err(unsupported(&format!(
                    "UpdateObject update_mask {path}; use metadata or content_type"
                )));
            }
        }
    }
    Ok(body)
}

fn rewrite_error(e: RewriteError) -> Status {
    match e {
        RewriteError::Invalid => Status::invalid_argument(
            "Invalid rewrite token; use the token from the preceding response",
        ),
        RewriteError::Gone => {
            Status::failed_precondition("Rewrite token expired or unavailable; restart the rewrite")
        }
        RewriteError::Object(e) => error(e),
    }
}

fn check_rewrite_request(r: &pb::RewriteObjectRequest) -> Result<(), Status> {
    common(&r.common_object_request_params)?;
    if !r.destination_kms_key.is_empty()
        || !r.destination_predefined_acl.is_empty()
        || !r.copy_source_encryption_algorithm.is_empty()
        || !r.copy_source_encryption_key_bytes.is_empty()
        || !r.copy_source_encryption_key_sha256_bytes.is_empty()
        || r.object_checksums
            .as_ref()
            .is_some_and(|c| c.crc32c.is_some() || !c.md5_hash.is_empty())
    {
        return Err(unsupported(
            "RewriteObject encryption/ACL/checksums; omit these options",
        ));
    }
    if r.source_generation < 0
        || r.max_bytes_rewritten_per_call < 0
        || r.max_bytes_rewritten_per_call % 1_048_576 != 0
    {
        return Err(Status::invalid_argument(
            "source_generation must be positive or omitted; max_bytes_rewritten_per_call must be a multiple of 1048576",
        ));
    }
    Ok(())
}

/// The request parameters a rewrite token binds, in their JSON API names.
fn rewrite_parameters(r: &pb::RewriteObjectRequest) -> Map<String, Value> {
    let mut parameters = Map::new();
    for (key, v) in [
        (
            "sourceGeneration",
            (r.source_generation != 0).then_some(r.source_generation),
        ),
        (
            "maxBytesRewrittenPerCall",
            (r.max_bytes_rewritten_per_call != 0).then_some(r.max_bytes_rewritten_per_call),
        ),
        ("ifGenerationMatch", r.if_generation_match),
        ("ifGenerationNotMatch", r.if_generation_not_match),
        ("ifMetagenerationMatch", r.if_metageneration_match),
        ("ifMetagenerationNotMatch", r.if_metageneration_not_match),
        ("ifSourceGenerationMatch", r.if_source_generation_match),
        (
            "ifSourceGenerationNotMatch",
            r.if_source_generation_not_match,
        ),
        (
            "ifSourceMetagenerationMatch",
            r.if_source_metageneration_match,
        ),
        (
            "ifSourceMetagenerationNotMatch",
            r.if_source_metageneration_not_match,
        ),
    ] {
        if let Some(v) = v {
            parameters.insert(key.to_owned(), json!(v.to_string()));
        }
    }
    parameters
}

/// Source and destination of a rewrite; names a request omits come from its token's binding.
struct RewriteTarget<'a> {
    source_bucket: &'a str,
    source_object: &'a str,
    destination_bucket: &'a str,
    destination_object: &'a str,
}

impl<'a> RewriteTarget<'a> {
    fn resolve(
        r: &'a pb::RewriteObjectRequest,
        binding: Option<&'a RewriteBinding>,
    ) -> Result<Self, Status> {
        let sb = if r.source_bucket.is_empty() {
            binding.map_or("", |b| b.source_bucket.as_str())
        } else {
            names::bucket_name(&r.source_bucket)?
        };
        let sn = if r.source_object.is_empty() {
            binding.map_or("", |b| b.source_object.as_str())
        } else {
            &r.source_object
        };
        let db = if r.destination_bucket.is_empty() {
            binding.map_or("", |b| b.destination_bucket.as_str())
        } else {
            names::bucket_name(&r.destination_bucket)?
        };
        let dn = if r.destination_name.is_empty() {
            binding.map_or("", |b| b.destination_object.as_str())
        } else {
            &r.destination_name
        };
        names::object_name(&format!("projects/_/buckets/{sb}"), sn)?;
        names::object_name(&format!("projects/_/buckets/{db}"), dn)?;
        Ok(Self {
            source_bucket: sb,
            source_object: sn,
            destination_bucket: db,
            destination_object: dn,
        })
    }

    fn matches(&self, binding: &RewriteBinding) -> bool {
        [
            self.source_bucket,
            self.source_object,
            self.destination_bucket,
            self.destination_object,
        ] == [
            binding.source_bucket.as_str(),
            binding.source_object.as_str(),
            binding.destination_bucket.as_str(),
            binding.destination_object.as_str(),
        ]
    }
}

/// A rewrite request without a token, after validation and name resolution.
struct NewRewrite<'a> {
    target: RewriteTarget<'a>,
    source_generation: i64,
    max_bytes_rewritten_per_call: i64,
    source_conditions: ObjectConditions,
    destination_conditions: ObjectConditions,
    parameters: Map<String, Value>,
    metadata: Option<Value>,
}

impl Storage {
    pub(super) async fn rewrite_object(
        &self,
        request: Request<pb::RewriteObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::RewriteResponse>, Status> {
        let r = request.into_inner();
        check_rewrite_request(&r)?;
        let sc = request_conditions!(strict_conditions, source r)?;
        let dc = request_conditions!(strict_conditions, r)?;
        let body = r.destination.as_ref().map(derived_fields).transpose()?;
        if r.destination
            .as_ref()
            .is_some_and(|o| !o.name.is_empty() || !o.bucket.is_empty())
        {
            return Err(Status::invalid_argument(
                "Rewrite destination metadata must omit name and bucket; use destination_name and destination_bucket",
            ));
        }
        let parameters = rewrite_parameters(&r);
        let session = if r.rewrite_token.is_empty() {
            None
        } else {
            Some(
                self.state
                    .catalog
                    .rewrite_session(&r.rewrite_token)
                    .await
                    .map_err(rewrite_error)?,
            )
        };
        let binding = session.as_ref().map(|s| &s.binding);
        let target = RewriteTarget::resolve(&r, binding)?;
        let read = self
            .access(caller, target.source_bucket, auth::OBJECTS_GET)
            .await?;
        let create = self
            .access(caller, target.destination_bucket, auth::OBJECTS_CREATE)
            .await?;
        let mut owner = None;
        let token = if let Some(binding) = binding {
            if !target.matches(binding)
                || parameters
                    .iter()
                    .any(|(k, v)| binding.parameters.get(k) != Some(v))
                || body
                    .as_ref()
                    .is_some_and(|v| binding.metadata.as_ref() != Some(v))
            {
                return Err(Status::invalid_argument(
                    "rewrite_token does not match the request; repeat the original parameters",
                ));
            }
            r.rewrite_token.clone()
        } else {
            let new = NewRewrite {
                target,
                source_generation: r.source_generation,
                max_bytes_rewritten_per_call: r.max_bytes_rewritten_per_call,
                source_conditions: sc,
                destination_conditions: dc,
                parameters,
                metadata: body,
            };
            let (token, guard) = self.begin_rewrite(new, &read, &create).await?;
            owner = Some(guard);
            token
        };
        let progress=self.state.catalog.continue_rewrite(&token,read,create,|record|Ok(json!({"proto":prost::Message::encode_to_vec(&convert::object(record).map_err(|_|ObjectError::InvalidMetadata)?)}))).await.map_err(rewrite_error)?;
        let resource = progress
            .resource
            .map(|v| {
                let bytes: Vec<u8> = serde_json::from_value(v["proto"].clone())
                    .map_err(|_| Status::internal("Invalid stored rewrite response"))?;
                <pb::Object as prost::Message>::decode(bytes.as_slice())
                    .map_err(|_| Status::internal("Invalid stored rewrite response"))
            })
            .transpose()?;
        if let Some(owner) = &mut owner {
            owner.disarm();
        }
        Ok(Response::new(pb::RewriteResponse {
            total_bytes_rewritten: progress.offset as i64,
            object_size: progress.total as i64,
            done: progress.token.is_none(),
            rewrite_token: progress.token.unwrap_or_default(),
            resource,
        }))
    }

    /// Resolves the source and destination of a rewrite without a token and opens its session.
    async fn begin_rewrite(
        &self,
        new: NewRewrite<'_>,
        read: &BucketAccess,
        create: &BucketAccess,
    ) -> Result<(String, DerivedRequestGuard), Status> {
        let NewRewrite {
            target:
                RewriteTarget {
                    source_bucket: sb,
                    source_object: sn,
                    destination_bucket: db,
                    destination_object: dn,
                },
            source_generation,
            max_bytes_rewritten_per_call,
            source_conditions: sc,
            destination_conditions: dc,
            parameters,
            metadata: body,
        } = new;
        let target = ObjectTarget::new(
            sb,
            sn,
            (source_generation != 0).then_some(source_generation),
        );
        let source = self
            .state
            .catalog
            .resolve_derived_source(target, sc, read)
            .await
            .map_err(error)?;
        let bytes = body.as_ref().map(Value::to_string).unwrap_or_default();
        let (metadata, class, _) =
            json::derived_metadata(bytes.as_bytes(), &source.object).map_err(error)?;
        self.state
            .catalog
            .check_upload(db, dn, dc, create)
            .await
            .map_err(error)?;
        let source_bucket = self
            .state
            .catalog
            .get_bucket(sb, &BucketAccess::Permissive)
            .await
            .map_err(error)?
            .ok_or_else(|| Status::not_found("Source bucket not found"))?;
        let destination_bucket = self
            .state
            .catalog
            .get_bucket(db, &BucketAccess::Permissive)
            .await
            .map_err(error)?
            .ok_or_else(|| Status::not_found("Destination bucket not found"))?;
        let multiple = source_bucket.location != destination_bucket.location
            || class
                .as_deref()
                .unwrap_or(&destination_bucket.storage_class)
                != source.object.storage_class;
        let budget = if multiple {
            if max_bytes_rewritten_per_call == 0 {
                8 * 1024 * 1024
            } else {
                max_bytes_rewritten_per_call as u64
            }
        } else {
            u64::MAX
        };
        let content_type = metadata
            .content_type
            .as_ref()
            .and_then(|v| v.clone())
            .unwrap_or_else(|| "application/octet-stream".into());
        let destination = DerivedDestination {
            bucket: db.into(),
            name: dn.into(),
            content_type,
            metadata,
            storage_class: class,
            expected: ExpectedHashes {
                md5: Some(source.object.md5),
                crc32c: Some(source.object.crc32c),
            },
            conditions: dc,
            access: create.clone(),
            component_count: source.object.component_count,
        };
        let start = RewriteStart {
            binding: RewriteBinding {
                source_bucket: sb.into(),
                source_object: sn.into(),
                source_generation: source.object.generation,
                destination_bucket: db.into(),
                destination_object: dn.into(),
                parameters,
                metadata: body,
            },
            destination,
            source_conditions: sc,
            source_metageneration: source.object.metageneration,
            source_access: read.clone(),
            source_key_sha256: None,
            budget,
        };
        self.state
            .catalog
            .begin_rewrite_session(vec![source.whole()], self.max_object_bytes, start)
            .await
            .map_err(rewrite_error)
    }
    pub(super) async fn lock_bucket_retention_policy(
        &self,
        request: Request<pb::LockBucketRetentionPolicyRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let r = request.into_inner();
        let bucket = names::bucket_name(&r.bucket)?;
        strict_conditions(None, None, Some(r.if_metageneration_match), None)?;
        let access = self.access(caller, bucket, auth::BUCKETS_UPDATE).await?;
        let record = self
            .state
            .catalog
            .patch_bucket(
                bucket,
                BucketMetadataUpdate {
                    lock_retention_policy: true,
                    ..Default::default()
                },
                Some(r.if_metageneration_match),
                None,
                &access,
            )
            .await
            .map_err(error)?;
        let mut result = convert::bucket(&record);
        self.add_bucket_acl(caller, &mut result).await?;
        Ok(Response::new(result))
    }
    pub(super) async fn get_iam_policy(
        &self,
        request: Request<iam::GetIamPolicyRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::Policy>, Status> {
        let r = request.into_inner();
        let bucket = iam_bucket(&r.resource)?;
        if r.options
            .is_some_and(|o| !matches!(o.requested_policy_version, 0 | 1 | 3))
        {
            return Err(Status::invalid_argument(
                "requested_policy_version must be 0, 1 or 3",
            ));
        }
        let access = self.access(caller, bucket, auth::BUCKETS_GET_IAM).await?;
        Ok(Response::new(policy(
            self.state
                .catalog
                .get_policy(bucket, &access)
                .await
                .map_err(error)?,
        )?))
    }
    pub(super) async fn set_iam_policy(
        &self,
        request: Request<iam::SetIamPolicyRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::Policy>, Status> {
        let r = request.into_inner();
        let bucket = iam_bucket(&r.resource)?;
        if let Some(mask) = r.update_mask
            && !mask.paths.is_empty()
            && !(mask.paths.len() == 2
                && mask.paths.iter().any(|p| p == "bindings")
                && mask.paths.iter().any(|p| p == "etag"))
        {
            return Err(unsupported(
                "SetIamPolicy update_mask; omit the mask to replace bindings and etag",
            ));
        }
        let access = self.access(caller, bucket, auth::BUCKETS_SET_IAM).await?;
        let p = r.policy.ok_or_else(|| {
            Status::invalid_argument(
                "policy is required; supply bindings, for example an empty list",
            )
        })?;
        if !p.audit_configs.is_empty() || p.bindings.iter().any(|b| b.condition.is_some()) {
            return Err(unsupported(
                "IAM conditions or audit configs; supply unconditional bindings",
            ));
        }
        let mut v = json!({"version":p.version,"bindings":p.bindings.iter().map(|b|json!({"role":b.role,"members":b.members})).collect::<Vec<_>>()});
        if !p.etag.is_empty() {
            let bytes: [u8; 8] = p.etag.try_into().map_err(|_| {
                Status::failed_precondition(
                    "Policy etag does not match; read the current policy before updating",
                )
            })?;
            v["etag"] = json::policy_etag(i64::from_be_bytes(bytes)).into();
        }
        let p = json::parse_policy(v.to_string().as_bytes()).map_err(error)?;
        Ok(Response::new(policy(
            self.state
                .catalog
                .set_policy(bucket, p, &access)
                .await
                .map_err(error)?,
        )?))
    }
    pub(super) async fn test_iam_permissions(
        &self,
        request: Request<iam::TestIamPermissionsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::TestIamPermissionsResponse>, Status> {
        let r = request.into_inner();
        let bucket = iam_bucket(&r.resource)?;
        let query = r
            .permissions
            .iter()
            .map(|p| format!("permissions={}", auth::signature::encode_query_component(p)))
            .collect::<Vec<_>>()
            .join("&");
        let (requested, _) = json::test_permissions_query(Some(&query)).map_err(error)?;
        self.state
            .catalog
            .get_policy(bucket, &BucketAccess::Permissive)
            .await
            .map_err(error)?;
        let mut permissions = Vec::new();
        for p in requested {
            match authorize_bucket(&self.state, caller, &p, bucket).await {
                Ok(access)
                    if self
                        .state
                        .catalog
                        .has_bucket_permission(bucket, &access)
                        .await
                        .map_err(error)? =>
                {
                    permissions.push(p)
                }
                Ok(_) => {}
                Err(e) if e.status == hyper::StatusCode::FORBIDDEN => {}
                Err(e) => return Err(error(e)),
            }
        }
        Ok(Response::new(iam::TestIamPermissionsResponse {
            permissions,
        }))
    }
    pub(super) async fn update_bucket(
        &self,
        request: Request<pb::UpdateBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let r = request.into_inner();
        let b = r
            .bucket
            .ok_or_else(|| Status::invalid_argument("bucket is required; supply bucket.name"))?;
        let bucket = names::bucket_name(&b.name)?;
        if !r.predefined_acl.is_empty() || !r.predefined_default_object_acl.is_empty() {
            return Err(unsupported("predefined bucket ACLs; omit predefined_acl"));
        }
        strict_conditions(
            None,
            None,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        let paths = mask(r.update_mask.as_ref())?;
        let mut body = bucket_patch(&b, paths)?;
        let access = self.access(caller, bucket, auth::BUCKETS_UPDATE).await?;
        let mut mm = r.if_metageneration_match;
        if paths.iter().any(|p| p == "labels") && !b.labels.is_empty() {
            let current = self
                .state
                .catalog
                .get_bucket(bucket, &access)
                .await
                .map_err(error)?
                .ok_or_else(|| Status::not_found("Bucket not found"))?;
            for key in current.labels.keys().filter(|k| !b.labels.contains_key(*k)) {
                body["labels"][key] = Value::Null;
            }
            mm = Some(mm.unwrap_or(current.metageneration));
        }
        let update = json::parse_bucket_patch(
            body.to_string().as_bytes(),
            crate::config::CompatProfile::Native,
        )
        .map_err(error)?;
        let record = self
            .state
            .catalog
            .patch_bucket(bucket, update, mm, r.if_metageneration_not_match, &access)
            .await
            .map_err(error)?;
        let mut result = convert::bucket(&record);
        self.add_bucket_acl(caller, &mut result).await?;
        Ok(Response::new(result))
    }
    pub(super) async fn update_object(
        &self,
        request: Request<pb::UpdateObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(&r.common_object_request_params)?;
        if !r.predefined_acl.is_empty() {
            return Err(unsupported(
                "UpdateObject predefined_acl; omit predefined_acl",
            ));
        }
        let retention_override = if r.override_unlocked_retention {
            crate::object::retention::UnlockedOverride::Requested
        } else {
            crate::object::retention::UnlockedOverride::NotRequested
        };
        let o = r.object.ok_or_else(|| {
            Status::invalid_argument("object is required; supply object.bucket and object.name")
        })?;
        let (bucket, name) = names::object_name(&o.bucket, &o.name)?;
        if o.generation < 0 {
            return Err(Status::invalid_argument(
                "generation must be positive or omitted",
            ));
        }
        let paths = mask(r.update_mask.as_ref())?;
        let mut body = object_patch(&o, paths)?;
        let access = self.access(caller, bucket, auth::OBJECTS_UPDATE).await?;
        let mut c = request_conditions!(strict_conditions, r)?;
        if paths.iter().any(|p| p == "metadata") && !o.metadata.is_empty() {
            let target =
                ObjectTarget::new(bucket, name, (o.generation != 0).then_some(o.generation));
            let current = self
                .state
                .catalog
                .get_object(target, c, &access)
                .await
                .map_err(|e| error(JsonError::from(e).naming_object(bucket, name)))?;
            for key in current
                .metadata
                .keys()
                .filter(|k| !o.metadata.contains_key(*k))
            {
                body["metadata"][key] = Value::Null;
            }
            c.if_metageneration_match =
                Some(c.if_metageneration_match.unwrap_or(current.metageneration));
            c.if_generation_match = Some(c.if_generation_match.unwrap_or(current.generation));
        }
        // The mutation parser also accepts `acl` and `retention`, which only their own paths
        // write; retention changes then need the same extra permissions as JSON.
        let mut parsed = json::parse_object_metadata_mutation(
            body.to_string().as_bytes(),
            json::WriteMode::Patch,
        )
        .map_err(error)?;
        parsed.update.validate().map_err(error)?;
        crate::server::authorize_retention_update(
            &self.state,
            &access,
            bucket,
            &mut parsed.update,
            retention_override,
        )
        .await
        .map_err(|e| access_status(e, caller))?;
        let record = self
            .state
            .catalog
            .update_object_metadata(
                ObjectTarget::new(bucket, name, (o.generation != 0).then_some(o.generation)),
                c,
                parsed.update,
                |_| Ok(()),
                &access,
            )
            .await
            .map_err(|e| error(JsonError::from(e).naming_object(bucket, name)))?;
        let mut object = convert::object(&record)?;
        self.add_written_owner_fields(caller, bucket, &mut object)
            .await?;
        Ok(Response::new(object))
    }
    pub(super) async fn compose_object(
        &self,
        request: Request<pb::ComposeObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(&r.common_object_request_params)?;
        if !r.destination_predefined_acl.is_empty()
            || !r.kms_key.is_empty()
            || r.object_checksums
                .as_ref()
                .is_some_and(|c| c.crc32c.is_some() || !c.md5_hash.is_empty())
        {
            return Err(unsupported(
                "ComposeObject encryption/ACL/checksums; omit these options",
            ));
        }
        let o = r.destination.ok_or_else(|| {
            Status::invalid_argument("destination is required; supply bucket and name")
        })?;
        let (bucket, name) = names::object_name(&o.bucket, &o.name)?;
        let access = self.access(caller, bucket, auth::OBJECTS_CREATE).await?;
        let body = derived_fields(&o)?;
        let sources = r
            .source_objects
            .iter()
            .map(|s| {
                names::object_name(&o.bucket, &s.name)?;
                if s.generation < 0 {
                    return Err(Status::invalid_argument(
                        "source generation must be positive or omitted",
                    ));
                }
                let gm = s
                    .object_preconditions
                    .as_ref()
                    .and_then(|p| p.if_generation_match);
                strict_conditions(gm, None, None, None)?;
                Ok(ComposeSource {
                    name: s.name.clone(),
                    generation: (s.generation != 0).then_some(s.generation),
                    if_generation_match: gm,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let parsed =
            json::parse_object_metadata(body.to_string().as_bytes(), json::WriteMode::Replace)
                .map_err(error)?;
        if !parsed.unchanged.is_empty() || o.temporary_hold || o.custom_time.is_some() {
            return Err(unsupported(
                "ComposeObject metadata; omit storage_class, temporary_hold and custom_time",
            ));
        }
        parsed.update.validate().map_err(error)?;
        let destination = DerivedDestination {
            bucket: bucket.into(),
            name: name.into(),
            content_type: if o.content_type.is_empty() {
                "application/octet-stream".into()
            } else {
                o.content_type
            },
            metadata: parsed.update,
            storage_class: None,
            expected: ExpectedHashes::default(),
            conditions: strict_conditions(
                r.if_generation_match,
                None,
                r.if_metageneration_match,
                None,
            )?,
            access,
            component_count: None,
        };
        let record = self
            .state
            .catalog
            .compose_object(
                destination,
                sources,
                if r.delete_source_objects.unwrap_or(false) {
                    crate::storage::SourceDisposal::Delete
                } else {
                    crate::storage::SourceDisposal::Keep
                },
                self.max_object_bytes,
            )
            .await
            .map_err(error)?;
        Ok(Response::new(convert::object(&record)?))
    }
    pub(super) async fn move_object(
        &self,
        request: Request<pb::MoveObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        let (bucket, source) = names::object_name(&r.bucket, &r.source_object)?;
        names::object_name(&r.bucket, &r.destination_object)?;
        let read = self.access(caller, bucket, auth::OBJECTS_GET).await?;
        let delete = self.access(caller, bucket, auth::OBJECTS_DELETE).await?;
        let create = self.access(caller, bucket, auth::OBJECTS_CREATE).await?;
        let sc = request_conditions!(strict_conditions, source r)?;
        let dc = request_conditions!(strict_conditions, r)?;
        let names = MoveNames {
            bucket: bucket.into(),
            source: source.into(),
            destination: r.destination_object.clone(),
        };
        let conditions = MoveConditions {
            source: sc,
            destination: dc,
        };
        let record = self
            .state
            .catalog
            .move_object(
                names,
                conditions,
                MoveAccess {
                    source_read: &read,
                    source_delete: &delete,
                    destination_create: &create,
                },
            )
            .await
            .map_err(error)?;
        Ok(Response::new(convert::object(&record)?))
    }
    pub(super) async fn restore_object(
        &self,
        request: Request<pb::RestoreObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(&r.common_object_request_params)?;
        if r.copy_source_acl == Some(true) || !r.restore_token.is_empty() {
            return Err(unsupported(
                "RestoreObject ACL/token; use copy_source_acl=false and omit restore_token",
            ));
        }
        if r.generation <= 0 {
            return Err(Status::invalid_argument(
                "generation is required; select a positive soft-deleted generation",
            ));
        }
        let (bucket, name) = names::object_name(&r.bucket, &r.object)?;
        let restore = self.access(caller, bucket, auth::OBJECTS_RESTORE).await?;
        let create = self.access(caller, bucket, auth::OBJECTS_CREATE).await?;
        let c = request_conditions!(strict_conditions, r)?;
        let target = ObjectTarget::new(bucket, name, r.generation);
        let access = RestoreAccess {
            restore: &restore,
            create: &create,
        };
        let record = self
            .state
            .catalog
            .restore_object(target, c, access)
            .await
            .map_err(error)?;
        Ok(Response::new(convert::object(&record)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_fixtures::{PERMISSIVE, live};
    use crate::{
        auth::Auth, clock::SystemClock, object::UploadChunk, server::State, storage::Catalog,
        test_support::TestDir,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    use tonic::Code;

    async fn setup() -> (Storage, TestDir) {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let now = std::time::SystemTime::now();
        for (name, location) in [("update-copy-bucket", "US"), ("update-copy-other", "EU")] {
            let bytes=json!({"name":name,"location":location,"labels":{"keep":"yes","remove":"yes"},"softDeletePolicy":{"retentionDurationSeconds":"604800"}}).to_string();
            catalog
                .insert_bucket(json::parse_insert(bytes.as_bytes(), "local-project", now).unwrap())
                .await
                .unwrap();
        }
        (
            Storage {
                state: Arc::new(State {
                    catalog,
                    ready: AtomicBool::new(true),
                    clock: Arc::new(SystemClock),
                    api_base_url: "http://localhost:4443".into(),
                    default_location: "US".into(),
                    auth: Auth::permissive(),
                }),
                max_object_bytes: 8 * 1024 * 1024,
            },
            dir,
        )
    }

    fn update_mask(paths: &[&str]) -> Option<prost_types::FieldMask> {
        Some(prost_types::FieldMask {
            paths: paths.iter().map(|p| p.to_string()).collect(),
        })
    }

    async fn upload(storage: &Storage, name: &str, length: usize) -> crate::object::ObjectRecord {
        upload_to(storage, "update-copy-bucket", name, length).await
    }

    async fn upload_to(
        storage: &Storage,
        bucket: &str,
        name: &str,
        length: usize,
    ) -> crate::object::ObjectRecord {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(Ok(UploadChunk::Data(bytes::Bytes::from(vec![42; length]))))
            .await
            .unwrap();
        tx.send(Ok(UploadChunk::End)).await.unwrap();
        drop(tx);
        storage
            .state
            .catalog
            .put_object(
                bucket,
                name,
                "text/plain".into(),
                Default::default(),
                Default::default(),
                storage.max_object_bytes,
                rx,
            )
            .await
            .unwrap()
    }

    #[test]
    fn grpc_bucket_masks_preserve_resets_and_omitted_parent_fields() {
        let bucket = pb::Bucket::default();
        let paths = [
            "autoclass",
            "versioning",
            "retention_policy",
            "soft_delete_policy",
            "cors",
            "iam_config.public_access_prevention",
            "iam_config.uniform_bucket_level_access",
            "iam_config",
        ]
        .map(String::from);
        assert_eq!(
            bucket_patch(&bucket, &paths).unwrap(),
            json!({
                "autoclass": {"enabled": false},
                "versioning": {"enabled": false},
                "retentionPolicy": null,
                "softDeletePolicy": {"retentionDurationSeconds": "0"},
                "cors": [],
                "iamConfiguration": {
                    "publicAccessPrevention": "inherited",
                    "uniformBucketLevelAccess": {"enabled": false},
                },
            })
        );
        let create = super::super::buckets::create_fields(&bucket, "").unwrap();
        for omitted in [
            "autoclass",
            "versioning",
            "retentionPolicy",
            "softDeletePolicy",
            "iamConfiguration",
        ] {
            assert!(create.get(omitted).is_none(), "{omitted}");
        }
    }

    #[test]
    fn grpc_masks_reject_unsupported_and_accept_defaults() {
        let status = error(JsonError::new(
            hyper::StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            "Unsupported operation; use JSON instead",
        ));
        assert_eq!(status.code(), Code::Unimplemented);
        let details = <crate::grpc::generated::google::rpc::Status as prost::Message>::decode(
            status.details(),
        )
        .unwrap();
        assert_eq!(
            details.details[0].type_url,
            "type.googleapis.com/google.rpc.ErrorInfo"
        );
        assert_eq!(
            error(JsonError::new(
                hyper::StatusCode::GONE,
                "gone",
                "Expired session; restart the request"
            ))
            .code(),
            Code::FailedPrecondition
        );
        for path in [
            "*",
            "acl.entity",
            "owner",
            "logging",
            "versioning.unknown",
            "iam_config.unknown",
        ] {
            assert_eq!(
                bucket_patch(&pb::Bucket::default(), &[path.into()])
                    .unwrap_err()
                    .code(),
                Code::Unimplemented
            );
        }
        for path in [
            "*",
            "acl.entity",
            "owner",
            "contexts.custom",
            "retention.mode",
        ] {
            assert_eq!(
                object_patch(&pb::Object::default(), &[path.into()])
                    .unwrap_err()
                    .code(),
                Code::Unimplemented
            );
        }
        assert_eq!(mask(None).unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(
            mask(Some(&prost_types::FieldMask::default()))
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        assert!(common(&Some(pb::CommonObjectRequestParams::default())).is_ok());
        assert_eq!(
            strict_conditions(Some(-1), None, None, None)
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        assert_eq!(
            strict_conditions(None, None, Some(0), None)
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        let b = pb::Bucket {
            labels: [
                ("keep".into(), "changed".into()),
                ("ignored".into(), "no".into()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(
            bucket_patch(&b, &["labels.keep".into()]).unwrap(),
            json!({"labels":{"keep":"changed"}})
        );
        assert_eq!(
            object_patch(
                &pb::Object::default(),
                &["temporary_hold".into(), "event_based_hold".into()]
            )
            .unwrap(),
            json!({"temporaryHold":false,"eventBasedHold":false})
        );
    }

    #[tokio::test]
    async fn grpc_update_bucket_autoclass_masks() {
        let autoclass = |enabled, class: Option<&str>| pb::Bucket {
            autoclass: Some(pb::bucket::Autoclass {
                enabled,
                terminal_storage_class: class.map(str::to_owned),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            bucket_patch(&autoclass(true, Some("ARCHIVE")), &["autoclass".into()]).unwrap(),
            json!({"autoclass":{"enabled":true,"terminalStorageClass":"ARCHIVE"}})
        );
        assert_eq!(
            bucket_patch(
                &autoclass(true, Some("ARCHIVE")),
                &["autoclass.terminal_storage_class".into()]
            )
            .unwrap(),
            json!({"autoclass":{"terminalStorageClass":"ARCHIVE"}})
        );
        assert_eq!(
            bucket_patch(&pb::Bucket::default(), &["autoclass.enabled".into()]).unwrap(),
            json!({"autoclass":{"enabled":false}})
        );
        let (s, _dir) = setup().await;
        let update = |bucket: pb::Bucket, paths: &[&str]| pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: "projects/_/buckets/update-copy-bucket".into(),
                ..bucket
            }),
            update_mask: update_mask(paths),
            ..Default::default()
        };
        let enabled = s
            .update_bucket(
                Request::new(update(autoclass(true, None), &["autoclass"])),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let config = enabled.autoclass.unwrap();
        assert!(config.enabled);
        assert_eq!(config.terminal_storage_class.as_deref(), Some("NEARLINE"));
        assert!(
            config.toggle_time.is_some() && config.terminal_storage_class_update_time.is_some()
        );
        assert_eq!(enabled.storage_class, "STANDARD");
        let invalid = update(autoclass(true, Some("COLDLINE")), &["autoclass"]);
        assert_eq!(
            s.update_bucket(Request::new(invalid), None)
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        let disabled = s
            .update_bucket(
                Request::new(update(autoclass(false, None), &["autoclass.enabled"])),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let config = disabled.autoclass.unwrap();
        assert!(!config.enabled);
        assert_eq!(config.terminal_storage_class, None);
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_iam_config_updates_preserve_other_setting() {
        assert_eq!(
            bucket_patch(&pb::Bucket::default(), &["iam_config".into()]).unwrap(),
            json!({})
        );
        assert_eq!(
            bucket_patch(
                &pb::Bucket::default(),
                &["iam_config.uniform_bucket_level_access.enabled".into()]
            )
            .unwrap(),
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}})
        );
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        s.state
            .catalog
            .patch_bucket(
                "update-copy-bucket",
                BucketMetadataUpdate {
                    public_access_prevention_enforced: Some(true),
                    ..Default::default()
                },
                None,
                None,
                PERMISSIVE,
            )
            .await
            .unwrap();
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                iam_config: Some(pb::bucket::IamConfig {
                    uniform_bucket_level_access: Some(
                        pb::bucket::iam_config::UniformBucketLevelAccess {
                            enabled: true,
                            ..Default::default()
                        },
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: update_mask(&["iam_config"]),
            ..Default::default()
        };
        let b = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let i = b.iam_config.unwrap();
        assert_eq!(i.public_access_prevention, "enforced");
        assert!(i.uniform_bucket_level_access.unwrap().enabled);
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                iam_config: Some(pb::bucket::IamConfig {
                    public_access_prevention: "inherited".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: update_mask(&["iam_config"]),
            ..Default::default()
        };
        let b = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let i = b.iam_config.unwrap();
        assert_eq!(i.public_access_prevention, "inherited");
        assert!(i.uniform_bucket_level_access.unwrap().enabled);
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_update_object_retention() {
        let (s, _dir) = setup().await;
        let mut record = json::parse_insert(
            br#"{"name":"retention-objects"}"#,
            "local-project",
            s.state.clock.now(),
        )
        .unwrap();
        record.object_retention_enabled = true;
        s.state.catalog.insert_bucket(record).await.unwrap();
        upload_to(&s, "retention-objects", "kept", 3).await;
        upload(&s, "plain", 3).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let request =
            |bucket: &str, name: &str, seconds: Option<i64>, force: bool| pb::UpdateObjectRequest {
                object: Some(pb::Object {
                    bucket: format!("projects/_/buckets/{bucket}"),
                    name: name.into(),
                    retention: seconds.map(|seconds| pb::object::Retention {
                        mode: pb::object::retention::Mode::Unlocked as i32,
                        retain_until_time: Some(prost_types::Timestamp {
                            seconds: now + seconds,
                            nanos: 0,
                        }),
                    }),
                    ..Default::default()
                }),
                update_mask: update_mask(&["retention"]),
                override_unlocked_retention: force,
                ..Default::default()
            };
        assert_eq!(
            s.update_object(
                Request::new(request("update-copy-bucket", "plain", Some(3600), false)),
                None
            )
            .await
            .unwrap_err()
            .code(),
            Code::InvalidArgument
        );
        let object = s
            .update_object(
                Request::new(request("retention-objects", "kept", Some(3600), false)),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let retention = object.retention.unwrap();
        assert_eq!(retention.mode, pb::object::retention::Mode::Unlocked as i32);
        for invalid in [
            pb::object::retention::Mode::Unspecified as i32,
            pb::object::retention::Mode::Unlocked as i32,
        ] {
            let mut bad = request("retention-objects", "kept", Some(7200), true);
            let retention = bad.object.as_mut().unwrap().retention.as_mut().unwrap();
            retention.mode = invalid;
            if invalid != pb::object::retention::Mode::Unspecified as i32 {
                retention.retain_until_time = None;
            }
            assert_eq!(
                s.update_object(Request::new(bad), None)
                    .await
                    .unwrap_err()
                    .code(),
                Code::InvalidArgument
            );
        }
        assert_eq!(retention.retain_until_time.unwrap().seconds, now + 3600);
        // Deleting a retained object is 403 in JSON, so PERMISSION_DENIED with the same text
        // (observed 2026-10-31).
        let denied = s
            .delete_object(
                Request::new(pb::DeleteObjectRequest {
                    bucket: "projects/_/buckets/retention-objects".into(),
                    object: "kept".into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.code(), Code::PermissionDenied);
        assert!(
            denied.message().starts_with(
                "Object 'retention-objects/kept' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until "
            ),
            "{}",
            denied.message()
        );
        // Shortening or removing an Unlocked configuration requires the override and is 403 in
        // JSON, so PERMISSION_DENIED here with the same text (observed 2026-10-31).
        for seconds in [Some(60), None] {
            let denied = s
                .update_object(
                    Request::new(request("retention-objects", "kept", seconds, false)),
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(denied.code(), Code::PermissionDenied, "{seconds:?}");
            assert_eq!(
                denied.message(),
                crate::object::UNLOCKED_RETENTION_OVERRIDE_MESSAGE
            );
        }
        let current = s
            .state
            .catalog
            .get_object(
                live("retention-objects", "kept"),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(
            convert::object(&current)
                .unwrap()
                .retention
                .unwrap()
                .retain_until_time
                .unwrap()
                .seconds,
            now + 3600
        );
        let shortened = s
            .update_object(
                Request::new(request("retention-objects", "kept", Some(60), true)),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            shortened
                .retention
                .unwrap()
                .retain_until_time
                .unwrap()
                .seconds,
            now + 60
        );
        let removed = s
            .update_object(
                Request::new(request("retention-objects", "kept", None, true)),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(removed.retention.is_none());
        s.state.catalog.close().await.unwrap();
    }

    /// Cloud Storage returned `retention.retain_until_time` with its nanoseconds over gRPC and
    /// `retention_expire_time` at millisecond precision (observed 2026-10-31).
    #[tokio::test]
    async fn grpc_retention_keeps_nanoseconds_and_expire_time_is_milliseconds() {
        // Not a multiple of 100 ns, which a platform flooring a SystemTime would lose.
        const FINE_NANOS: i32 = 123_456_789;
        let (s, _dir) = setup().await;
        let mut record = json::parse_insert(
            br#"{"name":"retention-ns"}"#,
            "local-project",
            s.state.clock.now(),
        )
        .unwrap();
        record.object_retention_enabled = true;
        s.state.catalog.insert_bucket(record).await.unwrap();
        upload_to(&s, "retention-ns", "kept", 3).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let object = s
            .update_object(
                Request::new(pb::UpdateObjectRequest {
                    object: Some(pb::Object {
                        bucket: "projects/_/buckets/retention-ns".into(),
                        name: "kept".into(),
                        retention: Some(pb::object::Retention {
                            mode: pb::object::retention::Mode::Unlocked as i32,
                            retain_until_time: Some(prost_types::Timestamp {
                                seconds: now + 3600,
                                nanos: FINE_NANOS,
                            }),
                        }),
                        ..Default::default()
                    }),
                    update_mask: update_mask(&["retention"]),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let until = object.retention.unwrap().retain_until_time.unwrap();
        assert_eq!((until.seconds, until.nanos), (now + 3600, FINE_NANOS));
        let expire = object.retention_expire_time.unwrap();
        assert_eq!((expire.seconds, expire.nanos), (now + 3600, 123_000_000));
        s.state.catalog.close().await.unwrap();
    }

    /// With a bucket retention policy, `retention_expire_time` is also cut to the millisecond
    /// (observed 2026-10-31) while the denial text keeps the microsecond creation time.
    #[tokio::test]
    async fn grpc_bucket_policy_expire_time_is_milliseconds() {
        let (s, _dir) = setup().await;
        let bytes = br#"{"name":"policy-ms","retentionPolicy":{"retentionPeriod":"3600"}}"#;
        let record = json::parse_insert(bytes, "local-project", s.state.clock.now()).unwrap();
        s.state.catalog.insert_bucket(record).await.unwrap();
        let object = upload_to(&s, "policy-ms", "kept", 3).await;
        let expire = convert::object(&object)
            .unwrap()
            .retention_expire_time
            .unwrap();
        assert_eq!(expire.nanos % 1_000_000, 0);
        let created = object
            .time_created
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!(expire.seconds, created.as_secs() as i64 + 3600);
        assert_eq!(expire.nanos as u32, created.subsec_millis() * 1_000_000);
        let denied = s
            .delete_object(
                Request::new(pb::DeleteObjectRequest {
                    bucket: "projects/_/buckets/policy-ms".into(),
                    object: "kept".into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.code(), Code::PermissionDenied);
        assert_eq!(
            denied.message(),
            format!(
                "Object 'policy-ms/kept' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until {}",
                crate::object::retention::pacific_rfc3339(
                    crate::object::unix_nanos::UnixNanos::from(
                        object.time_created + std::time::Duration::from_secs(3600)
                    )
                )
            )
        );
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_list_soft_deleted_objects() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let deleted = upload(&s, "gone", 3).await;
        upload(&s, "live", 3).await;
        s.delete_object(
            Request::new(pb::DeleteObjectRequest {
                bucket: resource.into(),
                object: "gone".into(),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
        let list = |soft_deleted: bool, versions: bool| pb::ListObjectsRequest {
            parent: resource.into(),
            soft_deleted,
            versions,
            ..Default::default()
        };
        let live = s
            .list_objects(Request::new(list(false, false)), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            live.objects
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>(),
            ["live"]
        );
        let soft = s
            .list_objects(Request::new(list(true, false)), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(soft.objects.len(), 1);
        assert_eq!(soft.objects[0].name, "gone");
        assert_eq!(soft.objects[0].generation, deleted.generation);
        assert!(soft.objects[0].soft_delete_time.is_some());
        assert!(soft.objects[0].hard_delete_time.is_some());
        // The JSON listing rejects softDeleted with versions; gRPC shares that rule.
        assert_eq!(
            s.list_objects(Request::new(list(true, true)), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_list_folders_as_prefixes_follows_the_json_rule() {
        let (s, _dir) = setup().await;
        upload(&s, "top", 3).await;
        upload(&s, "dir/nested", 3).await;
        let list = |include_folders_as_prefixes: bool, delimiter: &str| pb::ListObjectsRequest {
            parent: "projects/_/buckets/update-copy-bucket".into(),
            delimiter: delimiter.into(),
            include_folders_as_prefixes,
            ..Default::default()
        };
        // No local bucket holds folders or managed folders, so the flag changes nothing.
        let plain = s
            .list_objects(Request::new(list(false, "/")), None)
            .await
            .unwrap()
            .into_inner();
        let folders = s
            .list_objects(Request::new(list(true, "/")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(folders, plain);
        assert_eq!(folders.prefixes, ["dir/"]);
        assert_eq!(folders.objects.len(), 1);
        for delimiter in ["", "-"] {
            let error = s
                .list_objects(Request::new(list(true, delimiter)), None)
                .await
                .unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument, "{delimiter:?}");
            assert!(error.message().contains("delimiter=/"), "{error:?}");
        }
        assert_eq!(
            s.list_objects(
                Request::new(pb::ListObjectsRequest {
                    filter: "contexts.\"k\"=\"v\"".into(),
                    ..list(false, "/")
                }),
                None
            )
            .await
            .unwrap_err()
            .code(),
            Code::Unimplemented
        );
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_list_soft_deleted_objects_strict() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog
            .insert_bucket(
                json::parse_insert(
                    br#"{"name":"update-copy-bucket","softDeletePolicy":{"retentionDurationSeconds":"604800"}}"#,
                    "p",
                    std::time::SystemTime::now(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let s = Storage {
            state: Arc::new(State {
                catalog,
                ready: AtomicBool::new(true),
                clock: Arc::new(SystemClock),
                api_base_url: "http://localhost:4443".into(),
                default_location: "US".into(),
                auth: Auth::strict(crate::auth::Registry::parse(br#"{"identities":[{"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"p":["roles/storage.objectViewer"]}},{"id":"none@example.invalid","tokens":["none"]}]}"#).unwrap()),
            }),
            max_object_bytes: 1024,
        };
        let caller = |token: &str| {
            let mut metadata = Request::new(());
            metadata
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
            s.authenticate(&metadata).unwrap()
        };
        let request = pb::ListObjectsRequest {
            parent: "projects/_/buckets/update-copy-bucket".into(),
            soft_deleted: true,
            ..Default::default()
        };
        s.list_objects(Request::new(request.clone()), caller("viewer"))
            .await
            .unwrap();
        assert_eq!(
            s.list_objects(Request::new(request.clone()), caller("none"))
                .await
                .unwrap_err()
                .code(),
            Code::PermissionDenied
        );
        assert_eq!(
            s.list_objects(Request::new(request), Some(Caller::Anonymous))
                .await
                .unwrap_err()
                .code(),
            Code::Unauthenticated
        );
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_update_object_retention_strict() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let mut record = json::parse_insert(
            br#"{"name":"update-copy-bucket"}"#,
            "p",
            std::time::SystemTime::now(),
        )
        .unwrap();
        record.object_retention_enabled = true;
        catalog.insert_bucket(record).await.unwrap();
        let s = Storage {
            state: Arc::new(State {
                catalog,
                ready: AtomicBool::new(true),
                clock: Arc::new(SystemClock),
                api_base_url: "http://localhost:4443".into(),
                default_location: "US".into(),
                auth: Auth::strict(crate::auth::Registry::parse(br#"{"identities":[{"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"p":["roles/storage.objectAdmin"]}},{"id":"user@example.invalid","tokens":["user"],"project_roles":{"p":["roles/storage.objectUser"]}}]}"#).unwrap()),
            }),
            max_object_bytes: 1024,
        };
        upload(&s, "object", 3).await;
        let caller = |token: &str| {
            let mut metadata = Request::new(());
            metadata
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
            s.authenticate(&metadata).unwrap()
        };
        let until = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let request = pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: "projects/_/buckets/update-copy-bucket".into(),
                name: "object".into(),
                retention: Some(pb::object::Retention {
                    mode: pb::object::retention::Mode::Unlocked as i32,
                    retain_until_time: Some(prost_types::Timestamp {
                        seconds: until,
                        nanos: 0,
                    }),
                }),
                ..Default::default()
            }),
            update_mask: update_mask(&["retention"]),
            ..Default::default()
        };
        // objectUser may update objects but not set retention.
        assert_eq!(
            s.update_object(Request::new(request.clone()), caller("user"))
                .await
                .unwrap_err()
                .code(),
            Code::PermissionDenied
        );
        assert!(
            s.state
                .catalog
                .get_object(
                    live("update-copy-bucket", "object"),
                    Default::default(),
                    PERMISSIVE
                )
                .await
                .unwrap()
                .retention
                .is_none()
        );
        s.update_object(Request::new(request), caller("admin"))
            .await
            .unwrap();
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_update_object_acl() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let source = upload(&s, "acl-object", 3).await;
        let acl_request = |entity: &str| pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: resource.into(),
                name: "acl-object".into(),
                acl: vec![pb::ObjectAccessControl {
                    entity: entity.into(),
                    role: "READER".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            update_mask: update_mask(&["acl"]),
            ..Default::default()
        };
        let object = s
            .update_object(Request::new(acl_request("allUsers")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(object.metageneration, source.metageneration + 1);
        assert!(
            object
                .acl
                .iter()
                .any(|e| e.entity == "allUsers" && e.role == "READER" && !e.etag.is_empty())
        );
        let stored = s
            .state
            .catalog
            .object_acl_projection(
                "update-copy-bucket",
                vec![("acl-object".into(), object.generation)],
                PERMISSIVE,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(stored.objects[0].iter().any(|e| e.entity == "allUsers"));
        assert_eq!(
            s.update_object(Request::new(acl_request("nobody")), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        // Uniform bucket-level access disables object ACLs, exactly as in JSON.
        s.update_bucket(
            Request::new(pb::UpdateBucketRequest {
                bucket: Some(pb::Bucket {
                    name: resource.into(),
                    iam_config: Some(pb::bucket::IamConfig {
                        uniform_bucket_level_access: Some(
                            pb::bucket::iam_config::UniformBucketLevelAccess {
                                enabled: true,
                                lock_time: None,
                            },
                        ),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                update_mask: update_mask(&["iam_config"]),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            s.update_object(Request::new(acl_request("allAuthenticatedUsers")), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_update_masks() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                labels: [("new".into(), "yes".into())].into(),
                versioning: Some(pb::bucket::Versioning { enabled: true }),
                ..Default::default()
            }),
            update_mask: update_mask(&["labels"]),
            ..Default::default()
        };
        let b = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(b.labels.len(), 1);
        assert_eq!(b.labels["new"], "yes");
        assert!(b.versioning.is_none());
        let stale = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                ..Default::default()
            }),
            if_metageneration_match: Some(1),
            update_mask: update_mask(&["labels"]),
            ..Default::default()
        };
        assert_eq!(
            s.update_bucket(Request::new(stale), None)
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
        let source = upload(&s, "object", 10).await;
        let request = pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: resource.into(),
                name: "object".into(),
                metadata: [("keep".into(), "yes".into())].into(),
                content_type: "ignored".into(),
                ..Default::default()
            }),
            update_mask: update_mask(&["metadata"]),
            if_generation_match: Some(source.generation),
            ..Default::default()
        };
        let object = s
            .update_object(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(object.content_type, "text/plain");
        assert_eq!(object.metadata["keep"], "yes");
        let request = pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: resource.into(),
                name: "object".into(),
                metadata: [("replace".into(), "yes".into())].into(),
                temporary_hold: true,
                ..Default::default()
            }),
            update_mask: update_mask(&["metadata", "temporary_hold"]),
            ..Default::default()
        };
        let object = s
            .update_object(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(object.metadata.len(), 1);
        assert!(object.temporary_hold);
        let read = s
            .state
            .catalog
            .get_object(
                live("update-copy-bucket", "object"),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(read.metadata.len(), 1);
        assert_eq!(read.metadata["replace"], "yes");
        assert_eq!(read.temporary_hold, Some(true));
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_retention_lock_and_compose_limits() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let lock = pb::LockBucketRetentionPolicyRequest {
            bucket: resource.into(),
            if_metageneration_match: 0,
        };
        assert_eq!(
            s.lock_bucket_retention_policy(Request::new(lock), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                retention_policy: Some(pb::bucket::RetentionPolicy {
                    retention_duration: Some(prost_types::Duration {
                        seconds: 3600,
                        nanos: 0,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: update_mask(&["retention_policy"]),
            ..Default::default()
        };
        let b = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let lock = pb::LockBucketRetentionPolicyRequest {
            bucket: resource.into(),
            if_metageneration_match: b.metageneration,
        };
        assert!(
            s.lock_bucket_retention_policy(Request::new(lock), None)
                .await
                .unwrap()
                .into_inner()
                .retention_policy
                .unwrap()
                .is_locked
        );
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                ..Default::default()
            }),
            update_mask: update_mask(&["retention_policy"]),
            ..Default::default()
        };
        assert_eq!(
            s.update_bucket(Request::new(request), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        let source = upload(&s, "tiny", 1).await;
        let src = pb::compose_object_request::SourceObject {
            name: "tiny".into(),
            generation: source.generation,
            ..Default::default()
        };
        let request = pb::ComposeObjectRequest {
            destination: Some(pb::Object {
                bucket: resource.into(),
                name: "thirty-two".into(),
                ..Default::default()
            }),
            source_objects: vec![src; 32],
            ..Default::default()
        };
        let composed = s
            .compose_object(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(composed.size, 32);
        assert_eq!(composed.component_count, 32);
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_rewrite_continuation_and_compose_conditions() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let source = upload(&s, "source", 3 * 1024 * 1024).await;
        let request = pb::RewriteObjectRequest {
            source_bucket: resource.into(),
            source_object: "source".into(),
            source_generation: source.generation,
            destination_bucket: "projects/_/buckets/update-copy-other".into(),
            destination_name: "target".into(),
            max_bytes_rewritten_per_call: 1024 * 1024,
            if_generation_match: Some(0),
            ..Default::default()
        };
        let first = s
            .rewrite_object(Request::new(request.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert!(!first.done);
        assert_eq!(first.total_bytes_rewritten, 1024 * 1024);
        assert!(first.resource.is_none());
        let mut mismatched = request.clone();
        mismatched.rewrite_token = first.rewrite_token.clone();
        mismatched.destination_name = "different".into();
        assert_eq!(
            s.rewrite_object(Request::new(mismatched), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        let next = pb::RewriteObjectRequest {
            rewrite_token: first.rewrite_token.clone(),
            ..Default::default()
        };
        let second = s
            .rewrite_object(Request::new(next.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second.total_bytes_rewritten, 2 * 1024 * 1024);
        let replay = s
            .rewrite_object(Request::new(next), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second, replay);
        let last = s
            .rewrite_object(
                Request::new(pb::RewriteObjectRequest {
                    rewrite_token: second.rewrite_token,
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(last.done);
        assert!(last.rewrite_token.is_empty());
        assert_eq!(last.resource.unwrap().size, 3 * 1024 * 1024);
        let destination = pb::Object {
            bucket: resource.into(),
            name: "composed".into(),
            ..Default::default()
        };
        let src = pb::compose_object_request::SourceObject {
            name: "source".into(),
            generation: source.generation,
            object_preconditions: Some(
                pb::compose_object_request::source_object::ObjectPreconditions {
                    if_generation_match: Some(source.generation + 1),
                },
            ),
        };
        let request = pb::ComposeObjectRequest {
            destination: Some(destination.clone()),
            source_objects: vec![src.clone()],
            ..Default::default()
        };
        assert_eq!(
            s.compose_object(Request::new(request), None)
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
        let src = pb::compose_object_request::SourceObject {
            object_preconditions: None,
            ..src
        };
        let too_many = pb::ComposeObjectRequest {
            destination: Some(destination.clone()),
            source_objects: vec![src.clone(); 33],
            ..Default::default()
        };
        assert_eq!(
            s.compose_object(Request::new(too_many), None)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        let request = pb::ComposeObjectRequest {
            destination: Some(destination),
            source_objects: vec![src],
            if_generation_match: Some(0),
            ..Default::default()
        };
        let joined = s
            .compose_object(Request::new(request.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(joined.component_count, 1);
        assert_eq!(joined.size, source.size as i64);
        assert_eq!(
            s.compose_object(Request::new(request), None)
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
        s.state.catalog.close().await.unwrap();
    }
}
