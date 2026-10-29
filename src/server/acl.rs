//! JSON ACL handlers and the ACL fields of `projection=full` resources.

use super::{State, authorize_bucket, empty_response, json_response};
use crate::{
    acl::{AclKind, AclTarget},
    auth::{self, Caller},
    json::{
        self, JsonError,
        acl::{AclOperation, AclOwner, AclRequest},
    },
    storage::{AclChange, AclError, AclState},
};
use http_body_util::Full;
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
};
use serde_json::Value;

/// Whether an ACL request reads or changes the ACL.
#[derive(Clone, Copy)]
enum AclAccess {
    Read,
    Write,
}

fn permission(kind: AclKind, access: AclAccess) -> &'static str {
    match (kind, access) {
        (AclKind::Object, AclAccess::Read) => auth::OBJECTS_GET_IAM,
        (AclKind::Object, AclAccess::Write) => auth::OBJECTS_SET_IAM,
        (_, AclAccess::Read) => auth::BUCKETS_GET_IAM,
        (_, AclAccess::Write) => auth::BUCKETS_SET_IAM,
    }
}

fn acl_error(error: AclError, target: &AclTarget) -> JsonError {
    match error {
        AclError::Bucket(error) => error.into(),
        AclError::Object(error) => error.into(),
        // Exact service reason and message are unverified; `invalid` matches other 400s for
        // operations that uniform bucket-level access disables.
        AclError::UniformAccess => JsonError::new(
            StatusCode::BAD_REQUEST,
            "invalid",
            format!(
                "Cannot use legacy ACLs on bucket {} because uniform bucket-level access is enabled; manage access with bucket IAM policies (for example, PUT /storage/v1/b/{}/iam)",
                target.bucket(),
                target.bucket()
            ),
        ),
        AclError::EntityNotFound(entity) => JsonError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            format!(
                "No ACL entry exists for entity {entity}; list the ACL to see current entities"
            ),
        ),
        AclError::RoleRequired => JsonError::new(
            StatusCode::BAD_REQUEST,
            "required",
            "Required field role is missing for an entity without an ACL entry; for example, send {\"role\":\"READER\"}",
        ),
        AclError::TooManyEntries => JsonError::new(
            StatusCode::BAD_REQUEST,
            "invalid",
            format!(
                "An ACL can hold at most {} entries; delete an entry before adding another",
                crate::acl::MAX_ENTRIES
            ),
        ),
        AclError::NotModified => {
            JsonError::new(StatusCode::NOT_MODIFIED, "notModified", "Not modified")
        }
    }
}

fn owner<'a>(target: &'a AclTarget, state: &'a AclState) -> AclOwner<'a> {
    AclOwner {
        kind: target.kind(),
        bucket: target.bucket(),
        object: state
            .object
            .as_ref()
            .map(|(name, generation)| (name.as_str(), *generation)),
        metageneration: state.metageneration,
    }
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: AclRequest,
    caller: Option<Caller<'_>>,
    state: &State,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let AclRequest {
        target,
        operation,
        if_metageneration_match,
        if_metageneration_not_match,
        fields,
    } = operation;
    let acl_access = match operation {
        AclOperation::List | AclOperation::Get(_) => AclAccess::Read,
        _ => AclAccess::Write,
    };
    let access = authorize_bucket(
        state,
        caller,
        permission(target.kind(), acl_access),
        target.bucket(),
    )
    .await?;
    let mutation = matches!(
        operation,
        AclOperation::Insert | AclOperation::Patch(_) | AclOperation::Update(_)
    );
    if !mutation
        && (request.headers().contains_key("transfer-encoding")
            || request
                .headers()
                .get("content-length")
                .is_some_and(|value| value != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    let base = &state.api_base_url;
    let mut value = match &operation {
        AclOperation::List | AclOperation::Get(_) => {
            let acl = state
                .catalog
                .read_acl(
                    target.clone(),
                    if_metageneration_match,
                    if_metageneration_not_match,
                    &access,
                )
                .await
                .map_err(|error| acl_error(error, &target))?;
            match &operation {
                AclOperation::Get(entity) => {
                    let entry = acl
                        .entries
                        .iter()
                        .find(|entry| &entry.entity == entity)
                        .ok_or_else(|| {
                            acl_error(AclError::EntityNotFound(entity.clone()), &target)
                        })?;
                    json::acl::acl_entry_resource(base, owner(&target, &acl), entry)
                }
                _ => json::acl::acl_list_resource(base, owner(&target, &acl), &acl.entries),
            }
        }
        AclOperation::Delete(entity) => {
            state
                .catalog
                .change_acl(
                    target.clone(),
                    AclChange::Delete {
                        entity: entity.clone(),
                    },
                    &access,
                )
                .await
                .map_err(|error| acl_error(error, &target))?;
            return Ok(empty_response(StatusCode::NO_CONTENT));
        }
        AclOperation::Insert | AclOperation::Patch(_) | AclOperation::Update(_) => {
            let bytes = json::read_body(request.into_body(), super::BUCKET_BODY_LIMIT).await?;
            let body = json::acl::parse_acl_body(&bytes, target.kind(), &operation)?;
            let (acl, entry) = state
                .catalog
                .change_acl(
                    target.clone(),
                    AclChange::Upsert {
                        entity: body.entity,
                        role: body.role,
                    },
                    &access,
                )
                .await
                .map_err(|error| acl_error(error, &target))?;
            // The catalog returns the entry that every upsert writes.
            let entry = entry.ok_or_else(|| {
                JsonError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "ACL change returned no entry",
                )
            })?;
            json::acl::acl_entry_resource(base, owner(&target, &acl), &entry)
        }
    };
    fields.apply(&mut value);
    Ok(json_response(&value))
}

/// Whether a bucket or object request renders `projection=full`; PUT defaults to full.
pub(super) fn full_projection(method: &Method, query: Option<&str>) -> bool {
    let projection = json::parse_query(query.unwrap_or_default())
        .ok()
        .and_then(|mut query| query.remove("projection"));
    match projection.as_deref() {
        Some(projection) => projection == "full",
        None => method == Method::PUT,
    }
}

/// Add ACL fields to rendered bucket/object resources or lists.
///
/// The fields are omitted when uniform bucket-level access is enabled or when a strict caller
/// lacks the permission that reading the ACL itself requires; the request still succeeds.
pub(super) async fn add_full_projection(
    state: &State,
    caller: Option<Caller<'_>>,
    value: &mut Value,
) -> Result<(), JsonError> {
    match value["kind"].as_str() {
        Some("storage#bucket") => add_bucket(state, caller, value).await,
        Some("storage#buckets") => {
            if let Some(items) = value["items"].as_array_mut() {
                for item in items {
                    add_bucket(state, caller, item).await?;
                }
            }
            Ok(())
        }
        Some("storage#object") => {
            let Some(bucket) = value["bucket"].as_str().map(str::to_owned) else {
                return Ok(());
            };
            add_objects(state, caller, &bucket, std::slice::from_mut(value)).await
        }
        Some("storage#objects") => {
            let Some(items) = value["items"].as_array_mut() else {
                return Ok(());
            };
            let Some(bucket) = items
                .first()
                .and_then(|item| item["bucket"].as_str())
                .map(str::to_owned)
            else {
                return Ok(());
            };
            add_objects(state, caller, &bucket, items).await
        }
        _ => Ok(()),
    }
}

pub(super) async fn may_read(
    state: &State,
    caller: Option<Caller<'_>>,
    kind: AclKind,
    bucket: &str,
) -> Result<bool, JsonError> {
    match authorize_bucket(state, caller, permission(kind, AclAccess::Read), bucket).await {
        Ok(_) => Ok(true),
        Err(error) if error.status == StatusCode::FORBIDDEN => Ok(false),
        Err(error) => Err(error),
    }
}

async fn add_bucket(
    state: &State,
    caller: Option<Caller<'_>>,
    value: &mut Value,
) -> Result<(), JsonError> {
    let Some(bucket) = value["name"].as_str().map(str::to_owned) else {
        return Ok(());
    };
    if !may_read(state, caller, AclKind::Bucket, &bucket).await? {
        return Ok(());
    }
    let Some(acl) = state.catalog.acl_projection(&bucket, Vec::new()).await? else {
        return Ok(());
    };
    // The rendered metageneration belongs to the response; a later ACL change has its own etag.
    let metageneration = value["metageneration"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .unwrap_or(acl.bucket_metageneration);
    if acl.uniform_access {
        // ACLs stay stored for a later UBLA disable, but they are not part of the resource.
        value["owner"] = serde_json::json!({
            "entity": format!("project-owners-{}", crate::acl::project_number(&acl.project))
        });
        return Ok(());
    }
    json::acl::add_bucket_acl_fields(
        value,
        &state.api_base_url,
        &acl.project,
        metageneration,
        &acl.bucket,
        &acl.default_object,
    );
    Ok(())
}

async fn add_objects(
    state: &State,
    caller: Option<Caller<'_>>,
    bucket: &str,
    items: &mut [Value],
) -> Result<(), JsonError> {
    if !may_read(state, caller, AclKind::Object, bucket).await? {
        return Ok(());
    }
    let generations: Vec<(String, i64)> = items
        .iter()
        .filter_map(|item| {
            Some((
                item["name"].as_str()?.to_owned(),
                item["generation"].as_str()?.parse().ok()?,
            ))
        })
        .collect();
    if generations.len() != items.len() {
        return Ok(());
    }
    let Some(acl) = state.catalog.acl_projection(bucket, generations).await? else {
        return Ok(());
    };
    if acl.uniform_access {
        return Ok(());
    }
    for (item, entries) in items.iter_mut().zip(&acl.objects) {
        json::acl::add_object_acl_fields(item, &state.api_base_url, entries);
    }
    Ok(())
}
