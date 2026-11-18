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
    auth::{BUCKETS_LIST, Caller, ListenerRole},
    bucket::{BucketAccess, BucketConditions, CatalogError},
    grpc::{
        v2::status::unsupported,
        validate::{self, NegativeGeneration},
    },
    json,
    object::{
        MapPatch, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectTarget, Patch,
        WriteMode, generation::Generation,
    },
    page_token::{self, PageTokenError},
    server::State,
};
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

/// The path prefix of every legacy v1 method.
pub(crate) const SERVICE_PREFIX: &str = "/google.storage.v1.Storage/";

// Dispatch and forwarding come from one list: a method listed here has a handler, and the
// generated trait's "Not yet implemented" default can only answer a method that is not.
macro_rules! legacy_methods {
    ($($wire:ident => $method:ident($request:ty $(,)?) -> $response:ty;)*) => {
        pub(crate) fn known_method(path: &str) -> bool {
            path.strip_prefix(SERVICE_PREFIX)
                .is_some_and(|method| matches!(method, $(stringify!($wire))|*))
        }

        #[tonic::async_trait]
        impl pb::storage_server::Storage for Storage {
            $(
                async fn $method(
                    &self,
                    request: Request<$request>,
                ) -> Result<Response<$response>, Status> {
                    let caller = self.authenticate(&request).await?;
                    self.$method(request, caller).await
                }
            )*
        }
    };
}

legacy_methods! {
    GetBucket => get_bucket(pb::GetBucketRequest) -> pb::Bucket;
    DeleteBucket => delete_bucket(pb::DeleteBucketRequest) -> ();
    InsertBucket => insert_bucket(pb::InsertBucketRequest) -> pb::Bucket;
    UpdateBucket => update_bucket(pb::UpdateBucketRequest) -> pb::Bucket;
    ListBuckets => list_buckets(pb::ListBucketsRequest) -> pb::ListBucketsResponse;
    InsertObject => insert_object(Streaming<pb::InsertObjectRequest>) -> pb::Object;
    GetObject => get_object(pb::GetObjectRequest) -> pb::Object;
    DeleteObject => delete_object(pb::DeleteObjectRequest) -> ();
    UpdateObject => update_object(pb::UpdateObjectRequest) -> pb::Object;
    PatchObject => patch_object(pb::PatchObjectRequest) -> pb::Object;
    ComposeObject => compose_object(pb::ComposeObjectRequest) -> pb::Object;
    ListObjects => list_objects(pb::ListObjectsRequest) -> pb::ListObjectsResponse;
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
        "legacy common request parameters; omit quota, field selection and encryption";
    // The Requester Pays billing project is ignored, like the `x-goog-user-project` metadata;
    // every other field stays rejected.
    let params = params.map(|params| pb::CommonRequestParams {
        user_project: String::new(),
        ..params.clone()
    });
    validate::common(params.as_ref(), MESSAGE)?;
    validate::common(object, MESSAGE)
}

/// The legacy adapter words every broken object-name rule the same way.
fn object_name(name: &str) -> Result<(), Status> {
    use crate::object::name::InvalidObjectName;
    crate::object::name::validate(name).map_err(|error| match error {
        InvalidObjectName::Empty
        | InvalidObjectName::TooLong
        | InvalidObjectName::LineBreak
        | InvalidObjectName::DotSegment
        | InvalidObjectName::AcmeChallenge => Status::invalid_argument("Invalid object name"),
    })
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

const NEGATIVE_PRECONDITION: &str =
    "Preconditions must be nonnegative; use if_generation_match=0 for create only";

fn conditions(conditions: ObjectConditions) -> Result<ObjectConditions, Status> {
    validate::lenient_conditions(conditions, NEGATIVE_PRECONDITION)
}

fn bucket_conditions(conditions: BucketConditions) -> Result<BucketConditions, Status> {
    validate::lenient_bucket_conditions(conditions, NEGATIVE_PRECONDITION)
}

fn generation(value: i64) -> Result<Option<Generation>, Status> {
    validate::selected_generation(value).map_err(|NegativeGeneration| {
        Status::invalid_argument("generation must be positive or 0 for the live object")
    })
}

fn limit(value: i32) -> Result<usize, Status> {
    validate::page_size(value)
        .ok_or_else(|| Status::invalid_argument("max_results must be nonnegative, for example 100"))
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
    if query_len > crate::percent::MAX_QUERY_BYTES {
        return Err(Status::invalid_argument(
            "Invalid object listing or page token; use the token with its original query",
        ));
    }
    Ok(())
}

/// Which projects a legacy ListBuckets covers. The reference lists every bucket when the
/// request names no project; strict callers then see only projects where they may list, so
/// omitting the project never widens their access.
#[derive(Clone, Copy)]
enum ProjectScope<'a> {
    All,
    One(&'a str),
}

impl<'a> ProjectScope<'a> {
    fn of(project: &'a str) -> Self {
        if project.is_empty() {
            Self::All
        } else {
            Self::One(project)
        }
    }

    fn decode_token(self, token: &str, prefix: &str) -> Result<String, PageTokenError> {
        match self {
            Self::All => page_token::decode_all_projects_page_token(token, prefix),
            Self::One(project) => page_token::decode_page_token(token, project, prefix),
        }
    }

    fn encode_token(self, prefix: &str, last: &str) -> Result<String, PageTokenError> {
        match self {
            Self::All => page_token::encode_all_projects_page_token(prefix, last),
            Self::One(project) => page_token::encode_page_token(project, prefix, last),
        }
    }
}

/// One UpdateObject or PatchObject request after common parameter validation.
struct ObjectUpdate<'a> {
    bucket: &'a str,
    name: &'a str,
    selected: i64,
    conditions: ObjectConditions,
    object: pb::Object,
    write: WriteMode,
}

fn metadata(mut object: pb::Object, write: WriteMode) -> Result<ObjectMetadataUpdate, Status> {
    let replace = write == WriteMode::Replace;
    let mut update = ObjectMetadataUpdate {
        write,
        ..Default::default()
    };
    for (field, value) in [
        (&mut update.content_type, &object.content_type),
        (&mut update.content_encoding, &object.content_encoding),
        (&mut update.content_disposition, &object.content_disposition),
        (&mut update.content_language, &object.content_language),
        (&mut update.cache_control, &object.cache_control),
    ] {
        if replace || !value.is_empty() {
            *field = Patch::nullable((!value.is_empty()).then(|| value.clone()));
        }
    }
    if replace || !object.metadata.is_empty() {
        update.metadata = MapPatch::Merge(
            object
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect(),
        );
    }
    update.event_based_hold = object.event_based_hold;
    // `temporary_hold` is a plain bool, so false cannot be told from omitted; a replace
    // already releases a hold it does not name, and only true is a statement worth keeping.
    // Treating a v1 replace that sends `temporary_hold=false` as never set (not as a released
    // `false`) is an inference: the recording has no gRPC session.
    if object.temporary_hold {
        update.temporary_hold = Some(true);
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
    update.check_encoded_size()?;
    Ok(update)
}

impl Storage {
    fn authenticate<T>(
        &self,
        request: &Request<T>,
    ) -> impl Future<Output = Result<Option<Caller<'_>>, Status>> + Send {
        let metadata = request.metadata().clone();
        async move {
            if metadata.contains_key("x-goog-request-params") {
                return Err(unsupported(
                    "legacy routing metadata; put bucket and object in the request and omit x-goog-request-params",
                ));
            }
            // `x-goog-user-project` and `CommonRequestParams.user_project` name the Requester
            // Pays billing project, which a gcs-mimic bucket cannot enable, so both are ignored
            // as in the JSON API.
            // gRPC has no trusted listener, so a call without credentials is always anonymous.
            self.state
                .auth
                .authenticate_request(
                    &metadata.into_headers(),
                    None,
                    self.state.clock.as_ref(),
                    ListenerRole::Public,
                )
                .await
                .map_err(super::v2::auth_error)
        }
    }

    async fn access(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        permission: crate::auth::Permission,
    ) -> Result<BucketAccess, Status> {
        crate::bucket::name::validate_for_profile(bucket, self.compat_profile)?;
        crate::server::authorize_bucket(&self.state, caller, permission, bucket)
            .await
            .map_err(|e| Status::from(crate::server::anonymous_error(e, caller)))
    }

    /// The ID of the project `named` names by ID or by number, once the caller may act in it.
    async fn project_id(
        &self,
        caller: Option<Caller<'_>>,
        named: &str,
        permission: crate::auth::Permission,
    ) -> Result<String, Status> {
        crate::auth::validate_project_id(named)?;
        let project = self.state.project_id(named).await?;
        if let Some(caller) = caller {
            caller.authorize(permission, Some(&project))?;
        }
        Ok(project)
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
        object_name(name)?;
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
                ObjectTarget::new(bucket, name, generation(selected)?.map(Generation::get)),
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

/// The legacy RPC handlers, which `legacy_methods!` calls after authentication.
impl Storage {
    async fn get_bucket(
        &self,
        request: Request<pb::GetBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        let requested = bucket_conditions(BucketConditions::from(&r))?;
        let access = self
            .access(caller, &r.bucket, crate::auth::BUCKETS_GET)
            .await?;
        let record = self
            .state
            .catalog
            .get_bucket(&r.bucket, &access)
            .await?
            .ok_or_else(|| {
                Status::not_found("Bucket not found; create it with InsertBucket or JSON")
            })?;
        if !requested.hold(record.metageneration) {
            return Err(Status::failed_precondition(
                "Bucket metageneration condition failed; read current metadata",
            ));
        }
        Ok(Response::new(convert::bucket(&record)))
    }

    async fn delete_bucket(
        &self,
        request: Request<pb::DeleteBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        let requested = bucket_conditions(BucketConditions::from(&r))?;
        let access = self
            .access(caller, &r.bucket, crate::auth::BUCKETS_DELETE)
            .await?;
        self.state
            .catalog
            .delete_bucket(&r.bucket, requested, &access)
            .await?;
        Ok(Response::new(()))
    }

    async fn insert_bucket(
        &self,
        request: Request<pb::InsertBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        if r.predefined_acl != 0 || r.predefined_default_object_acl != 0 {
            return Err(unsupported("legacy bucket predefined ACLs; omit them"));
        }
        let project = self
            .project_id(caller, &r.project, crate::auth::BUCKETS_CREATE)
            .await?;
        let mut bucket = r.bucket.ok_or_else(|| {
            Status::invalid_argument("bucket is required; set bucket.name=example-bucket")
        })?;
        let body = convert::insert_body(&mut bucket)?;
        let record = json::insert_from_object(
            &body,
            &project,
            self.state.clock.now(),
            self.compat_profile,
            &self.state.default_location,
        )?;
        let record = self.state.catalog.insert_bucket(record).await?;
        Ok(Response::new(convert::bucket(&record)))
    }

    async fn update_bucket(
        &self,
        request: Request<pb::UpdateBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        if r.predefined_acl != 0 || r.predefined_default_object_acl != 0 {
            return Err(unsupported("legacy bucket predefined ACLs; omit them"));
        }
        let requested = bucket_conditions(BucketConditions::from(&r))?;
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
            .await?
            .ok_or_else(|| Status::from(CatalogError::NotFound))?;
        if !requested.hold(current.metageneration) {
            return Err(Status::from(CatalogError::PreconditionFailed));
        }
        let update =
            convert::bucket_update(&mut bucket, &convert::bucket(&current), self.compat_profile)?;
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
            .await?;
        Ok(Response::new(convert::bucket(&record)))
    }

    async fn list_buckets(
        &self,
        request: Request<pb::ListBucketsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListBucketsResponse>, Status> {
        let r = request.into_inner();
        common(r.common_request_params.as_ref(), None)?;
        projection(r.projection)?;
        let scope = ProjectScope::of(&r.project);
        let projects = match (scope, caller) {
            (ProjectScope::All, None) => None,
            (ProjectScope::All, Some(caller)) => {
                caller.authorize(BUCKETS_LIST, None)?;
                Some(caller.principal()?.projects_allowing(BUCKETS_LIST))
            }
            (ProjectScope::One(project), caller) => {
                Some(vec![self.project_id(caller, project, BUCKETS_LIST).await?])
            }
        };
        if r.prefix.len() > 222 {
            return Err(Status::invalid_argument("prefix must be at most 222 bytes"));
        }
        let start = if r.page_token.is_empty() {
            None
        } else {
            Some(scope.decode_token(&r.page_token, &r.prefix)?)
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
            .await?;
        let next_page_token = if page.has_more {
            let last = &page
                .items
                .last()
                .ok_or_else(|| Status::internal("Missing continuation bucket"))?
                .name;
            scope.encode_token(&r.prefix, last)?
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
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        projection(r.projection)?;
        object_name(&r.object)?;
        let access = self
            .access(caller, &r.bucket, crate::auth::OBJECTS_GET)
            .await?;
        let target = ObjectTarget::new(
            &r.bucket,
            &r.object,
            generation(r.generation)?.map(Generation::get),
        );
        let record = self
            .state
            .catalog
            .get_object(target, conditions(ObjectConditions::from(&r))?, &access)
            .await
            .map_err(|error| object_access_error(error, caller))?;
        Ok(Response::new(convert::object(&record)?))
    }

    async fn delete_object(
        &self,
        request: Request<pb::DeleteObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        object_name(&r.object)?;
        let access = self
            .access(caller, &r.bucket, crate::auth::OBJECTS_DELETE)
            .await?;
        let target = ObjectTarget::new(
            &r.bucket,
            &r.object,
            generation(r.generation)?.map(Generation::get),
        );
        self.state
            .catalog
            .delete_object(target, conditions(ObjectConditions::from(&r))?, &access)
            .await
            .map_err(|error| object_access_error(error, caller))?;
        Ok(Response::new(()))
    }

    async fn update_object(
        &self,
        request: Request<pb::UpdateObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
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
            conditions: conditions(ObjectConditions::from(&r))?,
            object: r.metadata.unwrap_or_default(),
            write: WriteMode::Replace,
        };
        self.update(caller, update).await
    }

    async fn patch_object(
        &self,
        request: Request<pb::PatchObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
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
            conditions: conditions(ObjectConditions::from(&r))?,
            object: r.metadata.unwrap_or_default(),
            write: WriteMode::Patch,
        };
        self.update(caller, update).await
    }

    async fn list_objects(
        &self,
        request: Request<pb::ListObjectsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListObjectsResponse>, Status> {
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
            page_bytes: Some(crate::grpc::LIST_PAGE_BYTES),
            ..Default::default()
        };
        if !r.page_token.is_empty() {
            query.cursor = Some(page_token::decode_object_page_token(
                &r.page_token, &query,
            ).map_err(|_| Status::invalid_argument(
                "Invalid object listing or page token; use the token with its original query",
            ))?);
        }
        let page = self
            .state
            .catalog
            .list_objects(query, max_results, &access)
            .await?;
        let next_page_token = page
            .next
            .as_ref()
            .map(page_token::encode_object_page_token)
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
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(
            r.common_request_params.as_ref(),
            r.common_object_request_params.as_ref(),
        )?;
        if r.destination_predefined_acl != 0 || !r.kms_key_name.is_empty() {
            return Err(unsupported("legacy compose ACL or KMS; omit these fields"));
        }
        object_name(&r.destination_object)?;
        let access = self
            .access(caller, &r.destination_bucket, crate::auth::OBJECTS_CREATE)
            .await?;
        // Validated after the sources, as before; the request's fields move below.
        let destination_conditions = ObjectConditions::from(&r);
        let object = r.destination.unwrap_or_default();
        if (!object.name.is_empty() && object.name != r.destination_object)
            || (!object.bucket.is_empty() && object.bucket != r.destination_bucket)
        {
            return Err(Status::invalid_argument(
                "Destination metadata bucket/name must match the request",
            ));
        }
        let content_type = object.content_type.clone();
        let metadata = metadata(object, WriteMode::Replace)?;
        let sources = r
            .source_objects
            .into_iter()
            .map(|s| {
                object_name(&s.name)?;
                let preconditions = conditions(
                    s.object_preconditions
                        .as_ref()
                        .map(ObjectConditions::from)
                        .unwrap_or_default(),
                )?;
                Ok(crate::object::ComposeSource {
                    name: s.name,
                    generation: generation(s.generation)?.map(Generation::get),
                    if_generation_match: preconditions.if_generation_match,
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
                    declared: Default::default(),
                    recorded: Default::default(),
                    conditions: conditions(destination_conditions)?,
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
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        self.upload(request.into_inner(), caller)
            .await
            .map(Response::new)
            .map_err(|status| crate::grpc::v2::status::anonymous(status, caller))
    }
}
