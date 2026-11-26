//! JSON `bucketAccessControls`, `defaultObjectAccessControls` and `objectAccessControls`
//! routes, request bodies and resources, plus the ACL fields of `projection=full` resources.

use super::{
    ApiError, Fields, Schema,
    buckets::encode_segment,
    integer::take_int64,
    query::{bucket_conditions, common, finish_for_profile},
    validate_object_name,
};
use crate::percent::{Decoding, decode, parse_query};
use crate::{
    acl::{
        AclEntry, AclKind, AclOwner, AclRole, AclTarget, Entity, ProjectTeam, parse_entity,
        validate_role,
    },
    bucket::BucketConditions,
    config::CompatProfile,
    storage::AclProjection,
};
use hyper::{Method, Uri};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// A rejected ACL entity, role or entry list becomes the error body text; the validation
/// error has no underlying cause to keep.
impl From<crate::acl::AclError> for ApiError {
    fn from(error: crate::acl::AclError) -> Self {
        Self::invalid(crate::storage::failure::wording::acl_refusal(&error))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AclOperation {
    List,
    /// The decoded path entity. It is matched against stored entities by exact text and may
    /// name no valid entity, which reads as not found.
    Get(String),
    Delete(String),
    Write(AclWrite),
}

/// A request that carries an access-control body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AclWrite {
    Insert,
    /// The decoded path entity; a body entity must repeat it.
    Patch(String),
    Update(String),
}

/// Whether an ACL request reads or changes the ACL, which selects its IAM permission and the
/// wording of a uniform-access refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AclAccess {
    Read,
    Write,
}

impl AclOperation {
    /// Listing and getting read the ACL; deleting and every write change it.
    pub(crate) fn access(&self) -> AclAccess {
        match self {
            Self::List | Self::Get(_) => AclAccess::Read,
            Self::Delete(_) | Self::Write(_) => AclAccess::Write,
        }
    }

    /// Only a write carries a body.
    pub(crate) fn accepts_body(&self) -> bool {
        match self {
            Self::Write(_) => true,
            Self::List | Self::Get(_) | Self::Delete(_) => false,
        }
    }
}

/// What a method on an ACL path asks for, before the entity segment is percent-decoded.
#[derive(Clone, Copy)]
enum Selection<'a> {
    List,
    Insert,
    Entry(EntryMethod, &'a str),
}

/// A method on one entry of an ACL.
#[derive(Clone, Copy)]
enum EntryMethod {
    Get,
    Patch,
    Update,
    Delete,
}

impl EntryMethod {
    fn operation(self, entity: String) -> AclOperation {
        match self {
            Self::Get => AclOperation::Get(entity),
            Self::Patch => AclOperation::Write(AclWrite::Patch(entity)),
            Self::Update => AclOperation::Write(AclWrite::Update(entity)),
            Self::Delete => AclOperation::Delete(entity),
        }
    }
}

/// The resource an ACL path names, before its segments are decoded.
#[derive(Clone, Copy)]
enum PathTarget<'a> {
    Bucket,
    DefaultObject,
    Object(&'a str),
}

impl PathTarget<'_> {
    fn kind(self) -> AclKind {
        match self {
            Self::Bucket => AclKind::Bucket,
            Self::DefaultObject => AclKind::DefaultObject,
            Self::Object(_) => AclKind::Object,
        }
    }

    /// Whether fake-gcs-server v1.56.1 registers an ACL route for `selection` on this target,
    /// whose handler reads only the query parameters it knows. It serves the list, insert, get,
    /// update and delete of bucket and object ACLs. It has no default object ACL route, and its
    /// object PATCH route takes a PATCH of an object ACL entry as an object name.
    fn served_by_fake_gcs_server(self, selection: Selection<'_>) -> bool {
        match (self, selection) {
            (Self::DefaultObject, _) | (_, Selection::Entry(EntryMethod::Patch, _)) => false,
            (
                Self::Bucket | Self::Object(_),
                Selection::List
                | Selection::Insert
                | Selection::Entry(EntryMethod::Get | EntryMethod::Update | EntryMethod::Delete, _),
            ) => true,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AclRequest {
    pub target: AclTarget,
    pub operation: AclOperation,
    /// Bucket metageneration conditions; Discovery defines them only for default object ACL lists.
    pub conditions: BucketConditions,
    pub fields: Fields,
}

/// Parse an ACL route. Other paths and methods return `None` so they keep their existing routing.
pub(crate) fn route_acl(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<AclRequest, ApiError>> {
    let path = uri.path().strip_prefix(super::query::BUCKET_PATH)?;
    let segments: Vec<&str> = path.split('/').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let (bucket, target, entity) = match segments.as_slice() {
        [bucket, "acl"] => (*bucket, PathTarget::Bucket, None),
        [bucket, "acl", entity] => (*bucket, PathTarget::Bucket, Some(*entity)),
        [bucket, "defaultObjectAcl"] => (*bucket, PathTarget::DefaultObject, None),
        [bucket, "defaultObjectAcl", entity] => (*bucket, PathTarget::DefaultObject, Some(*entity)),
        [bucket, "o", object, "acl"] => (*bucket, PathTarget::Object(object), None),
        [bucket, "o", object, "acl", entity] => {
            (*bucket, PathTarget::Object(object), Some(*entity))
        }
        _ => return None,
    };
    let selection = match (method, entity) {
        (&Method::GET, None) => Selection::List,
        (&Method::POST, None) => Selection::Insert,
        (&Method::GET, Some(entity)) => Selection::Entry(EntryMethod::Get, entity),
        (&Method::PATCH, Some(entity)) => Selection::Entry(EntryMethod::Patch, entity),
        (&Method::PUT, Some(entity)) => Selection::Entry(EntryMethod::Update, entity),
        (&Method::DELETE, Some(entity)) => Selection::Entry(EntryMethod::Delete, entity),
        _ => return None,
    };
    Some(parse_route(uri, bucket, target, selection, profile))
}

fn parse_route(
    uri: &Uri,
    bucket: &str,
    target: PathTarget<'_>,
    selection: Selection<'_>,
    profile: CompatProfile,
) -> Result<AclRequest, ApiError> {
    let bucket = decode(bucket, Decoding::Path)?;
    crate::bucket::name::validate_for_profile(&bucket, profile)?;
    let served_by_fake_gcs_server = target.served_by_fake_gcs_server(selection);
    let operation = match selection {
        Selection::List => AclOperation::List,
        Selection::Insert => AclOperation::Write(AclWrite::Insert),
        Selection::Entry(method, entity) => method.operation(decode(entity, Decoding::Path)?),
    };
    let mut query = parse_query(uri.query().unwrap_or_default())?;
    let kind = target.kind();
    let listing = operation == AclOperation::List;
    let schema = match (kind, listing) {
        (AclKind::Bucket, true) => Schema::BucketAccessControls,
        (AclKind::Bucket, false) => Schema::BucketAccessControl,
        (AclKind::DefaultObject | AclKind::Object, true) => Schema::ObjectAccessControls,
        (AclKind::DefaultObject | AclKind::Object, false) => Schema::ObjectAccessControl,
    };
    let fields = Fields::take(&mut query, schema)?;
    common(&mut query)?;
    let conditions = if kind == AclKind::DefaultObject && listing {
        bucket_conditions(&mut query)?
    } else {
        BucketConditions::default()
    };
    let target = match target {
        PathTarget::Bucket => AclTarget::Bucket(bucket),
        PathTarget::DefaultObject => AclTarget::DefaultObject(bucket),
        PathTarget::Object(object) => {
            let name = decode(object, Decoding::Path)?;
            validate_object_name(&name)?;
            AclTarget::Object {
                bucket,
                name,
                generation: take_int64(&mut query, "generation")?,
            }
        }
    };
    // Cloud Storage ignored an undeclared `gcsmimicUnknown=1` on the object ACL list (case
    // obj-undeclared-query-writes), on the object ACL get, patch, update, delete and insert and on
    // the bucket and default object ACL list and get (case obj-probe-undeclared-query-acls); the
    // bucket and default object ACL writes share this parser. The fake-gcs-server profile follows
    // fake-gcs-server, which ignores one only on the routes it serves.
    if !served_by_fake_gcs_server {
        finish_for_profile(&query, profile)?;
    }
    Ok(AclRequest {
        target,
        operation,
        conditions,
        fields,
    })
}

/// The entity and role a mutation applies; `role` is `None` only for a patch that omits it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AclBody {
    pub entity: Entity,
    pub role: Option<AclRole>,
}

pub(crate) fn parse_entries(value: &Value, kind: AclKind) -> Result<Vec<AclEntry>, ApiError> {
    let input = Vec::<crate::acl::AclEntryInput>::deserialize(value).map_err(|_| {
        ApiError::invalid("ACL must be an array of objects with entity and role strings")
    })?;
    crate::acl::parse_entries(kind, input).map_err(ApiError::from)
}

/// Validate an insert, patch or update body against the path entity and resource kind.
///
/// Output-only resource fields and unknown fields are ignored, as JSON API resources do.
pub(crate) fn parse_acl_body(
    bytes: &[u8],
    kind: AclKind,
    write: &AclWrite,
) -> Result<AclBody, ApiError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| crate::json::parse_error(bytes, &error))?;
    let body = value
        .as_object()
        .ok_or_else(|| ApiError::invalid("Access control must be a JSON object"))?;
    let string = |field: &str| {
        body.get(field)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ApiError::invalid(format!("{field} must be a string")))
            })
            .transpose()
    };
    let required = |field: &str| {
        ApiError::required(format!(
            "Access control must contain {}",
            if field == "role" {
                "a role"
            } else {
                "an entity"
            }
        ))
    };
    let body_entity = string("entity")?;
    let role = string("role")?;
    let entity = match write {
        AclWrite::Insert => body_entity.ok_or_else(|| required("entity"))?,
        AclWrite::Patch(path) | AclWrite::Update(path) => {
            if body_entity.as_ref().is_some_and(|entity| entity != path) {
                return Err(ApiError::invalid(format!(
                    "Body entity must match the path entity {path}; omit entity or send {path}"
                )));
            }
            path.clone()
        }
    };
    let entity = parse_entity(&entity)?;
    if role.is_none() && !matches!(write, AclWrite::Patch(_)) {
        return Err(required("role"));
    }
    let role = role
        .as_deref()
        .map(|role| validate_role(kind, role))
        .transpose()?;
    Ok(AclBody { entity, role })
}

/// The fields Cloud Storage derives from the entity; an entity has at most one of them.
fn entity_fields(resource: &mut Map<String, Value>, entity: &Entity) {
    if let Some(email) = entity.email() {
        resource.insert("email".into(), email.into());
    }
    if let Some(id) = entity.entity_id() {
        resource.insert("entityId".into(), id.into());
    }
    if let Some(domain) = entity.domain() {
        resource.insert("domain".into(), domain.into());
    }
    if let Some((team, project)) = entity.team() {
        resource.insert(
            "projectTeam".into(),
            json!({"projectNumber": project, "team": team.as_str()}),
        );
    }
}

/// The JSON resource kinds of the entries an owner holds.
impl AclOwner<'_> {
    fn resource_kind(self) -> &'static str {
        match self {
            Self::Bucket { .. } => "storage#bucketAccessControl",
            Self::DefaultObject { .. } | Self::Object { .. } => "storage#objectAccessControl",
        }
    }

    fn list_kind(self) -> &'static str {
        match self {
            Self::Bucket { .. } => "storage#bucketAccessControls",
            Self::DefaultObject { .. } | Self::Object { .. } => "storage#objectAccessControls",
        }
    }
}

pub(crate) fn acl_entry_resource(base_url: &str, owner: AclOwner<'_>, entry: &AclEntry) -> Value {
    let base = base_url.trim_end_matches('/');
    let entity_segment = || encode_segment(entry.entity.as_str()).replace("%40", "@");
    let mut resource = Map::new();
    resource.insert("kind".into(), owner.resource_kind().into());
    if let Some(id) = owner.entry_id(&entry.entity) {
        resource.insert("id".into(), id.into());
    }
    match owner {
        AclOwner::Bucket { bucket, .. } => {
            resource.insert(
                "selfLink".into(),
                format!(
                    "{base}/storage/v1/b/{}/acl/{}",
                    encode_segment(bucket),
                    entity_segment()
                )
                .into(),
            );
            resource.insert("bucket".into(), bucket.into());
        }
        AclOwner::Object {
            bucket,
            name,
            generation,
            ..
        } => {
            resource.insert(
                "selfLink".into(),
                format!(
                    "{base}/storage/v1/b/{}/o/{}/acl/{}",
                    encode_segment(bucket),
                    encode_segment(name),
                    entity_segment()
                )
                .into(),
            );
            resource.insert("bucket".into(), bucket.into());
            resource.insert("object".into(), name.into());
            resource.insert("generation".into(), generation.to_string().into());
        }
        // Default object ACL entries describe future objects, so they name no resource.
        AclOwner::DefaultObject { .. } => {}
    }
    resource.insert("entity".into(), entry.entity.as_str().into());
    resource.insert("role".into(), entry.role.as_str().into());
    entity_fields(&mut resource, &entry.entity);
    resource.insert("etag".into(), owner.etag().into());
    Value::Object(resource)
}

pub(crate) fn acl_list_resource(
    base_url: &str,
    owner: AclOwner<'_>,
    entries: &[AclEntry],
) -> Value {
    Value::Object(Map::from_iter([
        ("kind".into(), owner.list_kind().into()),
        (
            "items".into(),
            acl_entry_resources(base_url, owner, entries),
        ),
    ]))
}

fn acl_entry_resources(base_url: &str, owner: AclOwner<'_>, entries: &[AclEntry]) -> Value {
    entries
        .iter()
        .map(|entry| acl_entry_resource(base_url, owner, entry))
        .collect::<Vec<_>>()
        .into()
}

/// Add `acl`, `defaultObjectAcl` and `owner` to a rendered bucket resource. `acl.project`,
/// `acl.bucket` and `acl.default_object` supply the owner and both lists; the metageneration is
/// the rendered resource's, since a later ACL change has its own etag.
pub(crate) fn add_bucket_acl_fields(
    resource: &mut Map<String, Value>,
    base_url: &str,
    bucket: &str,
    metageneration: i64,
    acl: &AclProjection,
) {
    let bucket_owner = AclOwner::Bucket {
        bucket,
        metageneration,
    };
    let default_owner = AclOwner::DefaultObject { metageneration };
    resource.insert(
        "acl".into(),
        acl_entry_resources(base_url, bucket_owner, &acl.bucket),
    );
    resource.insert(
        "defaultObjectAcl".into(),
        acl_entry_resources(base_url, default_owner, &acl.default_object),
    );
    resource.insert(
        "owner".into(),
        json!({"entity": Entity::project_team(ProjectTeam::Owners, &acl.project)}),
    );
}

/// Add `acl` to a rendered object resource; `owner` is the object generation it renders.
pub(crate) fn add_object_acl_fields(
    resource: &mut Map<String, Value>,
    base_url: &str,
    owner: AclOwner<'_>,
    acl: &[AclEntry],
) {
    resource.insert("acl".into(), acl_entry_resources(base_url, owner, acl));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_role_errors_keep_validation_order_and_text() {
        for (kind, input, expected) in [
            (
                AclKind::Bucket,
                r#"[{"entity":"allUsers","role":"reader"}]"#,
                "Invalid value for: reader is not a valid value",
            ),
            (
                AclKind::Object,
                r#"[{"entity":"allUsers","role":"WRITER"}]"#,
                "Invalid value for: WRITER is not a valid value",
            ),
            (
                AclKind::DefaultObject,
                r#"[{"entity":"allUsers","role":"WRITER"}]"#,
                "Invalid value for: WRITER is not a valid value",
            ),
            (
                AclKind::Bucket,
                r#"[{"entity":"bad","role":"bad"}]"#,
                "Scope text \"bad\" is not valid.",
            ),
            (
                AclKind::Bucket,
                r#"[{"entity":"allUsers","role":1}]"#,
                "ACL must be an array of objects with entity and role strings",
            ),
        ] {
            let error = parse_entries(&serde_json::from_str(input).unwrap(), kind).unwrap_err();
            assert_eq!(error.status, hyper::StatusCode::BAD_REQUEST, "{input}");
            assert_eq!(error.message, expected, "{input}");
        }
    }

    fn route(method: Method, path: &str) -> Option<Result<AclRequest, ApiError>> {
        route_acl(&method, &path.parse().unwrap(), CompatProfile::Native)
    }

    #[test]
    fn routes_cover_the_three_collections_and_leave_other_paths() {
        let request = route(Method::PUT, "/storage/v1/b/bucket/acl/allUsers")
            .unwrap()
            .unwrap();
        assert_eq!(request.target, AclTarget::Bucket("bucket".into()));
        assert_eq!(
            request.operation,
            AclOperation::Write(AclWrite::Update("allUsers".into()))
        );
        let request = route(
            Method::GET,
            "/storage/v1/b/bucket/defaultObjectAcl?ifMetagenerationMatch=3&ifMetagenerationNotMatch=2",
        )
        .unwrap()
        .unwrap();
        assert_eq!(request.target, AclTarget::DefaultObject("bucket".into()));
        assert_eq!(
            request.conditions,
            BucketConditions {
                if_metageneration_match: Some(3),
                if_metageneration_not_match: Some(2),
            }
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
        // A get matches the decoded path text against stored entities, so one of no valid form
        // is routed as it is and later reads as not found.
        let request = route(Method::GET, "/storage/v1/b/bucket/acl/not%20an%20entity")
            .unwrap()
            .unwrap();
        assert_eq!(request.operation, AclOperation::Get("not an entity".into()));
        assert!(!request.operation.accepts_body());
        assert_eq!(request.operation.access(), AclAccess::Read);
        let request = route(Method::POST, "/storage/v1/b/bucket/o/x/acl")
            .unwrap()
            .unwrap();
        assert_eq!(request.operation, AclOperation::Write(AclWrite::Insert));
        assert!(request.operation.accepts_body());
        assert_eq!(request.operation.access(), AclAccess::Write);
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
                "/storage/v1/b/bucket/o/x/acl?generation=x",
                "invalidParameter",
            ),
            (
                "/storage/v1/b/bucket/o/x/acl?generation=%2B1",
                "invalidParameter",
            ),
            ("/storage/v1/b/bucket/acl?callback=p", "badRequest"),
            (
                "/storage/v1/b/bucket/acl?fields=unknown",
                "invalidParameter",
            ),
        ] {
            let error = route(Method::GET, path).unwrap().unwrap_err();
            assert_eq!(error.reason, reason, "{path}");
        }
        // The Requester Pays billing project is ignored, as on every JSON method. A condition or
        // generation the bucket ACL list does not declare is an undeclared parameter, which
        // Cloud Storage ignored on every ACL method (case obj-probe-undeclared-query-acls).
        for path in [
            "/storage/v1/b/bucket/acl?userProject=p",
            "/storage/v1/b/bucket/acl?ifMetagenerationMatch=1",
            "/storage/v1/b/bucket/acl?generation=1",
        ] {
            let request = route(Method::GET, path).unwrap().unwrap();
            assert_eq!(request.conditions, BucketConditions::default(), "{path}");
            assert_eq!(request.target, AclTarget::Bucket("bucket".into()), "{path}");
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

    /// Cloud Storage ignored an undeclared parameter on every recorded ACL method (cases
    /// obj-undeclared-query-writes and obj-probe-undeclared-query-acls). fake-gcs-server v1.56.1
    /// has no default object ACL route and no ACL PATCH route, so its profile rejects one there.
    #[test]
    fn undeclared_parameters_are_ignored_where_each_profile_serves_the_method() {
        for collection in [
            "/storage/v1/b/bucket/acl",
            "/storage/v1/b/bucket/defaultObjectAcl",
            "/storage/v1/b/bucket/o/x/acl",
        ] {
            for (method, entity) in [
                (Method::GET, ""),
                (Method::POST, ""),
                (Method::GET, "/allUsers"),
                (Method::PUT, "/allUsers"),
                (Method::DELETE, "/allUsers"),
                (Method::PATCH, "/allUsers"),
            ] {
                let uri = format!("{collection}{entity}?gcsmimicUnknown=1")
                    .parse()
                    .unwrap();
                let native = route_acl(&method, &uri, CompatProfile::Native).unwrap();
                assert!(native.is_ok(), "{method} {uri}");
                let fake = route_acl(&method, &uri, CompatProfile::FakeGcsServer).unwrap();
                if collection.ends_with("/defaultObjectAcl") || method == Method::PATCH {
                    let error = fake.unwrap_err();
                    assert_eq!(error.reason, "invalidParameter", "{method} {uri}");
                    assert_eq!(
                        error.message, "Unknown query parameter for this method: gcsmimicUnknown",
                        "{method} {uri}"
                    );
                } else {
                    assert!(fake.is_ok(), "{method} {uri}");
                }
            }
        }
        // A recognized parameter gcs-mimic does not implement is still refused in both profiles.
        let uri = "/storage/v1/b/bucket/acl?gcsmimicUnknown=1&callback=p"
            .parse()
            .unwrap();
        for profile in [CompatProfile::Native, CompatProfile::FakeGcsServer] {
            let error = route_acl(&Method::GET, &uri, profile).unwrap().unwrap_err();
            assert_eq!(error.reason, "badRequest", "{profile:?}");
        }
    }

    #[test]
    fn bodies_require_entity_and_role_per_operation() {
        let insert = AclWrite::Insert;
        assert_eq!(
            parse_acl_body(
                br#"{"entity":"allUsers","role":"READER","kind":"ignored","etag":"x"}"#,
                AclKind::Bucket,
                &insert
            ),
            Ok(AclBody {
                entity: "allUsers".parse().unwrap(),
                role: Some(crate::acl::AclRole::Reader)
            })
        );
        let patch = AclWrite::Patch("allUsers".into());
        assert_eq!(
            parse_acl_body(b"{}", AclKind::Object, &patch),
            Ok(AclBody {
                entity: "allUsers".parse().unwrap(),
                role: None
            })
        );
        let update = AclWrite::Update("allUsers".into());
        for (body, kind, operation, reason) in [
            (&b"[]"[..], AclKind::Bucket, &insert, "invalid"),
            (b"{", AclKind::Bucket, &insert, "invalid"),
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
                "invalid",
            ),
            (
                br#"{"entity":"allUsers","role":"WRITER"}"#,
                AclKind::DefaultObject,
                &insert,
                "invalid",
            ),
            (
                br#"{"entity":"nobody","role":"READER"}"#,
                AclKind::Bucket,
                &insert,
                "invalid",
            ),
            (
                br#"{"entity":"allAuthenticatedUsers","role":"READER"}"#,
                AclKind::Bucket,
                &update,
                "invalid",
            ),
            (
                br#"{"entity":"allUsers","role":1}"#,
                AclKind::Bucket,
                &insert,
                "invalid",
            ),
        ] {
            let error = parse_acl_body(body, kind, operation).unwrap_err();
            assert_eq!(error.reason, reason, "{}", String::from_utf8_lossy(body));
        }
        assert!(
            parse_acl_body(
                br#"{"role":"READER"}"#,
                AclKind::Bucket,
                &AclWrite::Update("bad".into())
            )
            .is_err()
        );
    }

    #[test]
    fn resources_follow_the_discovery_schemas() {
        let entry = |entity: &str| AclEntry {
            entity: entity.parse().unwrap(),
            role: crate::acl::AclRole::Owner,
        };
        let bucket = AclOwner::Bucket {
            bucket: "bucket",
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
        let object = AclOwner::Object {
            bucket: "bucket",
            name: "dir/a b",
            generation: 1_700_000_000_000_000,
            metageneration: 2,
        };
        assert_eq!(
            acl_entry_resource("http://host", object, &entry("user-liz@example.com")),
            json!({
                "kind": "storage#objectAccessControl",
                "id": "bucket/dir/a b/1700000000000000/user-liz@example.com",
                "selfLink": "http://host/storage/v1/b/bucket/o/dir%2Fa%20b/acl/user-liz@example.com",
                "bucket": "bucket", "object": "dir/a b", "generation": "1700000000000000",
                "entity": "user-liz@example.com", "role": "OWNER", "email": "liz@example.com",
                "etag": "CICA+cDBxIIDEAI="
            })
        );
        let default = AclOwner::DefaultObject {
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
        let mut resource = Map::from_iter([("name".into(), "bucket".into())]);
        let projection = AclProjection {
            uniform_access: false,
            project: "local".into(),
            bucket: vec![entry("allUsers")],
            default_object: Vec::new(),
            bucket_metageneration: 7,
            objects: Vec::new(),
        };
        add_bucket_acl_fields(&mut resource, "http://host", "bucket", 1, &projection);
        assert_eq!(resource["acl"][0]["entity"], "allUsers");
        assert_eq!(resource["acl"][0]["id"], "bucket/allUsers");
        // The rendered metageneration, not the stored one, signs the entries.
        assert_eq!(resource["acl"][0]["etag"], "CAE=");
        assert_eq!(resource["defaultObjectAcl"], json!([]));
        assert_eq!(
            resource["owner"],
            json!({"entity": "project-owners-902648055030"})
        );
        let mut resource = Map::new();
        let object = AclOwner::Object {
            bucket: "bucket",
            name: "o",
            generation: 5,
            metageneration: 1,
        };
        add_object_acl_fields(&mut resource, "http://host", object, &[entry("allUsers")]);
        assert_eq!(resource["acl"][0]["id"], "bucket/o/5/allUsers");
        assert_eq!(resource["acl"][0]["etag"], "CAUQAQ==");
    }
}
