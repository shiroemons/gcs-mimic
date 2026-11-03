//! Retired Storage v1 migration adapter; shared catalog operations enforce state and permissions.
mod convert;
#[cfg(test)]
mod tests;
mod upload;

#[expect(
    clippy::allow_attributes,
    reason = "which lints fire in generated code varies by target and cfg(test), so an \
              expectation on them could go unfulfilled"
)]
#[allow(
    clippy::all,
    dead_code,
    unreachable_pub,
    rustdoc::broken_intra_doc_links,
    reason = "generated upstream code keeps its own style and documentation links"
)]
pub mod pb {
    include!("../generated/google.storage.v1.rs");
}

use crate::{
    auth::{BUCKETS_LIST, Caller},
    bucket::{BucketAccess, BucketConditions, CatalogError},
    grpc::{
        v2::status::unsupported,
        validate::{self, bucket_conditions, request_conditions},
    },
    json,
    object::{ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectTarget},
    server::State,
};
use prost::Message;
use serde_json::json;
use std::sync::Arc;
use tonic::{Request, Response, Status, Streaming};

pub(crate) struct Storage {
    state: Arc<State>,
    max_object_bytes: u64,
    compat_profile: crate::config::CompatProfile,
}

pub(crate) fn service(
    state: Arc<State>,
    max_object_bytes: u64,
    compat_profile: crate::config::CompatProfile,
) -> pb::storage_server::StorageServer<Storage> {
    pb::storage_server::StorageServer::new(Storage {
        state,
        max_object_bytes,
        compat_profile,
    })
    .max_decoding_message_size(4 * 1024 * 1024)
    .max_encoding_message_size(4 * 1024 * 1024)
}

pub(crate) fn known_method(path: &str) -> bool {
    path.strip_prefix("/google.storage.v1.Storage/")
        .is_some_and(|method| {
            matches!(
                method,
                "GetBucket"
                    | "DeleteBucket"
                    | "InsertBucket"
                    | "UpdateBucket"
                    | "ListBuckets"
                    | "InsertObject"
                    | "GetObject"
                    | "DeleteObject"
                    | "UpdateObject"
                    | "PatchObject"
                    | "ComposeObject"
                    | "ListObjects"
            )
        })
}

/// The catalog can deny after `access` (deferred object ACLs, overwrite, holds, compose sources);
/// anonymous callers then still get JSON's credentials-required mapping.
fn object_access_error(error: ObjectError, caller: Option<Caller<'_>>) -> Status {
    crate::grpc::v2::status::anonymous(error.into(), caller)
}

fn common(
    params: Option<&pb::CommonRequestParams>,
    object: Option<&pb::CommonObjectRequestParams>,
) -> Result<(), Status> {
    const MESSAGE: &str =
        "legacy common request parameters; omit billing, quota, field selection and encryption";
    validate::common(params, MESSAGE)?;
    validate::common(object, MESSAGE)
}

fn projection(value: i32) -> Result<(), Status> {
    if value == 0 || value == pb::common_enums::Projection::NoAcl as i32 {
        Ok(())
    } else {
        Err(unsupported(
            "legacy ACL projection; omit projection or use NO_ACL",
        ))
    }
}

fn conditions(conditions: ObjectConditions) -> Result<ObjectConditions, Status> {
    validate::lenient_conditions(
        conditions,
        "Preconditions must be nonnegative; use if_generation_match=0 for create only",
    )
}

fn generation(value: i64) -> Result<Option<i64>, Status> {
    validate::generation(
        value,
        "generation must be positive or 0 for the live object",
    )
}

fn limit(value: i32) -> Result<usize, Status> {
    let value = usize::try_from(value).map_err(|_| {
        Status::invalid_argument("max_results must be nonnegative, for example 100")
    })?;
    Ok(if value == 0 { 1000 } else { value.min(1000) })
}

fn validate_list_query(request: &pb::ListObjectsRequest, max_results: usize) -> Result<(), Status> {
    let encoded_len = |value: &str| {
        value
            .bytes()
            .map(|byte| {
                if crate::url::is_unreserved(byte) {
                    1
                } else {
                    3
                }
            })
            .sum::<usize>()
    };
    let query_len = format!("prefix=&delimiter=&pageToken=&maxResults={max_results}&versions={}&includeTrailingDelimiter={}", request.versions, request.include_trailing_delimiter).len()
        + encoded_len(&request.prefix) + encoded_len(&request.delimiter) + encoded_len(&request.page_token);
    let uri_len = "/storage/v1/b//o?".len() + encoded_len(&request.bucket) + query_len;
    // The pinned http URI parser checks this limit before the JSON query parser runs.
    if uri_len > usize::from(u16::MAX) - 1 {
        return Err(Status::invalid_argument("Invalid list request"));
    }
    if query_len > json::MAX_QUERY_BYTES {
        return Err(Status::invalid_argument(
            "Invalid object listing or page token; use the token with its original query",
        ));
    }
    Ok(())
}

/// One UpdateObject or PatchObject request after common parameter validation.
struct ObjectUpdate<'a> {
    bucket: &'a str,
    name: &'a str,
    selected: i64,
    conditions: ObjectConditions,
    object: pb::Object,
    write: json::WriteMode,
}

fn metadata(
    mut object: pb::Object,
    write: json::WriteMode,
) -> Result<ObjectMetadataUpdate, Status> {
    let replace = write == json::WriteMode::Replace;
    if object.encoded_len() > 8192 {
        return Err(Status::resource_exhausted(
            "Object metadata exceeds 8 KiB; reduce metadata",
        ));
    }
    let mut body = json!({});
    for (key, value) in [
        ("contentType", &object.content_type),
        ("contentEncoding", &object.content_encoding),
        ("contentDisposition", &object.content_disposition),
        ("contentLanguage", &object.content_language),
        ("cacheControl", &object.cache_control),
    ] {
        if replace || !value.is_empty() {
            body[key] = if value.is_empty() {
                serde_json::Value::Null
            } else {
                json!(value)
            };
        }
    }
    if replace || !object.metadata.is_empty() {
        body["metadata"] = json!(object.metadata);
    }
    if let Some(hold) = object.event_based_hold {
        body["eventBasedHold"] = json!(hold);
    }
    // `temporary_hold` is a plain bool, so false cannot be told from omitted; a replace
    // already releases a hold it does not name, and only true is a statement worth keeping.
    // Treating a v1 replace that sends `temporary_hold=false` as never set (not as a released
    // `false`) is an inference: the recording has no gRPC session.
    if object.temporary_hold {
        body["temporaryHold"] = json!(true);
    }
    object.name.clear();
    object.bucket.clear();
    object.content_type.clear();
    object.content_encoding.clear();
    object.content_disposition.clear();
    object.content_language.clear();
    object.cache_control.clear();
    object.metadata.clear();
    object.event_based_hold = None;
    object.temporary_hold = false;
    if object != Default::default() {
        return Err(unsupported(
            "legacy object metadata outside content fields, custom metadata and holds",
        ));
    }
    Ok(json::parse_object_metadata(
        &serde_json::to_vec(&body).map_err(|_| Status::internal("Cannot encode metadata"))?,
        write,
    )?
    .update)
}

impl Storage {
    fn authenticate<T>(&self, request: &Request<T>) -> Result<Option<Caller<'_>>, Status> {
        if request.metadata().contains_key("x-goog-request-params") {
            return Err(unsupported(
                "legacy routing metadata; put bucket and object in the request and omit x-goog-request-params",
            ));
        }
        if request.metadata().contains_key("x-goog-user-project") {
            return Err(unsupported(
                "legacy billing metadata; omit x-goog-user-project",
            ));
        }
        self.state.auth.authenticate_at(&request.metadata().clone().into_headers(), None, self.state.clock.as_ref())
            .map_err(|_| Status::unauthenticated("Use a registered local token, for example authorization: Bearer local-admin-token with identity_file"))
    }

    async fn access(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        permission: &str,
    ) -> Result<BucketAccess, Status> {
        json::validate_bucket_name_for_profile(bucket, self.compat_profile)?;
        crate::server::authorize_bucket(&self.state, caller, permission, bucket)
            .await
            .map_err(|e| Status::from(crate::server::anonymous_error(e, caller)))
    }

    fn project(
        &self,
        caller: Option<Caller<'_>>,
        project: &str,
        permission: &str,
    ) -> Result<(), Status> {
        json::validate_project(project)?;
        if let Some(caller) = caller {
            caller.authorize(permission, Some(project))?;
        }
        Ok(())
    }

    async fn update(
        &self,
        caller: Option<Caller<'_>>,
        update: ObjectUpdate<'_>,
    ) -> Result<Response<pb::Object>, Status> {
        let ObjectUpdate {
            bucket,
            name,
            selected,
            conditions,
            mut object,
            write,
        } = update;
        crate::object::name::validate(name)
            .map_err(|_| Status::invalid_argument("Invalid object name"))?;
        let access = self
            .access(caller, bucket, crate::auth::OBJECTS_UPDATE)
            .await?;
        if (!object.name.is_empty() && object.name != name)
            || (!object.bucket.is_empty() && object.bucket != bucket)
        {
            return Err(Status::invalid_argument(
                "Metadata bucket/name must match the request",
            ));
        }
        let mut supplied = object.clone();
        convert::object_outputs(&mut object, None)?;
        let update = metadata(object, write)?;
        let record = self
            .state
            .catalog
            .update_object_metadata(
                ObjectTarget::new(bucket, name, generation(selected)?),
                conditions,
                update,
                move |record| {
                    let current = convert::object(record)?;
                    convert::object_outputs(&mut supplied, Some(&current))
                },
                &access,
            )
            .await
            .map_err(|error| object_access_error(error, caller))?;
        Ok(Response::new(convert::object(&record)?))
    }
}

#[tonic::async_trait]
impl pb::storage_server::Storage for Storage {
    async fn get_bucket(
        &self,
        request: Request<pb::GetBucketRequest>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        conditions(ObjectConditions {
            if_metageneration_match: r.if_metageneration_match,
            if_metageneration_not_match: r.if_metageneration_not_match,
            ..Default::default()
        })?;
        let access = self
            .access(caller, &r.bucket, crate::auth::BUCKETS_GET)
            .await?;
        let record = self
            .state
            .catalog
            .get_bucket(&r.bucket, &access)
            .await
            .map_err(Status::from)?
            .ok_or_else(|| {
                Status::not_found("Bucket not found; create it with InsertBucket or JSON")
            })?;
        if !bucket_conditions!(r).hold(record.metageneration) {
            return Err(Status::failed_precondition(
                "Bucket metageneration condition failed; read current metadata",
            ));
        }
        Ok(Response::new(convert::bucket(&record)))
    }

    async fn delete_bucket(
        &self,
        request: Request<pb::DeleteBucketRequest>,
    ) -> Result<Response<()>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        conditions(ObjectConditions {
            if_metageneration_match: r.if_metageneration_match,
            if_metageneration_not_match: r.if_metageneration_not_match,
            ..Default::default()
        })?;
        let access = self
            .access(caller, &r.bucket, crate::auth::BUCKETS_DELETE)
            .await?;
        self.state
            .catalog
            .delete_bucket(&r.bucket, bucket_conditions!(r), &access)
            .await
            .map_err(Status::from)?;
        Ok(Response::new(()))
    }

    async fn insert_bucket(
        &self,
        request: Request<pb::InsertBucketRequest>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        if r.predefined_acl != 0 || r.predefined_default_object_acl != 0 {
            return Err(unsupported("legacy bucket predefined ACLs; omit them"));
        }
        self.project(caller, &r.project, crate::auth::BUCKETS_CREATE)?;
        let mut bucket = r.bucket.ok_or_else(|| {
            Status::invalid_argument("bucket is required; set bucket.name=example-bucket")
        })?;
        let body = convert::bucket_metadata(&mut bucket, None)?;
        let record = json::parse_insert_with_profile(
            &body,
            &r.project,
            self.state.clock.now(),
            self.compat_profile,
            &self.state.default_location,
        )?;
        let record = self
            .state
            .catalog
            .insert_bucket(record)
            .await
            .map_err(Status::from)?;
        Ok(Response::new(convert::bucket(&record)))
    }

    async fn update_bucket(
        &self,
        request: Request<pb::UpdateBucketRequest>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        if r.predefined_acl != 0 || r.predefined_default_object_acl != 0 {
            return Err(unsupported("legacy bucket predefined ACLs; omit them"));
        }
        conditions(ObjectConditions {
            if_metageneration_match: r.if_metageneration_match,
            if_metageneration_not_match: r.if_metageneration_not_match,
            ..Default::default()
        })?;
        let requested = bucket_conditions!(r);
        let access = self
            .access(caller, &r.bucket, crate::auth::BUCKETS_UPDATE)
            .await?;
        let mut bucket = r.metadata.ok_or_else(|| {
            Status::invalid_argument("metadata is required; supply default_event_based_hold=false")
        })?;
        if !bucket.name.is_empty() && bucket.name != r.bucket {
            return Err(Status::invalid_argument("metadata.name must match bucket"));
        }
        let current = self
            .state
            .catalog
            .get_bucket(&r.bucket, &access)
            .await
            .map_err(Status::from)?
            .ok_or_else(|| Status::from(CatalogError::NotFound))?;
        if !requested.hold(current.metageneration) {
            return Err(Status::from(CatalogError::PreconditionFailed));
        }
        let body = convert::bucket_metadata(&mut bucket, Some(&convert::bucket(&current)))?;
        let update = json::parse_bucket_replacement(&body, self.compat_profile)?;
        let record = self
            .state
            .catalog
            .replace_bucket(
                &r.bucket,
                update,
                // Fence output-only validation against concurrent metadata changes.
                BucketConditions {
                    if_metageneration_match: Some(current.metageneration),
                    ..requested
                },
                &access,
            )
            .await
            .map_err(Status::from)?;
        Ok(Response::new(convert::bucket(&record)))
    }

    async fn list_buckets(
        &self,
        request: Request<pb::ListBucketsRequest>,
    ) -> Result<Response<pb::ListBucketsResponse>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        // The reference lists every bucket when project is omitted. Strict callers only see
        // buckets in projects where they may list, so omission never widens their access.
        let all_projects = r.project.is_empty();
        let projects = if all_projects {
            match caller {
                Some(caller) => {
                    caller.authorize(BUCKETS_LIST, None)?;
                    let identity = caller.principal()?;
                    Some(identity.projects_allowing(BUCKETS_LIST))
                }
                None => None,
            }
        } else {
            self.project(caller, &r.project, BUCKETS_LIST)?;
            Some(vec![r.project.clone()])
        };
        if r.prefix.len() > 222 {
            return Err(Status::invalid_argument("prefix must be at most 222 bytes"));
        }
        let start = if r.page_token.is_empty() {
            None
        } else if all_projects {
            Some(json::decode_all_projects_page_token(
                &r.page_token,
                &r.prefix,
            )?)
        } else {
            Some(json::decode_page_token(
                &r.page_token,
                &r.project,
                &r.prefix,
            )?)
        };
        let page = self
            .state
            .catalog
            .list_buckets_page_among(
                projects,
                Some(&r.prefix),
                start.as_deref(),
                limit(r.max_results)?,
            )
            .await
            .map_err(Status::from)?;
        let next_page_token = if page.has_more {
            let last = &page
                .items
                .last()
                .ok_or_else(|| Status::internal("Missing continuation bucket"))?
                .name;
            if all_projects {
                json::encode_all_projects_page_token(&r.prefix, last)
            } else {
                json::encode_page_token(&r.project, &r.prefix, last)
            }?
        } else {
            String::new()
        };
        Ok(Response::new(pb::ListBucketsResponse {
            items: page.items.iter().map(convert::bucket).collect(),
            next_page_token,
        }))
    }

    async fn get_object(
        &self,
        request: Request<pb::GetObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        projection(r.projection)?;
        crate::object::name::validate(&r.object)
            .map_err(|_| Status::invalid_argument("Invalid object name"))?;
        let access = self
            .access(caller, &r.bucket, crate::auth::OBJECTS_GET)
            .await?;
        let target = ObjectTarget::new(&r.bucket, &r.object, generation(r.generation)?);
        let record = self
            .state
            .catalog
            .get_object(target, request_conditions!(conditions, r)?, &access)
            .await
            .map_err(|error| object_access_error(error, caller))?;
        Ok(Response::new(convert::object(&record)?))
    }

    async fn delete_object(
        &self,
        request: Request<pb::DeleteObjectRequest>,
    ) -> Result<Response<()>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        crate::object::name::validate(&r.object)
            .map_err(|_| Status::invalid_argument("Invalid object name"))?;
        let access = self
            .access(caller, &r.bucket, crate::auth::OBJECTS_DELETE)
            .await?;
        let target = ObjectTarget::new(&r.bucket, &r.object, generation(r.generation)?);
        self.state
            .catalog
            .delete_object(target, request_conditions!(conditions, r)?, &access)
            .await
            .map_err(|error| object_access_error(error, caller))?;
        Ok(Response::new(()))
    }

    async fn update_object(
        &self,
        request: Request<pb::UpdateObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        projection(r.projection)?;
        if r.predefined_acl != 0 {
            return Err(unsupported("legacy predefined ACL; omit predefined_acl"));
        }
        let update = ObjectUpdate {
            bucket: &r.bucket,
            name: &r.object,
            selected: r.generation,
            conditions: request_conditions!(conditions, r)?,
            object: r.metadata.unwrap_or_default(),
            write: json::WriteMode::Replace,
        };
        self.update(caller, update).await
    }

    async fn patch_object(
        &self,
        request: Request<pb::PatchObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        projection(r.projection)?;
        if r.predefined_acl != 0 || r.update_mask.as_ref().is_some_and(|m| !m.paths.is_empty()) {
            return Err(unsupported(
                "legacy PatchObject predefined ACL or update_mask; omit them for the reference merge profile",
            ));
        }
        let update = ObjectUpdate {
            bucket: &r.bucket,
            name: &r.object,
            selected: r.generation,
            conditions: request_conditions!(conditions, r)?,
            object: r.metadata.unwrap_or_default(),
            write: json::WriteMode::Patch,
        };
        self.update(caller, update).await
    }

    async fn list_objects(
        &self,
        request: Request<pb::ListObjectsRequest>,
    ) -> Result<Response<pb::ListObjectsResponse>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        let access = self
            .access(caller, &r.bucket, crate::auth::OBJECTS_LIST)
            .await?;
        let max_results = limit(r.max_results)?;
        validate_list_query(&r, max_results)?;
        let mut query = crate::storage::ObjectListQuery {
            bucket: r.bucket,
            prefix: r.prefix,
            delimiter: r.delimiter,
            include_trailing_delimiter: r.include_trailing_delimiter,
            versions: r.versions,
            ..Default::default()
        };
        if !r.page_token.is_empty() {
            query.cursor = Some(json::decode_object_page_token(
                &r.page_token, &query, json::Projection::NoAcl,
            ).map_err(|_| Status::invalid_argument(
                "Invalid object listing or page token; use the token with its original query",
            ))?);
        }
        let page = self
            .state
            .catalog
            .list_objects(query.clone(), max_results, &access)
            .await?;
        let next_page_token = page
            .next
            .as_ref()
            .map(|cursor| json::encode_object_page_token(&query, json::Projection::NoAcl, cursor))
            .unwrap_or_default();
        Ok(Response::new(pb::ListObjectsResponse {
            items: page
                .items
                .iter()
                .map(convert::object)
                .collect::<Result<_, _>>()?,
            prefixes: page.prefixes,
            next_page_token,
        }))
    }

    async fn compose_object(
        &self,
        request: Request<pb::ComposeObjectRequest>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        if r.destination_predefined_acl != 0 || !r.kms_key_name.is_empty() {
            return Err(unsupported("legacy compose ACL or KMS; omit these fields"));
        }
        crate::object::name::validate(&r.destination_object)
            .map_err(|_| Status::invalid_argument("Invalid object name"))?;
        let access = self
            .access(caller, &r.destination_bucket, crate::auth::OBJECTS_CREATE)
            .await?;
        let object = r.destination.unwrap_or_default();
        if (!object.name.is_empty() && object.name != r.destination_object)
            || (!object.bucket.is_empty() && object.bucket != r.destination_bucket)
        {
            return Err(Status::invalid_argument(
                "Destination metadata bucket/name must match the request",
            ));
        }
        let content_type = object.content_type.clone();
        let metadata = metadata(object, json::WriteMode::Replace)?;
        let sources = r
            .source_objects
            .into_iter()
            .map(|s| {
                crate::object::name::validate(&s.name)
                    .map_err(|_| Status::invalid_argument("Invalid object name"))?;
                let condition = s.object_preconditions.and_then(|p| p.if_generation_match);
                conditions(ObjectConditions {
                    if_generation_match: condition,
                    ..Default::default()
                })?;
                Ok(crate::object::ComposeSource {
                    name: s.name,
                    generation: generation(s.generation)?,
                    if_generation_match: condition,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let record = self
            .state
            .catalog
            .compose_object(
                crate::storage::DerivedDestination {
                    bucket: r.destination_bucket,
                    name: r.destination_object,
                    content_type,
                    metadata,
                    storage_class: None,
                    expected: Default::default(),
                    conditions: conditions(ObjectConditions {
                        if_generation_match: r.if_generation_match,
                        if_metageneration_match: r.if_metageneration_match,
                        ..Default::default()
                    })?,
                    access,
                    component_count: None,
                },
                sources,
                crate::storage::SourceDisposal::Keep,
                self.max_object_bytes,
            )
            .await
            .map_err(|error| object_access_error(error, caller))?;
        Ok(Response::new(convert::object(&record)?))
    }

    async fn insert_object(
        &self,
        request: Request<Streaming<pb::InsertObjectRequest>>,
    ) -> Result<Response<pb::Object>, Status> {
        let caller = self.authenticate(&request)?;
        self.upload(request.into_inner(), caller)
            .await
            .map(Response::new)
            .map_err(|status| crate::grpc::v2::status::anonymous(status, caller))
    }
}
