//! JSON ACL handlers and the ACL fields of `projection=full` resources.

use super::{State, authorize_bucket, empty_response};
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

fn permission(kind: AclKind, read: bool) -> &'static str {
    match (kind, read) {
        (AclKind::Object, true) => auth::OBJECTS_GET_IAM,
        (AclKind::Object, false) => auth::OBJECTS_SET_IAM,
        (_, true) => auth::BUCKETS_GET_IAM,
        (_, false) => auth::BUCKETS_SET_IAM,
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
    let read = matches!(operation, AclOperation::List | AclOperation::Get(_));
    let access = authorize_bucket(
        state,
        caller,
        permission(target.kind(), read),
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
            let entry = entry.expect("an upsert returns its entry");
            json::acl::acl_entry_resource(base, owner(&target, &acl), &entry)
        }
    };
    fields.apply(&mut value);
    Ok(Response::builder()
        .header("content-type", "application/json; charset=UTF-8")
        .body(Full::new(Bytes::from(value.to_string())))
        .unwrap())
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
    match authorize_bucket(state, caller, permission(kind, true), bucket).await {
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

/// Remove `acl`/`defaultObjectAcl` resource fields that repeat the current effective ACL.
///
/// Clients that read a `projection=full` resource and send it back through PATCH/PUT echo these
/// fields. An identical echo changes nothing, so it is accepted; any other value stays in the
/// body and the metadata parser keeps rejecting it, because replacing ACLs through resource
/// metadata is not implemented. The comparison is a separate read before the metadata update.
///
/// Only callers who may read the ACL get the comparison. Otherwise accepting an identical echo
/// and rejecting any other value would let an update-only caller probe the stored entries, so
/// the field stays and is rejected whatever the stored ACL is.
pub(super) async fn strip_echoed_acls(
    state: &State,
    caller: Option<Caller<'_>>,
    bytes: Vec<u8>,
    fields: &[(&str, AclTarget)],
    access: &crate::bucket::BucketAccess,
) -> Result<Vec<u8>, JsonError> {
    let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(bytes);
    };
    let mut changed = false;
    for (field, target) in fields {
        let Some(echoed) = body.get(*field).and_then(entry_pairs) else {
            continue;
        };
        if !may_read(state, caller, target.kind(), target.bucket()).await? {
            continue;
        }
        // Missing resources, uniform access and denials keep the field for the usual errors.
        let current = match state
            .catalog
            .read_acl(target.clone(), None, None, access)
            .await
        {
            Ok(current) => current,
            Err(AclError::UniformAccess | AclError::Bucket(_) | AclError::Object(_)) => continue,
            Err(error) => return Err(acl_error(error, target)),
        };
        let mut current: Vec<(String, String)> = current
            .entries
            .into_iter()
            .map(|entry| (entry.entity, entry.role))
            .collect();
        current.sort();
        if current == echoed {
            body.remove(*field);
            changed = true;
        }
    }
    Ok(if changed {
        Value::Object(body).to_string().into_bytes()
    } else {
        bytes
    })
}

fn entry_pairs(value: &Value) -> Option<Vec<(String, String)>> {
    let mut pairs = value
        .as_array()?
        .iter()
        .map(|entry| {
            Some((
                entry.get("entity")?.as_str()?.to_owned(),
                entry.get("role")?.as_str()?.to_owned(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    pairs.sort();
    Some(pairs)
}
