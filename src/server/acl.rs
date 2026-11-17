//! JSON ACL handlers and the ACL fields of `projection=full` resources.

use super::RequestBody as Incoming;
use super::{
    QueryPrecondition, State, ValidatedRead, authorize_bucket, bucket_access, empty_response,
    json_response_selected, read_metadata_response, require_empty_body,
};
use crate::{
    acl::{AclEntry, AclKind, AclTarget},
    auth::{self, Caller},
    bucket::{BucketAccess, BucketConditions, BucketRecord},
    json::{
        self, JsonError,
        acl::{AclAccess, AclOperation, AclOwner, AclRequest},
    },
    object::{ObjectRecord, owner::ObjectOwner},
    storage::{AclChange, AclError, AclProjection, AclState, ObjectAcl},
};
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Bytes};
use serde_json::{Map, Value};

fn permission(kind: AclKind, access: AclAccess) -> auth::Permission {
    match (kind, access) {
        (AclKind::Object, AclAccess::Read) => auth::OBJECTS_GET_IAM,
        (AclKind::Object, AclAccess::Write) => auth::OBJECTS_SET_IAM,
        (AclKind::Bucket | AclKind::DefaultObject, AclAccess::Read) => auth::BUCKETS_GET_IAM,
        (AclKind::Bucket | AclKind::DefaultObject, AclAccess::Write) => auth::BUCKETS_SET_IAM,
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
        } => entity_not_found(&entity, project_number),
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
fn entity_not_found(entity: &str, project_number: crate::project::ProjectNumber) -> JsonError {
    if let Ok(entity) = crate::acl::parse_entity(entity)
        && let crate::acl::EntityRef::ProjectTeam { project, .. } = entity.parsed()
        && project != project_number.to_string()
    {
        return JsonError::invalid(format!("Project \"{project}\" was not found."));
    }
    JsonError::new(
        StatusCode::NOT_FOUND,
        "notFound",
        "The specified key does not exist.",
    )
}

/// The ACL a list or get reads, with its errors in the read's wording.
async fn read(
    state: &State,
    target: &AclTarget,
    conditions: BucketConditions,
    access: &BucketAccess,
) -> Result<AclState, JsonError> {
    state
        .catalog
        .read_acl(target.clone(), conditions, access)
        .await
        .map_err(|error| naming_object(acl_error(error, AclAccess::Read), target))
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
    let acl_access = operation.access();
    let access = authorize_bucket(
        state,
        caller,
        permission(target.kind(), acl_access),
        target.bucket(),
    )
    .await?;
    if !operation.accepts_body() {
        require_empty_body(request.headers())?;
    }
    let mut value = match &operation {
        AclOperation::List => {
            let acl = read(state, &target, conditions, &access).await?;
            let owner = AclOwner::of(&acl.target, acl.metageneration);
            // The listing carries its owner's entity tag, and Cloud Storage evaluated
            // `If-None-Match` and `If-Match` against it (case bkt-probe-collection-validators).
            return read_metadata_response(
                request.headers(),
                &owner.etag(),
                json::acl::acl_list_resource(base, owner, &acl.entries),
                &fields,
                QueryPrecondition::Unmatched,
                ValidatedRead::AclList,
            );
        }
        AclOperation::Get(entity) => {
            let acl = read(state, &target, conditions, &access).await?;
            let owner = AclOwner::of(&acl.target, acl.metageneration);
            let entry = acl
                .entries
                .iter()
                .find(|entry| entry.entity.as_str() == entity)
                .ok_or_else(|| {
                    acl_error(
                        AclError::EntityNotFound {
                            entity: entity.clone(),
                            project_number: acl.project_number,
                        },
                        AclAccess::Read,
                    )
                })?;
            json::acl::acl_entry_resource(base, owner, entry)
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
            if let Ok(etag) = AclOwner::of(&acl.target, acl.metageneration).etag().parse() {
                response.headers_mut().insert("etag", etag);
            }
            return Ok(response);
        }
        AclOperation::Write(write) => {
            let bytes = json::read_body(request.into_body(), super::RESOURCE_BODY_LIMIT).await?;
            let body = json::acl::parse_acl_body(&bytes, target.kind(), write)?;
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
            json::acl::acl_entry_resource(
                base,
                AclOwner::of(&acl.target, acl.metageneration),
                &entry,
            )
        }
    };
    // An entry resource carries its owner's `etag`, which becomes the ETag header.
    Ok(json_response_selected(
        &mut value,
        &fields,
        json::ResponseCache::Unrecorded,
    ))
}

/// A bucket at the metageneration a response rendered it.
pub(super) struct BucketVersion {
    name: String,
    metageneration: i64,
}

impl From<&BucketRecord> for BucketVersion {
    fn from(record: &BucketRecord) -> Self {
        Self {
            name: record.name.clone(),
            metageneration: record.metageneration,
        }
    }
}

/// An object generation at the metageneration a response rendered it.
pub(super) struct ObjectVersion {
    pub(super) bucket: String,
    pub(super) name: String,
    pub(super) generation: i64,
    metageneration: i64,
}

impl From<&ObjectRecord> for ObjectVersion {
    fn from(record: &ObjectRecord) -> Self {
        Self {
            bucket: record.bucket.clone(),
            name: record.name.clone(),
            generation: record.generation,
            metageneration: record.metageneration,
        }
    }
}

impl ObjectVersion {
    /// The version a stored object resource describes, read from its own identity fields. A
    /// completed resumable session and a finished rewrite keep only the resource they first
    /// answered with, not its record, so this is the one place a version is read back out of
    /// JSON. `None` when a field selection left an identity field out, as it could for a session
    /// completed before selections were applied per response.
    fn of_stored(resource: &Value) -> Option<Self> {
        let text = |field: &str| resource.get(field)?.as_str();
        let number = |field: &str| text(field)?.parse().ok();
        Some(Self {
            bucket: text("bucket")?.to_owned(),
            name: text("name")?.to_owned(),
            generation: number("generation")?,
            metageneration: number("metageneration")?,
        })
    }

    /// The owner whose ids and etags the object's ACL entries carry.
    fn acl_owner(&self) -> AclOwner<'_> {
        AclOwner::Object {
            bucket: &self.bucket,
            name: &self.name,
            generation: self.generation,
            metageneration: self.metageneration,
        }
    }
}

/// The resources a rendered JSON response names, recorded from the records it was rendered from,
/// so `projection=full` adds ACL fields without reading names and generations back out of JSON.
pub(super) enum AclSubjects {
    /// The response carries no ACL fields: a policy, a permission test, a storage layout, a
    /// soft-deleted version or a listing that renders no items.
    None,
    Bucket(BucketVersion),
    /// One per `items` element, in order.
    Buckets(Vec<BucketVersion>),
    Object(ObjectVersion),
    /// One listing's objects, one per `items` element, in order.
    Objects {
        bucket: String,
        items: Vec<ObjectVersion>,
    },
}

/// A rendered response and what `projection=full` adds ACL fields for.
pub(super) struct Rendered {
    pub(super) value: Value,
    pub(super) subjects: AclSubjects,
}

/// Add ACL fields to rendered bucket/object resources or lists.
///
/// The fields are omitted when uniform bucket-level access is enabled or when a strict caller
/// lacks the permission that reading the ACL itself requires; the request still succeeds.
pub(super) async fn add_full_projection(
    state: &State,
    caller: Option<Caller<'_>>,
    rendered: &mut Rendered,
    base_url: &str,
) -> Result<(), JsonError> {
    let Rendered { value, subjects } = rendered;
    match &*subjects {
        AclSubjects::None => Ok(()),
        AclSubjects::Bucket(bucket) => add_bucket(state, caller, value, bucket, base_url).await,
        AclSubjects::Buckets(buckets) => {
            let items = rendered_items(value, buckets.len())?;
            for (item, bucket) in items.iter_mut().zip(buckets) {
                add_bucket(state, caller, item, bucket, base_url).await?;
            }
            Ok(())
        }
        AclSubjects::Object(object) => {
            add_object(state, AclReader::Caller(caller), value, object, base_url).await
        }
        AclSubjects::Objects { bucket, items } => {
            let resources = rendered_items(value, items.len())?;
            add_objects(
                state,
                AclReader::Caller(caller),
                bucket,
                resources,
                items,
                base_url,
            )
            .await
        }
    }
}

/// The `items` of a rendered list, which holds one element per subject and none when there are
/// no subjects.
fn rendered_items(value: &mut Value, subjects: usize) -> Result<&mut [Value], JsonError> {
    if subjects == 0 {
        return Ok(&mut []);
    }
    match value.get_mut("items").and_then(Value::as_array_mut) {
        Some(items) if items.len() == subjects => Ok(items),
        // Both come from one page, so they differ only through a rendering bug.
        Some(_) | None => Err(JsonError::service_unavailable(
            "Rendered items differ from their ACL subjects",
        )),
    }
}

/// The members of a rendered resource, which every resource renderer builds as a JSON object.
fn resource_members(value: &mut Value) -> Result<&mut Map<String, Value>, JsonError> {
    value
        .as_object_mut()
        .ok_or_else(|| JsonError::service_unavailable("Rendered resource must be an object"))
}

pub(crate) async fn may_read(
    state: &State,
    caller: Option<Caller<'_>>,
    kind: AclKind,
    bucket: &str,
) -> Result<bool, JsonError> {
    match bucket_access(state, caller, permission(kind, AclAccess::Read), bucket).await {
        Ok(access) => state
            .catalog
            .has_bucket_permission(bucket, &access)
            .await
            .map_err(Into::into),
        Err(error) if error.is_refusal() => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn add_bucket(
    state: &State,
    caller: Option<Caller<'_>>,
    value: &mut Value,
    bucket: &BucketVersion,
    base_url: &str,
) -> Result<(), JsonError> {
    if !may_read(state, caller, AclKind::Bucket, &bucket.name).await? {
        return Ok(());
    }
    let Some(acl) = state
        .catalog
        .acl_projection(&bucket.name, Vec::new())
        .await?
    else {
        return Ok(());
    };
    if acl.uniform_access {
        // ACLs stay stored for a later UBLA disable, but they are not part of the resource, and
        // Cloud Storage reports no `owner` either (recorded 2026-10-31).
        return Ok(());
    }
    // The rendered metageneration belongs to the response; a later ACL change has its own etag.
    json::acl::add_bucket_acl_fields(
        resource_members(value)?,
        base_url,
        &bucket.name,
        bucket.metageneration,
        &acl,
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
    object_acls(state, AclReader::Caller(caller), bucket, generations).await
}

/// Whose permission decides whether a response carries the ACL fields of `projection=full`.
#[derive(Clone, Copy)]
pub(super) enum AclReader<'a> {
    /// The request's own caller.
    Caller(Option<Caller<'a>>),
    /// The initiator of a resumable session, as its stored access. A session URI is a capability
    /// that acts for its initiator, so requests on it carry no caller of their own, and passing
    /// none would read the ACL without any permission check.
    Initiator(&'a BucketAccess),
}

impl AclReader<'_> {
    /// The access that reads object ACLs in `bucket` as this reader.
    async fn object_acl_access(
        self,
        state: &State,
        bucket: &str,
    ) -> Result<BucketAccess, JsonError> {
        let read = permission(AclKind::Object, AclAccess::Read);
        match self {
            Self::Caller(caller) => authorize_bucket(state, caller, read, bucket).await,
            Self::Initiator(access) => Ok(access.with_permission(read)),
        }
    }
}

async fn object_acls(
    state: &State,
    reader: AclReader<'_>,
    bucket: &str,
    generations: Vec<(String, i64)>,
) -> Result<Option<AclProjection>, JsonError> {
    let access = reader.object_acl_access(state, bucket).await?;
    let acl = state
        .catalog
        .object_acl_projection(bucket, generations, &access)
        .await?;
    Ok(acl.filter(|acl| !acl.uniform_access))
}

/// Adds `projection=full`'s ACL fields, as the object has them now and as `reader` may read
/// them, to the stored resource of a completed resumable upload or a finished rewrite (see
/// [`ObjectVersion::of_stored`]).
pub(super) async fn add_full_projection_to_stored(
    state: &State,
    reader: AclReader<'_>,
    resource: &mut Value,
    base_url: &str,
) -> Result<(), JsonError> {
    let Some(object) = ObjectVersion::of_stored(resource) else {
        return Ok(());
    };
    add_object(state, reader, resource, &object, base_url).await
}

async fn add_object(
    state: &State,
    reader: AclReader<'_>,
    resource: &mut Value,
    object: &ObjectVersion,
    base_url: &str,
) -> Result<(), JsonError> {
    add_objects(
        state,
        reader,
        &object.bucket,
        std::slice::from_mut(resource),
        std::slice::from_ref(object),
        base_url,
    )
    .await
}

/// `resources` holds one rendered resource per element of `objects`, in the same order.
async fn add_objects(
    state: &State,
    reader: AclReader<'_>,
    bucket: &str,
    resources: &mut [Value],
    objects: &[ObjectVersion],
    base_url: &str,
) -> Result<(), JsonError> {
    if objects.is_empty() {
        return Ok(());
    }
    let generations = objects
        .iter()
        .map(|object| (object.name.clone(), object.generation))
        .collect();
    let Some(acl) = object_acls(state, reader, bucket, generations).await? else {
        return Ok(());
    };
    for ((resource, object), object_acl) in resources.iter_mut().zip(objects).zip(&acl.objects) {
        let ObjectAcl::Visible { entries, owner } = object_acl else {
            continue;
        };
        add_object_acl(resource, base_url, object, entries, owner.as_ref())?;
    }
    Ok(())
}

/// Add `acl`, and `owner` when the object has an owner entity, to a rendered object resource.
pub(super) fn add_object_acl(
    resource: &mut Value,
    base_url: &str,
    object: &ObjectVersion,
    entries: &[AclEntry],
    owner: Option<&ObjectOwner>,
) -> Result<(), JsonError> {
    let resource = resource_members(resource)?;
    json::acl::add_object_acl_fields(resource, base_url, object.acl_owner(), entries);
    if let Some(entity) = owner.and_then(ObjectOwner::entity) {
        resource.insert(
            "owner".into(),
            Value::Object(Map::from_iter([(
                "entity".into(),
                Value::from(entity.as_str()),
            )])),
        );
    }
    Ok(())
}

/// Cloud Storage names the requested object in its not-found message.
fn naming_object(error: JsonError, target: &AclTarget) -> JsonError {
    match target {
        AclTarget::Object { bucket, name, .. } => error.naming_object(bucket, name),
        AclTarget::Bucket(_) | AclTarget::DefaultObject(_) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::ObjectVersion;
    use serde_json::json;

    /// A stored resource names its version only with every identity field; one a field selection
    /// trimmed names none, so its response goes without ACL fields instead of failing.
    #[test]
    fn a_stored_resource_names_its_version_only_with_every_identity_field() {
        let stored =
            json!({"bucket":"b","name":"o","generation":"7","metageneration":"2","size":"5"});
        let version = ObjectVersion::of_stored(&stored).unwrap();
        assert_eq!(
            (
                version.bucket.as_str(),
                version.name.as_str(),
                version.generation,
                version.metageneration
            ),
            ("b", "o", 7, 2)
        );
        for field in ["bucket", "name", "generation", "metageneration"] {
            let mut trimmed = stored.clone();
            trimmed.as_object_mut().unwrap().remove(field);
            assert!(ObjectVersion::of_stored(&trimmed).is_none(), "{field}");
        }
        let unnumbered = json!({"bucket":"b","name":"o","generation":"x","metageneration":"1"});
        assert!(ObjectVersion::of_stored(&unnumbered).is_none());
    }
}
