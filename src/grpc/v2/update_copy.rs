use super::{Storage, convert, iam, names, pb, status::unsupported};
use crate::{
    auth::{self, Caller},
    bucket::{BucketAccess, BucketMetadataUpdate},
    json::{self, JsonError},
    object::{ComposeSource, ExpectedHashes, ObjectConditions, ObjectError},
    server::authorize_bucket,
    storage::{
        DerivedDestination, DerivedRequestGuard, MoveAccess, ObjectSelector,
        rewrite::{RewriteBinding, RewriteError, RewriteStart},
    },
};
use serde_json::{Map, Value, json};
use tonic::{Request, Response, Status};

fn error(error: impl Into<JsonError>) -> Status {
    super::status::from_json(error.into())
}

/// Anonymous denials ask for credentials, matching bucket RPCs, BidiReadObject and JSON.
fn access_error(error: JsonError, caller: Option<Caller<'_>>) -> Status {
    super::status::from_json(crate::server::anonymous_error(error, caller))
}

fn conditions(
    gm: Option<i64>,
    gn: Option<i64>,
    mm: Option<i64>,
    mn: Option<i64>,
) -> Result<ObjectConditions, Status> {
    if gm.is_some_and(|v| v < 0)
        || gn.is_some_and(|v| v < 0)
        || mm.is_some_and(|v| v <= 0)
        || mn.is_some_and(|v| v <= 0)
        || (gm.is_some() && gn.is_some())
        || (mm.is_some() && mn.is_some())
    {
        return Err(Status::invalid_argument(
            "Invalid generation conditions; use one match or not-match per generation, for example if_generation_match=0",
        ));
    }
    Ok(ObjectConditions {
        if_generation_match: gm,
        if_generation_not_match: gn,
        if_metageneration_match: mm,
        if_metageneration_not_match: mn,
    })
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
    if params
        .as_ref()
        .is_some_and(|p| p != &pb::CommonObjectRequestParams::default())
    {
        return Err(unsupported(
            "customer-supplied encryption; omit common_object_request_params",
        ));
    }
    Ok(())
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
    json!({"rule":value.rule.iter().map(|r| {
        let mut v=json!({});
        if let Some(a)=&r.action {v["action"]=json!({"type":a.r#type}); if !a.storage_class.is_empty() {v["action"]["storageClass"]=a.storage_class.clone().into();}}
        if let Some(c)=&r.condition {
            let mut conditions=Map::new();
            for (key,n) in [("age",c.age_days),("numNewerVersions",c.num_newer_versions),("daysSinceCustomTime",c.days_since_custom_time),("daysSinceNoncurrentTime",c.days_since_noncurrent_time)] {if let Some(n)=n {conditions.insert(key.into(),n.into());}}
            if let Some(live)=c.is_live {conditions.insert("isLive".into(),live.into());}
            for (key,date) in [("createdBefore",&c.created_before),("customTimeBefore",&c.custom_time_before),("noncurrentTimeBefore",&c.noncurrent_time_before)] {if let Some(d)=date {conditions.insert(key.into(),format!("{:04}-{:02}-{:02}",d.year,d.month,d.day).into());}}
            for (key,values) in [("matchesStorageClass",&c.matches_storage_class),("matchesPrefix",&c.matches_prefix),("matchesSuffix",&c.matches_suffix)] {if !values.is_empty() {conditions.insert(key.into(),json!(values));}}
            v["condition"]=conditions.into();
        }
        v
    }).collect::<Vec<_>>()})
}

fn bucket_resource(record: &crate::bucket::BucketRecord) -> pb::Bucket {
    use pb::bucket::lifecycle::{
        Rule,
        rule::{Action, Condition},
    };
    let mut result = convert::bucket(record);
    let date = |v: &Value| -> Option<crate::grpc::generated::google::r#type::Date> {
        let mut parts = v.as_str()?.split('-');
        Some(crate::grpc::generated::google::r#type::Date {
            year: parts.next()?.parse().ok()?,
            month: parts.next()?.parse().ok()?,
            day: parts.next()?.parse().ok()?,
        })
    };
    let strings = |v: &Value| {
        v.as_array()
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    result.lifecycle = Some(pb::bucket::Lifecycle {
        rule: record
            .lifecycle
            .iter()
            .map(|r| {
                let c = &r["condition"];
                Rule {
                    action: Some(Action {
                        r#type: r["action"]["type"].as_str().unwrap_or_default().into(),
                        storage_class: r["action"]["storageClass"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                    }),
                    condition: Some(Condition {
                        age_days: c["age"].as_i64().map(|v| v as i32),
                        created_before: date(&c["createdBefore"]),
                        is_live: c["isLive"].as_bool(),
                        num_newer_versions: c["numNewerVersions"].as_i64().map(|v| v as i32),
                        matches_storage_class: strings(&c["matchesStorageClass"]),
                        days_since_custom_time: c["daysSinceCustomTime"].as_i64().map(|v| v as i32),
                        custom_time_before: date(&c["customTimeBefore"]),
                        days_since_noncurrent_time: c["daysSinceNoncurrentTime"]
                            .as_i64()
                            .map(|v| v as i32),
                        noncurrent_time_before: date(&c["noncurrentTimeBefore"]),
                        matches_prefix: strings(&c["matchesPrefix"]),
                        matches_suffix: strings(&c["matchesSuffix"]),
                    }),
                }
            })
            .collect(),
    });
    result
}

fn bucket_patch(b: &pb::Bucket, paths: &[String]) -> Result<Value, Status> {
    let mut body = json!({});
    for path in paths {
        match path.as_str() {
        "labels"=>body["labels"]=if b.labels.is_empty(){Value::Null}else{json!(b.labels)},
        p if p.starts_with("labels.") && p.len()>7=>{let key=&p[7..];body["labels"][key]=b.labels.get(key).map_or(Value::Null,|v|v.clone().into());}
        "storage_class"=>body["storageClass"]=b.storage_class.clone().into(),
        "autoclass"|"autoclass.enabled"|"autoclass.terminal_storage_class"=>{
            let a=b.autoclass.clone().unwrap_or_default();
            if path!="autoclass.terminal_storage_class"{body["autoclass"]["enabled"]=a.enabled.into();}
            if path!="autoclass.enabled" && let Some(class)=a.terminal_storage_class{body["autoclass"]["terminalStorageClass"]=class.into();}
        }
        "versioning"|"versioning.enabled"=>body["versioning"]=json!({"enabled":b.versioning.as_ref().is_some_and(|v|v.enabled)}),
        "default_event_based_hold"=>body["defaultEventBasedHold"]=b.default_event_based_hold.into(),
        "cors"=>body["cors"]=json!(b.cors.iter().map(|c|json!({"origin":c.origin,"method":c.method,"responseHeader":c.response_header,"maxAgeSeconds":c.max_age_seconds})).collect::<Vec<_>>()),
        "lifecycle"|"lifecycle.rule"=>body["lifecycle"]=b.lifecycle.as_ref().map_or(json!({"rule":[]}),lifecycle),
        "retention_policy"|"retention_policy.retention_duration"=>{
            body["retentionPolicy"]=b.retention_policy.as_ref().and_then(|p|p.retention_duration).map_or(Value::Null,|d|json!({"retentionPeriod":d.seconds.to_string()}));
            if b.retention_policy.as_ref().is_some_and(|p|p.is_locked){return Err(unsupported("retention_policy.is_locked; use LockBucketRetentionPolicy"));}
        }
        "soft_delete_policy"|"soft_delete_policy.retention_duration"=>{
            let d=b.soft_delete_policy.as_ref().and_then(|p|p.retention_duration).unwrap_or_default();
            if d.nanos!=0{return Err(unsupported("fractional soft delete duration; use whole seconds"));}
            body["softDeletePolicy"]=json!({"retentionDurationSeconds":d.seconds.to_string()});
        }
        "iam_config" => {
            // Go uses this parent mask even when only one IAM setting is present.
            if let Some(i) = &b.iam_config {
                if let Some(u) = &i.uniform_bucket_level_access {
                    body["iamConfiguration"]["uniformBucketLevelAccess"] = json!({"enabled": u.enabled});
                }
                if !i.public_access_prevention.is_empty() {
                    body["iamConfiguration"]["publicAccessPrevention"] = i.public_access_prevention.clone().into();
                }
            }
        }
        "iam_config.uniform_bucket_level_access"|"iam_config.uniform_bucket_level_access.enabled"=>{
            let i=b.iam_config.as_ref();body["iamConfiguration"]["uniformBucketLevelAccess"]=json!({"enabled":i.and_then(|i|i.uniform_bucket_level_access.as_ref()).is_some_and(|u|u.enabled)});
        }
        "iam_config.public_access_prevention"=>body["iamConfiguration"]["publicAccessPrevention"]=b.iam_config.as_ref().map(|i|i.public_access_prevention.as_str()).filter(|s|!s.is_empty()).unwrap_or("inherited").into(),
        _=>return Err(unsupported(&format!("UpdateBucket update_mask {path}; use labels, versioning, lifecycle or iam_config"))),
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
        let sc = conditions(
            r.if_source_generation_match,
            r.if_source_generation_not_match,
            r.if_source_metageneration_match,
            r.if_source_metageneration_not_match,
        )?;
        let dc = conditions(
            r.if_generation_match,
            r.if_generation_not_match,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
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
        let read = authorize_bucket(&self.state, caller, auth::OBJECTS_GET, target.source_bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let create = authorize_bucket(
            &self.state,
            caller,
            auth::OBJECTS_CREATE,
            target.destination_bucket,
        )
        .await
        .map_err(|e| access_error(e, caller))?;
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
        let source = self
            .state
            .catalog
            .resolve_derived_source(
                sb,
                sn,
                (source_generation != 0).then_some(source_generation),
                sc,
                read,
            )
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
            .get_bucket(sb)
            .await
            .map_err(error)?
            .ok_or_else(|| Status::not_found("Source bucket not found"))?;
        let destination_bucket = self
            .state
            .catalog
            .get_bucket(db)
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
        conditions(None, None, Some(r.if_metageneration_match), None)?;
        let access = authorize_bucket(&self.state, caller, auth::BUCKETS_UPDATE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
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
        Ok(Response::new(bucket_resource(&record)))
    }
    pub(super) async fn get_iam_policy(
        &self,
        request: Request<iam::GetIamPolicyRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::Policy>, Status> {
        let r = request.into_inner();
        let bucket = names::bucket_name(&r.resource)?;
        if r.options
            .is_some_and(|o| !matches!(o.requested_policy_version, 0 | 1 | 3))
        {
            return Err(Status::invalid_argument(
                "requested_policy_version must be 0, 1 or 3",
            ));
        }
        let access = authorize_bucket(&self.state, caller, auth::BUCKETS_GET_IAM, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
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
        let bucket = names::bucket_name(&r.resource)?;
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
        let access = authorize_bucket(&self.state, caller, auth::BUCKETS_SET_IAM, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
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
        let bucket = names::bucket_name(&r.resource)?;
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
                Ok(_) => permissions.push(p),
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
        conditions(
            None,
            None,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        let paths = mask(r.update_mask.as_ref())?;
        let mut body = bucket_patch(&b, paths)?;
        let access = authorize_bucket(&self.state, caller, auth::BUCKETS_UPDATE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let mut mm = r.if_metageneration_match;
        if paths.iter().any(|p| p == "labels") && !b.labels.is_empty() {
            let current = self
                .state
                .catalog
                .get_bucket_authorized(bucket, &access)
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
        Ok(Response::new(bucket_resource(&record)))
    }
    pub(super) async fn update_object(
        &self,
        request: Request<pb::UpdateObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(&r.common_object_request_params)?;
        if !r.predefined_acl.is_empty() || r.override_unlocked_retention {
            return Err(unsupported(
                "UpdateObject ACL/retention; omit predefined_acl and override_unlocked_retention",
            ));
        }
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
        let access = authorize_bucket(&self.state, caller, auth::OBJECTS_UPDATE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let mut c = conditions(
            r.if_generation_match,
            r.if_generation_not_match,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        if paths.iter().any(|p| p == "metadata") && !o.metadata.is_empty() {
            let current = self
                .state
                .catalog
                .get_object_authorized(
                    bucket,
                    name,
                    (o.generation != 0).then_some(o.generation),
                    c,
                    &access,
                )
                .await
                .map_err(error)?;
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
        let parsed =
            json::parse_object_metadata(body.to_string().as_bytes(), json::WriteMode::Patch)
                .map_err(error)?;
        parsed.update.validate().map_err(error)?;
        let record = self
            .state
            .catalog
            .update_object_metadata_authorized(
                ObjectSelector {
                    bucket,
                    name,
                    generation: (o.generation != 0).then_some(o.generation),
                    conditions: c,
                },
                parsed.update,
                |_| Ok(()),
                &access,
            )
            .await
            .map_err(error)?;
        Ok(Response::new(convert::object(&record)?))
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
        let access = authorize_bucket(&self.state, caller, auth::OBJECTS_CREATE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
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
                conditions(gm, None, None, None)?;
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
            conditions: conditions(r.if_generation_match, None, r.if_metageneration_match, None)?,
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
        let read = authorize_bucket(&self.state, caller, auth::OBJECTS_GET, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let delete = authorize_bucket(&self.state, caller, auth::OBJECTS_DELETE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let create = authorize_bucket(&self.state, caller, auth::OBJECTS_CREATE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let sc = conditions(
            r.if_source_generation_match,
            r.if_source_generation_not_match,
            r.if_source_metageneration_match,
            r.if_source_metageneration_not_match,
        )?;
        let dc = conditions(
            r.if_generation_match,
            r.if_generation_not_match,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        let record = self
            .state
            .catalog
            .move_object_authorized(
                bucket,
                source,
                &r.destination_object,
                sc,
                dc,
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
        let restore = authorize_bucket(&self.state, caller, auth::OBJECTS_RESTORE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let create = authorize_bucket(&self.state, caller, auth::OBJECTS_CREATE, bucket)
            .await
            .map_err(|e| access_error(e, caller))?;
        let c = conditions(
            r.if_generation_match,
            r.if_generation_not_match,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        let record = self
            .state
            .catalog
            .restore_object_authorized(bucket, name, r.generation, c, &restore, &create)
            .await
            .map_err(error)?;
        Ok(Response::new(convert::object(&record)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                "update-copy-bucket",
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
            "acl",
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
        for path in ["*", "acl", "contexts.custom", "retention"] {
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
            conditions(Some(-1), None, None, None).unwrap_err().code(),
            Code::InvalidArgument
        );
        assert_eq!(
            conditions(None, None, Some(0), None).unwrap_err().code(),
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
                &BucketAccess::Permissive,
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
        assert!(!b.versioning.unwrap().enabled);
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
            .get_object("update-copy-bucket", "object", None, Default::default())
            .await
            .unwrap();
        assert_eq!(read.metadata.len(), 1);
        assert_eq!(read.metadata["replace"], "yes");
        assert!(read.temporary_hold);
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
