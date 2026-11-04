//! JSON ACL handlers and the ACL fields of `projection=full` resources.

use super::RequestBody as Incoming;
use super::{JsonCache, State, authorize_bucket, empty_response, json_response_selected};
use crate::{
    acl::{AclKind, AclTarget},
    auth::{self, Caller},
    json::{
        self, JsonError,
        acl::{AclOperation, AclOwner, AclRequest},
    },
    storage::{AclChange, AclError, AclProjection, AclState},
};
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Bytes};
use serde_json::Value;

/// Whether an ACL request reads or changes the ACL.
#[derive(Clone, Copy)]
enum AclAccess {
    Read,
    Write,
}

fn permission(kind: AclKind, access: AclAccess) -> auth::Permission {
    match (kind, access) {
        (AclKind::Object, AclAccess::Read) => auth::OBJECTS_GET_IAM,
        (AclKind::Object, AclAccess::Write) => auth::OBJECTS_SET_IAM,
        (_, AclAccess::Read) => auth::BUCKETS_GET_IAM,
        (_, AclAccess::Write) => auth::BUCKETS_SET_IAM,
    }
}

fn acl_error(error: AclError, access: AclAccess) -> JsonError {
    match error {
        AclError::Bucket(error) => error.into(),
        AclError::Object(error) => error.into(),
        // Cloud Storage words the refusal of a read; the write wording was not recorded, so a
        // write reuses the bucket update refusal.
        AclError::UniformAccess => JsonError::invalid(match access {
            AclAccess::Read => UNIFORM_ACCESS_GET_ACL,
            AclAccess::Write => UNIFORM_ACCESS_UPDATE_ACL,
        }),
        AclError::EntityNotFound {
            entity,
            project_number,
        } => entity_not_found(&entity, &project_number),
        AclError::RoleRequired => JsonError::required("Access control must contain a role"),
        AclError::TooManyEntries => JsonError::invalid(format!(
            "An ACL can hold at most {} entries; delete an entry before adding another",
            crate::acl::MAX_ENTRIES
        )),
        AclError::NotModified => JsonError::not_modified(),
    }
}

const UNIFORM_ACCESS_GET_ACL: &str = "Cannot get legacy ACL for a bucket that has uniform bucket-level access. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access";
const UNIFORM_ACCESS_UPDATE_ACL: &str = "Cannot use ACL API to update bucket policy when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access";

/// A missing entity. A project-team entity that names another project is a bad request about
/// that project, and any other missing entity is a missing key (recorded 2026-10-31).
fn entity_not_found(entity: &str, project_number: &str) -> JsonError {
    if let Ok(crate::acl::Entity::ProjectTeam { project, .. }) = crate::acl::Entity::parse(entity)
        && project != project_number
    {
        return JsonError::invalid(format!("Project \"{project}\" was not found."));
    }
    JsonError::new(
        StatusCode::NOT_FOUND,
        "notFound",
        "The specified key does not exist.",
    )
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

fn read_resource(
    base: &str,
    target: &AclTarget,
    acl: &AclState,
    operation: &AclOperation,
) -> Result<Value, JsonError> {
    if let AclOperation::Get(entity) = operation {
        let entry = acl
            .entries
            .iter()
            .find(|entry| &entry.entity == entity)
            .ok_or_else(|| {
                acl_error(
                    AclError::EntityNotFound {
                        entity: entity.clone(),
                        project_number: acl.project_number.clone(),
                    },
                    AclAccess::Read,
                )
            })?;
        Ok(json::acl::acl_entry_resource(
            base,
            owner(target, acl),
            entry,
        ))
    } else {
        Ok(json::acl::acl_list_resource(
            base,
            owner(target, acl),
            &acl.entries,
        ))
    }
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: AclRequest,
    caller: Option<Caller<'_>>,
    state: &State,
    base: &str,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let AclRequest {
        target,
        operation,
        conditions,
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
    let mut etag = None;
    let mut value = match &operation {
        AclOperation::List | AclOperation::Get(_) => {
            let acl = state
                .catalog
                .read_acl(target.clone(), conditions, &access)
                .await
                .map_err(|error| naming_object(acl_error(error, acl_access), &target))?;
            etag = Some(json::revision_etag(
                acl.object.as_ref().map(|(_, generation)| *generation),
                acl.metageneration,
            ));
            read_resource(base, &target, &acl, &operation)?
        }
        AclOperation::Delete(entity) => {
            let (acl, _) = state
                .catalog
                .change_acl(
                    target.clone(),
                    AclChange::Delete {
                        entity: entity.clone(),
                    },
                    &access,
                )
                .await
                .map_err(|error| naming_object(acl_error(error, acl_access), &target))?;
            let mut response = empty_response(StatusCode::NO_CONTENT);
            if let Ok(etag) = json::revision_etag(
                acl.object.as_ref().map(|(_, generation)| *generation),
                acl.metageneration,
            )
            .parse()
            {
                response.headers_mut().insert("etag", etag);
            }
            return Ok(response);
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
                .map_err(|error| naming_object(acl_error(error, acl_access), &target))?;
            // The catalog returns the entry that every upsert writes.
            let entry = entry
                .ok_or_else(|| JsonError::service_unavailable("ACL change returned no entry"))?;
            json::acl::acl_entry_resource(base, owner(&target, &acl), &entry)
        }
    };
    let mut response = json_response_selected(&mut value, &fields, JsonCache::Unrecorded);
    if let Some(etag) = etag.and_then(|etag| etag.parse().ok()) {
        response.headers_mut().insert("etag", etag);
    }
    Ok(response)
}

/// Add ACL fields to rendered bucket/object resources or lists.
///
/// The fields are omitted when uniform bucket-level access is enabled or when a strict caller
/// lacks the permission that reading the ACL itself requires; the request still succeeds.
pub(super) async fn add_full_projection(
    state: &State,
    caller: Option<Caller<'_>>,
    value: &mut Value,
    base_url: &str,
) -> Result<(), JsonError> {
    match value["kind"].as_str() {
        Some("storage#bucket") => add_bucket(state, caller, value, base_url).await,
        Some("storage#buckets") => {
            if let Some(items) = value["items"].as_array_mut() {
                for item in items {
                    add_bucket(state, caller, item, base_url).await?;
                }
            }
            Ok(())
        }
        Some("storage#object") => {
            let Some(bucket) = value["bucket"].as_str().map(str::to_owned) else {
                return Ok(());
            };
            add_objects(
                state,
                caller,
                &bucket,
                std::slice::from_mut(value),
                base_url,
            )
            .await
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
            add_objects(state, caller, &bucket, items, base_url).await
        }
        _ => Ok(()),
    }
}

pub(crate) async fn may_read(
    state: &State,
    caller: Option<Caller<'_>>,
    kind: AclKind,
    bucket: &str,
) -> Result<bool, JsonError> {
    match authorize_bucket(state, caller, permission(kind, AclAccess::Read), bucket).await {
        Ok(access) => state
            .catalog
            .has_bucket_permission(bucket, &access)
            .await
            .map_err(Into::into),
        Err(error) if error.status == StatusCode::FORBIDDEN => Ok(false),
        Err(error) => Err(error),
    }
}

async fn add_bucket(
    state: &State,
    caller: Option<Caller<'_>>,
    value: &mut Value,
    base_url: &str,
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
        // ACLs stay stored for a later UBLA disable, but they are not part of the resource, and
        // Cloud Storage reports no `owner` either (recorded 2026-10-31).
        return Ok(());
    }
    json::acl::add_bucket_acl_fields(
        value,
        base_url,
        &acl.project,
        metageneration,
        &acl.bucket,
        &acl.default_object,
    );
    Ok(())
}

/// The ACLs and owners of the selected object generations, in input order, for every API that
/// renders them as resource fields (JSON `projection=full`, gRPC read masks).
///
/// `None` when the bucket is gone or uses uniform bucket-level access, where ACLs are not part
/// of the resource. An object whose ACL the caller may not read is marked invisible.
pub(crate) async fn readable_object_acls(
    state: &State,
    caller: Option<Caller<'_>>,
    bucket: &str,
    generations: Vec<(String, i64)>,
) -> Result<Option<AclProjection>, JsonError> {
    let access = authorize_bucket(
        state,
        caller,
        permission(AclKind::Object, AclAccess::Read),
        bucket,
    )
    .await?;
    let acl = state
        .catalog
        .object_acl_projection(bucket, generations, &access)
        .await?;
    Ok(acl.filter(|acl| !acl.uniform_access))
}

async fn add_objects(
    state: &State,
    caller: Option<Caller<'_>>,
    bucket: &str,
    items: &mut [Value],
    base_url: &str,
) -> Result<(), JsonError> {
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
    let Some(acl) = readable_object_acls(state, caller, bucket, generations).await? else {
        return Ok(());
    };
    for (((item, entries), owner), visible) in items
        .iter_mut()
        .zip(&acl.objects)
        .zip(&acl.owners)
        .zip(&acl.visible)
    {
        if !visible {
            continue;
        }
        json::acl::add_object_acl_fields(item, base_url, entries);
        if let Some(entity) = owner.as_ref().and_then(|owner| owner.entity()) {
            item["owner"] = serde_json::json!({"entity":entity});
        }
    }
    Ok(())
}

/// Cloud Storage names the requested object in its not-found message.
fn naming_object(error: JsonError, target: &AclTarget) -> JsonError {
    match target {
        AclTarget::Object { bucket, name, .. } => error.naming_object(bucket, name),
        _ => error,
    }
}
