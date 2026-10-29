//! JSON `bucketAccessControls`, `defaultObjectAccessControls` and `objectAccessControls`
//! routes, request bodies and resources, plus the ACL fields of `projection=full` resources.

use super::{
    Fields, JsonError,
    buckets::encode_segment,
    decode,
    objects::base64,
    parse_query,
    query::{common, finish},
    validate_bucket_name_for_profile, validate_object_name,
};
use crate::{
    acl::{AclEntry, AclKind, AclTarget, Entity, project_number, validate_role},
    config::CompatProfile,
};
use hyper::{Method, StatusCode, Uri};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AclOperation {
    List,
    Get(String),
    Insert,
    Patch(String),
    Update(String),
    Delete(String),
}

#[derive(Clone, Debug)]
pub struct AclRequest {
    pub target: AclTarget,
    pub operation: AclOperation,
    /// Bucket metageneration conditions; Discovery defines them only for default object ACL lists.
    pub if_metageneration_match: Option<i64>,
    pub if_metageneration_not_match: Option<i64>,
    pub fields: Fields,
}

/// Parse an ACL route. Other paths and methods return `None` so they keep their existing routing.
pub fn route_acl(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<AclRequest, JsonError>> {
    let path = uri.path().strip_prefix("/storage/v1/b/")?;
    let segments: Vec<&str> = path.split('/').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let (kind, object, entity) = match segments.as_slice() {
        [_, "acl"] => (AclKind::Bucket, None, None),
        [_, "acl", entity] => (AclKind::Bucket, None, Some(*entity)),
        [_, "defaultObjectAcl"] => (AclKind::DefaultObject, None, None),
        [_, "defaultObjectAcl", entity] => (AclKind::DefaultObject, None, Some(*entity)),
        [_, "o", object, "acl"] => (AclKind::Object, Some(*object), None),
        [_, "o", object, "acl", entity] => (AclKind::Object, Some(*object), Some(*entity)),
        _ => return None,
    };
    let operation = match (method, entity) {
        (&Method::GET, None) => AclOperation::List,
        (&Method::POST, None) => AclOperation::Insert,
        (&Method::GET, Some(_)) => AclOperation::Get(String::new()),
        (&Method::PATCH, Some(_)) => AclOperation::Patch(String::new()),
        (&Method::PUT, Some(_)) => AclOperation::Update(String::new()),
        (&Method::DELETE, Some(_)) => AclOperation::Delete(String::new()),
        _ => return None,
    };
    Some(parse_route(
        uri,
        segments[0],
        kind,
        object,
        entity,
        operation,
        profile,
    ))
}

fn parse_route(
    uri: &Uri,
    bucket: &str,
    kind: AclKind,
    object: Option<&str>,
    entity: Option<&str>,
    operation: AclOperation,
    profile: CompatProfile,
) -> Result<AclRequest, JsonError> {
    let bucket = decode(bucket, false)?;
    validate_bucket_name_for_profile(&bucket, profile)?;
    let entity = entity.map(|entity| decode(entity, false)).transpose()?;
    let operation = match (operation, entity) {
        (AclOperation::Get(_), Some(entity)) => AclOperation::Get(entity),
        (AclOperation::Patch(_), Some(entity)) => AclOperation::Patch(entity),
        (AclOperation::Update(_), Some(entity)) => AclOperation::Update(entity),
        (AclOperation::Delete(_), Some(entity)) => AclOperation::Delete(entity),
        (operation, _) => operation,
    };
    let mut query = parse_query(uri.query().unwrap_or_default())?;
    let schema = match (kind, &operation) {
        (AclKind::Bucket, AclOperation::List) => "BucketAccessControls",
        (AclKind::Bucket, _) => "BucketAccessControl",
        (_, AclOperation::List) => "ObjectAccessControls",
        _ => "ObjectAccessControl",
    };
    let fields = Fields::take(&mut query, schema)?;
    common(&mut query)?;
    let mut integer = |key: &str| {
        query
            .remove(key)
            .map(|value| {
                value
                    .parse::<i64>()
                    .ok()
                    .filter(|_| !value.starts_with('+'))
                    .ok_or_else(|| JsonError::invalid_parameter(format!("{key} must be an int64")))
            })
            .transpose()
    };
    let (if_metageneration_match, if_metageneration_not_match) =
        if kind == AclKind::DefaultObject && operation == AclOperation::List {
            (
                integer("ifMetagenerationMatch")?,
                integer("ifMetagenerationNotMatch")?,
            )
        } else {
            (None, None)
        };
    let target = match object {
        None if kind == AclKind::Bucket => AclTarget::Bucket(bucket),
        None => AclTarget::DefaultObject(bucket),
        Some(object) => {
            let name = decode(object, false)?;
            validate_object_name(&name)?;
            AclTarget::Object {
                bucket,
                name,
                generation: integer("generation")?,
            }
        }
    };
    finish(query)?;
    Ok(AclRequest {
        target,
        operation,
        if_metageneration_match,
        if_metageneration_not_match,
        fields,
    })
}

/// The entity and role a mutation applies; `role` is `None` only for a patch that omits it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclBody {
    pub entity: String,
    pub role: Option<String>,
}

pub(crate) fn parse_entries(value: &Value, kind: AclKind) -> Result<Vec<AclEntry>, JsonError> {
    let entries: Vec<AclEntry> = serde_json::from_value(value.clone()).map_err(|_| {
        JsonError::invalid_argument("ACL must be an array of objects with entity and role strings")
    })?;
    crate::acl::validate_entries(kind, &entries)
        .map_err(|error| JsonError::invalid_argument(error.to_string()))?;
    Ok(entries)
}

/// Validate an insert, patch or update body against the path entity and resource kind.
///
/// Output-only resource fields and unknown fields are ignored, as JSON API resources do.
pub fn parse_acl_body(
    bytes: &[u8],
    kind: AclKind,
    operation: &AclOperation,
) -> Result<AclBody, JsonError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid JSON access control resource",
        )
    })?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("Access control must be a JSON object"))?;
    let string = |field: &str| {
        body.get(field)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| JsonError::invalid_argument(format!("{field} must be a string")))
            })
            .transpose()
    };
    let required = |field: &str| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "required",
            format!(
                "Required field {field} is missing; for example, send {{\"entity\":\"allUsers\",\"role\":\"READER\"}}"
            ),
        )
    };
    let body_entity = string("entity")?;
    let role = string("role")?;
    let entity = match operation {
        AclOperation::Insert => body_entity.ok_or_else(|| required("entity"))?,
        AclOperation::Patch(path) | AclOperation::Update(path) => {
            if body_entity.as_ref().is_some_and(|entity| entity != path) {
                return Err(JsonError::invalid_argument(format!(
                    "Body entity must match the path entity {path}; omit entity or send {path}"
                )));
            }
            path.clone()
        }
        _ => unreachable!("only mutations carry a body"),
    };
    Entity::parse(&entity).map_err(|error| JsonError::invalid_argument(error.to_string()))?;
    if role.is_none() && !matches!(operation, AclOperation::Patch(_)) {
        return Err(required("role"));
    }
    if let Some(role) = &role {
        validate_role(kind, role)
            .map_err(|error| JsonError::invalid_argument(error.to_string()))?;
    }
    Ok(AclBody { entity, role })
}

/// The protobuf-encoded version that the service uses for ACL etags: generation (field 1, objects
/// only) then metageneration.
fn etag(generation: Option<i64>, metageneration: i64) -> String {
    fn varint(bytes: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            bytes.push((value as u8) | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
    }
    let mut bytes = Vec::new();
    match generation {
        Some(generation) => {
            bytes.push(0x08);
            varint(&mut bytes, generation as u64);
            bytes.push(0x10);
        }
        None => bytes.push(0x08),
    }
    varint(&mut bytes, metageneration as u64);
    base64(&bytes)
}

fn entity_fields(resource: &mut Map<String, Value>, entity: &str) {
    match Entity::parse(entity) {
        Ok(Entity::UserEmail(email) | Entity::GroupEmail(email)) => {
            resource.insert("email".into(), email.into());
        }
        Ok(Entity::UserId(id) | Entity::GroupId(id)) => {
            resource.insert("entityId".into(), id.into());
        }
        Ok(Entity::Domain(domain)) => {
            resource.insert("domain".into(), domain.into());
        }
        Ok(Entity::ProjectTeam { team, project }) => {
            resource.insert(
                "projectTeam".into(),
                json!({"projectNumber": project, "team": team}),
            );
        }
        Ok(Entity::AllUsers | Entity::AllAuthenticatedUsers) | Err(_) => {}
    }
}

/// The resource that owns rendered entries, with the version their etags carry.
#[derive(Clone, Copy, Debug)]
pub struct AclOwner<'a> {
    pub kind: AclKind,
    pub bucket: &'a str,
    /// Object name and generation for object ACLs.
    pub object: Option<(&'a str, i64)>,
    pub metageneration: i64,
}

pub fn acl_entry_resource(base_url: &str, owner: AclOwner<'_>, entry: &AclEntry) -> Value {
    let base = base_url.trim_end_matches('/');
    let mut resource = Map::new();
    let kind = if owner.kind == AclKind::Bucket {
        "storage#bucketAccessControl"
    } else {
        "storage#objectAccessControl"
    };
    resource.insert("kind".into(), kind.into());
    match (owner.kind, owner.object) {
        (AclKind::Bucket, _) => {
            resource.insert(
                "id".into(),
                format!("{}/{}", owner.bucket, entry.entity).into(),
            );
            resource.insert(
                "selfLink".into(),
                format!(
                    "{base}/storage/v1/b/{}/acl/{}",
                    encode_segment(owner.bucket),
                    encode_segment(&entry.entity)
                )
                .into(),
            );
            resource.insert("bucket".into(), owner.bucket.into());
        }
        (AclKind::Object, Some((name, generation))) => {
            resource.insert(
                "id".into(),
                format!("{}/{name}/{generation}/{}", owner.bucket, entry.entity).into(),
            );
            resource.insert(
                "selfLink".into(),
                format!(
                    "{base}/storage/v1/b/{}/o/{}/acl/{}",
                    encode_segment(owner.bucket),
                    encode_segment(name),
                    encode_segment(&entry.entity)
                )
                .into(),
            );
            resource.insert("bucket".into(), owner.bucket.into());
            resource.insert("object".into(), name.into());
            resource.insert("generation".into(), generation.to_string().into());
        }
        // Default object ACL entries describe future objects, so they name no resource.
        _ => {}
    }
    resource.insert("entity".into(), entry.entity.as_str().into());
    resource.insert("role".into(), entry.role.as_str().into());
    entity_fields(&mut resource, &entry.entity);
    resource.insert(
        "etag".into(),
        etag(
            owner.object.map(|(_, generation)| generation),
            owner.metageneration,
        )
        .into(),
    );
    Value::Object(resource)
}

pub fn acl_list_resource(base_url: &str, owner: AclOwner<'_>, entries: &[AclEntry]) -> Value {
    let kind = if owner.kind == AclKind::Bucket {
        "storage#bucketAccessControls"
    } else {
        "storage#objectAccessControls"
    };
    json!({
        "kind": kind,
        "items": entries
            .iter()
            .map(|entry| acl_entry_resource(base_url, owner, entry))
            .collect::<Vec<_>>(),
    })
}

/// Add `acl`, `defaultObjectAcl` and `owner` to a rendered bucket resource.
pub fn add_bucket_acl_fields(
    resource: &mut Value,
    base_url: &str,
    project: &str,
    metageneration: i64,
    acl: &[AclEntry],
    default_object_acl: &[AclEntry],
) {
    let Some(bucket) = resource["name"].as_str().map(str::to_owned) else {
        return;
    };
    let owner = |kind| AclOwner {
        kind,
        bucket: &bucket,
        object: None,
        metageneration,
    };
    let entries = |kind, entries: &[AclEntry]| {
        entries
            .iter()
            .map(|entry| acl_entry_resource(base_url, owner(kind), entry))
            .collect::<Vec<_>>()
    };
    resource["acl"] = entries(AclKind::Bucket, acl).into();
    resource["defaultObjectAcl"] = entries(AclKind::DefaultObject, default_object_acl).into();
    resource["owner"] = json!({"entity": format!("project-owners-{}", project_number(project))});
}

/// Add `acl` to a rendered object resource.
pub fn add_object_acl_fields(resource: &mut Value, base_url: &str, acl: &[AclEntry]) {
    let (Some(bucket), Some(name), Some(generation), Some(metageneration)) = (
        resource["bucket"].as_str().map(str::to_owned),
        resource["name"].as_str().map(str::to_owned),
        resource["generation"]
            .as_str()
            .and_then(|value| value.parse().ok()),
        resource["metageneration"]
            .as_str()
            .and_then(|value| value.parse().ok()),
    ) else {
        return;
    };
    let owner = AclOwner {
        kind: AclKind::Object,
        bucket: &bucket,
        object: Some((&name, generation)),
        metageneration,
    };
    resource["acl"] = acl
        .iter()
        .map(|entry| acl_entry_resource(base_url, owner, entry))
        .collect::<Vec<_>>()
        .into();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(method: Method, path: &str) -> Option<Result<AclRequest, JsonError>> {
        route_acl(&method, &path.parse().unwrap(), CompatProfile::Native)
    }

    #[test]
    fn routes_cover_the_three_collections_and_leave_other_paths() {
        let request = route(Method::PUT, "/storage/v1/b/bucket/acl/allUsers")
            .unwrap()
            .unwrap();
        assert_eq!(request.target, AclTarget::Bucket("bucket".into()));
        assert_eq!(request.operation, AclOperation::Update("allUsers".into()));
        let request = route(
            Method::GET,
            "/storage/v1/b/bucket/defaultObjectAcl?ifMetagenerationMatch=3&ifMetagenerationNotMatch=2",
        )
        .unwrap()
        .unwrap();
        assert_eq!(request.target, AclTarget::DefaultObject("bucket".into()));
        assert_eq!(
            (
                request.if_metageneration_match,
                request.if_metageneration_not_match
            ),
            (Some(3), Some(2))
        );
        let request = route(
            Method::DELETE,
            "/storage/v1/b/bucket/o/dir%2Fname/acl/user-liz%40example.com?generation=7",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            request.target,
            AclTarget::Object {
                bucket: "bucket".into(),
                name: "dir/name".into(),
                generation: Some(7)
            }
        );
        assert_eq!(
            request.operation,
            AclOperation::Delete("user-liz@example.com".into())
        );
        for (method, path) in [
            (Method::POST, "/storage/v1/b/bucket/acl/allUsers"),
            (Method::HEAD, "/storage/v1/b/bucket/acl"),
            (Method::GET, "/storage/v1/b/bucket/o/acl"),
            (Method::GET, "/storage/v1/b/bucket/o/a/b/acl"),
            (Method::GET, "/storage/v1/b/bucket//acl"),
            (Method::GET, "/storage/v1/b/bucket/iam"),
        ] {
            assert!(route(method.clone(), path).is_none(), "{method} {path}");
        }
    }

    #[test]
    fn route_errors_follow_query_and_name_validation() {
        for (path, reason) in [
            ("/storage/v1/b/Bad/acl", "invalidParameter"),
            (
                "/storage/v1/b/bucket/acl?ifMetagenerationMatch=1",
                "invalidParameter",
            ),
            ("/storage/v1/b/bucket/acl?generation=1", "invalidParameter"),
            (
                "/storage/v1/b/bucket/o/x/acl?generation=x",
                "invalidParameter",
            ),
            (
                "/storage/v1/b/bucket/o/x/acl?generation=%2B1",
                "invalidParameter",
            ),
            ("/storage/v1/b/bucket/acl?userProject=p", "badRequest"),
            (
                "/storage/v1/b/bucket/acl?fields=unknown",
                "invalidParameter",
            ),
        ] {
            let error = route(Method::GET, path).unwrap().unwrap_err();
            assert_eq!(error.reason, reason, "{path}");
        }
        assert!(
            route(
                Method::GET,
                "/storage/v1/b/bucket/acl?fields=items(entity,role)"
            )
            .unwrap()
            .is_ok()
        );
    }

    #[test]
    fn bodies_require_entity_and_role_per_operation() {
        let insert = AclOperation::Insert;
        assert_eq!(
            parse_acl_body(
                br#"{"entity":"allUsers","role":"READER","kind":"ignored","etag":"x"}"#,
                AclKind::Bucket,
                &insert
            ),
            Ok(AclBody {
                entity: "allUsers".into(),
                role: Some("READER".into())
            })
        );
        let patch = AclOperation::Patch("allUsers".into());
        assert_eq!(
            parse_acl_body(b"{}", AclKind::Object, &patch),
            Ok(AclBody {
                entity: "allUsers".into(),
                role: None
            })
        );
        let update = AclOperation::Update("allUsers".into());
        for (body, kind, operation, reason) in [
            (&b"[]"[..], AclKind::Bucket, &insert, "invalidArgument"),
            (b"{", AclKind::Bucket, &insert, "parseError"),
            (
                br#"{"role":"READER"}"#,
                AclKind::Bucket,
                &insert,
                "required",
            ),
            (
                br#"{"entity":"allUsers"}"#,
                AclKind::Bucket,
                &insert,
                "required",
            ),
            (b"{}", AclKind::Bucket, &update, "required"),
            (
                br#"{"entity":"allUsers","role":"WRITER"}"#,
                AclKind::Object,
                &insert,
                "invalidArgument",
            ),
            (
                br#"{"entity":"allUsers","role":"WRITER"}"#,
                AclKind::DefaultObject,
                &insert,
                "invalidArgument",
            ),
            (
                br#"{"entity":"nobody","role":"READER"}"#,
                AclKind::Bucket,
                &insert,
                "invalidArgument",
            ),
            (
                br#"{"entity":"allAuthenticatedUsers","role":"READER"}"#,
                AclKind::Bucket,
                &update,
                "invalidArgument",
            ),
            (
                br#"{"entity":"allUsers","role":1}"#,
                AclKind::Bucket,
                &insert,
                "invalidArgument",
            ),
        ] {
            let error = parse_acl_body(body, kind, operation).unwrap_err();
            assert_eq!(error.reason, reason, "{}", String::from_utf8_lossy(body));
        }
        assert!(
            parse_acl_body(
                br#"{"role":"READER"}"#,
                AclKind::Bucket,
                &AclOperation::Update("bad".into())
            )
            .is_err()
        );
    }

    #[test]
    fn resources_follow_the_discovery_schemas() {
        let entry = |entity: &str| AclEntry {
            entity: entity.into(),
            role: "OWNER".into(),
        };
        let bucket = AclOwner {
            kind: AclKind::Bucket,
            bucket: "bucket",
            object: None,
            metageneration: 1,
        };
        assert_eq!(
            acl_entry_resource("http://host/", bucket, &entry("project-owners-123")),
            json!({
                "kind": "storage#bucketAccessControl", "id": "bucket/project-owners-123",
                "selfLink": "http://host/storage/v1/b/bucket/acl/project-owners-123",
                "bucket": "bucket", "entity": "project-owners-123", "role": "OWNER",
                "projectTeam": {"projectNumber": "123", "team": "owners"}, "etag": "CAE="
            })
        );
        let object = AclOwner {
            kind: AclKind::Object,
            bucket: "bucket",
            object: Some(("dir/a b", 1_700_000_000_000_000)),
            metageneration: 2,
        };
        assert_eq!(
            acl_entry_resource("http://host", object, &entry("user-liz@example.com")),
            json!({
                "kind": "storage#objectAccessControl",
                "id": "bucket/dir/a b/1700000000000000/user-liz@example.com",
                "selfLink": "http://host/storage/v1/b/bucket/o/dir%2Fa%20b/acl/user-liz%40example.com",
                "bucket": "bucket", "object": "dir/a b", "generation": "1700000000000000",
                "entity": "user-liz@example.com", "role": "OWNER", "email": "liz@example.com",
                "etag": "CICA+cDBxIIDEAI="
            })
        );
        let default = AclOwner {
            kind: AclKind::DefaultObject,
            bucket: "bucket",
            object: None,
            metageneration: 300,
        };
        assert_eq!(
            acl_list_resource(
                "http://host",
                default,
                &[entry("domain-example.com"), entry("group-00ab")]
            ),
            json!({"kind": "storage#objectAccessControls", "items": [
                {"kind": "storage#objectAccessControl", "entity": "domain-example.com", "role": "OWNER", "domain": "example.com", "etag": "CKwC"},
                {"kind": "storage#objectAccessControl", "entity": "group-00ab", "role": "OWNER", "entityId": "00ab", "etag": "CKwC"}
            ]})
        );
        let mut resource = json!({"name": "bucket"});
        add_bucket_acl_fields(
            &mut resource,
            "http://host",
            "local",
            1,
            &[entry("allUsers")],
            &[],
        );
        assert_eq!(resource["acl"][0]["entity"], "allUsers");
        assert_eq!(resource["defaultObjectAcl"], json!([]));
        assert_eq!(resource["owner"], json!({"entity": "project-owners-0"}));
        let mut resource =
            json!({"bucket": "bucket", "name": "o", "generation": "5", "metageneration": "1"});
        add_object_acl_fields(&mut resource, "http://host", &[entry("allUsers")]);
        assert_eq!(resource["acl"][0]["id"], "bucket/o/5/allUsers");
        assert_eq!(resource["acl"][0]["etag"], "CAUQAQ==");
    }
}
