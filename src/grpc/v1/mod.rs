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
    bucket::{BucketAccess, CatalogError},
    grpc::v2::status::{from_json, unsupported},
    json,
    object::{ObjectConditions, ObjectError, ObjectMetadataUpdate},
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

fn object_error(error: ObjectError) -> Status {
    from_json(error.into())
}
/// Object reads and updates defer their ACL decision to the catalog, past `access`.
fn object_access_error(error: ObjectError, caller: Option<Caller<'_>>) -> Status {
    crate::grpc::v2::status::anonymous(object_error(error), caller)
}
fn catalog_error(error: CatalogError) -> Status {
    if matches!(error, CatalogError::NotEmpty) {
        Status::failed_precondition("Bucket is not empty; delete objects and versions first")
    } else {
        from_json(error.into())
    }
}

fn common(
    params: Option<&pb::CommonRequestParams>,
    object: Option<&pb::CommonObjectRequestParams>,
) -> Result<(), Status> {
    if params.is_some_and(|p| p != &Default::default())
        || object.is_some_and(|p| p != &Default::default())
    {
        return Err(unsupported(
            "legacy common request parameters; omit billing, quota, field selection and encryption",
        ));
    }
    Ok(())
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

fn conditions(
    g: Option<i64>,
    ng: Option<i64>,
    m: Option<i64>,
    nm: Option<i64>,
) -> Result<ObjectConditions, Status> {
    if [g, ng, m, nm].into_iter().flatten().any(|v| v < 0) {
        return Err(Status::invalid_argument(
            "Preconditions must be nonnegative; use if_generation_match=0 for create only",
        ));
    }
    Ok(ObjectConditions {
        if_generation_match: g,
        if_generation_not_match: ng,
        if_metageneration_match: m,
        if_metageneration_not_match: nm,
    })
}

fn generation(value: i64) -> Result<Option<i64>, Status> {
    if value < 0 {
        return Err(Status::invalid_argument(
            "generation must be positive or 0 for the live object",
        ));
    }
    Ok((value != 0).then_some(value))
}

fn limit(value: i32) -> Result<usize, Status> {
    if value < 0 {
        return Err(Status::invalid_argument(
            "max_results must be nonnegative, for example 100",
        ));
    }
    Ok(if value == 0 {
        1000
    } else {
        value.min(1000) as usize
    })
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
    if replace || object.temporary_hold {
        body["temporaryHold"] = json!(object.temporary_hold);
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
    )
    .map_err(from_json)?
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
        json::validate_bucket_name_for_profile(bucket, self.compat_profile).map_err(from_json)?;
        crate::server::authorize_bucket(&self.state, caller, permission, bucket)
            .await
            .map_err(|e| from_json(crate::server::anonymous_error(e, caller)))
    }

    fn project(
        &self,
        caller: Option<Caller<'_>>,
        project: &str,
        permission: &str,
    ) -> Result<(), Status> {
        json::validate_project(project).map_err(from_json)?;
        if let Some(caller) = caller {
            caller
                .authorize(permission, Some(project))
                .map_err(|e| from_json(e.into()))?;
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
        json::validate_object_name(name).map_err(from_json)?;
        let access = self
            .access(caller, bucket, "storage.objects.update")
            .await?;
        if (!object.name.is_empty() && object.name != name)
            || (!object.bucket.is_empty() && object.bucket != bucket)
        {
            return Err(Status::invalid_argument(
                "Metadata bucket/name must match the request",
            ));
        }
        let mut supplied = object.clone();
        convert::object_outputs(&mut object, None).map_err(object_error)?;
        let update = metadata(object, write)?;
        let record = self
            .state
            .catalog
            .update_object_metadata_authorized(
                crate::storage::ObjectSelector {
                    bucket,
                    name,
                    generation: generation(selected)?,
                    conditions,
                },
                update,
                move |record| {
                    let current = convert::object(record)
                        .map_err(|e| ObjectError::Unavailable(e.message().into()))?;
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
        conditions(
            None,
            None,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        let access = self
            .access(caller, &r.bucket, "storage.buckets.get")
            .await?;
        let record = self
            .state
            .catalog
            .get_bucket_authorized(&r.bucket, &access)
            .await
            .map_err(catalog_error)?
            .ok_or_else(|| {
                Status::not_found("Bucket not found; create it with InsertBucket or JSON")
            })?;
        if r.if_metageneration_match
            .is_some_and(|v| v != record.metageneration)
            || r.if_metageneration_not_match == Some(record.metageneration)
        {
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
        conditions(
            None,
            None,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        let access = self
            .access(caller, &r.bucket, "storage.buckets.delete")
            .await?;
        self.state
            .catalog
            .delete_bucket_authorized(
                &r.bucket,
                r.if_metageneration_match,
                r.if_metageneration_not_match,
                &access,
            )
            .await
            .map_err(catalog_error)?;
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
        self.project(caller, &r.project, "storage.buckets.create")?;
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
        )
        .map_err(from_json)?;
        let record = self
            .state
            .catalog
            .insert_bucket(record)
            .await
            .map_err(catalog_error)?;
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
        conditions(
            None,
            None,
            r.if_metageneration_match,
            r.if_metageneration_not_match,
        )?;
        let access = self
            .access(caller, &r.bucket, "storage.buckets.update")
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
            .get_bucket_authorized(&r.bucket, &access)
            .await
            .map_err(catalog_error)?
            .ok_or_else(|| catalog_error(CatalogError::NotFound))?;
        if r.if_metageneration_match
            .is_some_and(|v| v != current.metageneration)
            || r.if_metageneration_not_match == Some(current.metageneration)
        {
            return Err(catalog_error(CatalogError::PreconditionFailed));
        }
        let body = convert::bucket_metadata(&mut bucket, Some(&convert::bucket(&current)))?;
        let update =
            json::parse_bucket_replacement(&body, self.compat_profile).map_err(from_json)?;
        let record = self
            .state
            .catalog
            .replace_bucket(
                &r.bucket,
                update,
                // Fence output-only validation against concurrent metadata changes.
                Some(current.metageneration),
                r.if_metageneration_not_match,
                &access,
            )
            .await
            .map_err(catalog_error)?;
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
                    caller
                        .authorize(BUCKETS_LIST, None)
                        .map_err(|e| from_json(e.into()))?;
                    let identity = caller.principal().map_err(|e| from_json(e.into()))?;
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
            Some(
                json::decode_all_projects_page_token(&r.page_token, &r.prefix)
                    .map_err(from_json)?,
            )
        } else {
            Some(json::decode_page_token(&r.page_token, &r.project, &r.prefix).map_err(from_json)?)
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
            .map_err(catalog_error)?;
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
            }
            .map_err(from_json)?
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
        json::validate_object_name(&r.object).map_err(from_json)?;
        let access = self
            .access(caller, &r.bucket, "storage.objects.get")
            .await?;
        let record = self
            .state
            .catalog
            .get_object_authorized(
                &r.bucket,
                &r.object,
                generation(r.generation)?,
                conditions(
                    r.if_generation_match,
                    r.if_generation_not_match,
                    r.if_metageneration_match,
                    r.if_metageneration_not_match,
                )?,
                &access,
            )
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
        json::validate_object_name(&r.object).map_err(from_json)?;
        let access = self
            .access(caller, &r.bucket, "storage.objects.delete")
            .await?;
        self.state
            .catalog
            .delete_object_authorized(
                &r.bucket,
                &r.object,
                generation(r.generation)?,
                conditions(
                    r.if_generation_match,
                    r.if_generation_not_match,
                    r.if_metageneration_match,
                    r.if_metageneration_not_match,
                )?,
                &access,
            )
            .await
            .map_err(object_error)?;
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
            conditions: conditions(
                r.if_generation_match,
                r.if_generation_not_match,
                r.if_metageneration_match,
                r.if_metageneration_not_match,
            )?,
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
            conditions: conditions(
                r.if_generation_match,
                r.if_generation_not_match,
                r.if_metageneration_match,
                r.if_metageneration_not_match,
            )?,
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
            .access(caller, &r.bucket, "storage.objects.list")
            .await?;
        let uri = format!("/storage/v1/b/{}/o?prefix={}&delimiter={}&pageToken={}&maxResults={}&versions={}&includeTrailingDelimiter={}",crate::auth::signature::encode_query_component(&r.bucket),crate::auth::signature::encode_query_component(&r.prefix),crate::auth::signature::encode_query_component(&r.delimiter),crate::auth::signature::encode_query_component(&r.page_token),limit(r.max_results)?,r.versions,r.include_trailing_delimiter).parse().map_err(|_| Status::invalid_argument("Invalid list request"))?;
        let Some(Ok(json::ObjectRequest::List(params))) =
            json::route_object_with_profile(&hyper::Method::GET, &uri, self.compat_profile)
        else {
            return Err(Status::invalid_argument(
                "Invalid object listing or page token; use the token with its original query",
            ));
        };
        let page = self
            .state
            .catalog
            .list_objects_authorized(params.query.clone(), params.max_results, &access)
            .await
            .map_err(object_error)?;
        let rendered =
            json::object_list(&page, &params, &self.state.api_base_url).map_err(from_json)?;
        Ok(Response::new(pb::ListObjectsResponse {
            items: page
                .items
                .iter()
                .map(convert::object)
                .collect::<Result<_, _>>()?,
            prefixes: page.prefixes,
            next_page_token: rendered["nextPageToken"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
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
        json::validate_object_name(&r.destination_object).map_err(from_json)?;
        let access = self
            .access(caller, &r.destination_bucket, "storage.objects.create")
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
                json::validate_object_name(&s.name).map_err(from_json)?;
                let condition = s.object_preconditions.and_then(|p| p.if_generation_match);
                conditions(condition, None, None, None)?;
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
                    conditions: conditions(
                        r.if_generation_match,
                        None,
                        r.if_metageneration_match,
                        None,
                    )?,
                    access,
                    component_count: None,
                },
                sources,
                crate::storage::SourceDisposal::Keep,
                self.max_object_bytes,
            )
            .await
            .map_err(object_error)?;
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
    }
}
